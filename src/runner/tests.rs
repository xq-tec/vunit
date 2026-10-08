// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests ported from `tests/unit/test_test_suites.py`, adapted to one test per simulation, and
//! tests of the simulation plan.
//!
//! AI NOTICE: Generated, minimally reviewed.

use tokio::sync::Semaphore;

use super::*;
use crate::discovery;
use crate::spec::ConfigurationSpec;
use crate::spec::TestConfigSpec;
use crate::test_support::add_vhdl;
use crate::test_support::simulator;
use crate::test_support::simulator_identity;

// -------------------------------------------------------------------------------------------------
// runner_cfg and seeds
// -------------------------------------------------------------------------------------------------

#[test]
fn encode_dict_sorts_and_escapes() {
    assert_eq!(encode_dict([]), "");
    assert_eq!(
        encode_dict([("b", "x,y"), ("a:b", "c:d")]),
        "a::b : c::d,b : x,,y"
    );
}

#[test]
fn runner_cfg_of_an_explicit_test() {
    let cfg = RunnerCfg {
        test: Some("Test 1, with comma"),
        output_path: Utf8Path::new("/ws/risim-out/test_output/x"),
        seed: "0123456789abcdef",
        tb_path: Utf8Path::new("/ws/tb"),
    };
    assert_eq!(
        cfg.encode(),
        "active python runner : true,\
         enabled_test_cases : Test 1,,,, with comma,\
         output path : /ws/risim-out/test_output/x/,\
         seed : 0123456789abcdef,\
         tb path : /ws/tb/,\
         use_color : false"
    );
}

#[test]
fn runner_cfg_of_an_implicit_test_enables_nothing() {
    let cfg = RunnerCfg {
        test: None,
        output_path: Utf8Path::new(r"C:\ws\out"),
        seed: "1",
        tb_path: Utf8Path::new(r"C:\ws\tb\"),
    };
    let encoded = cfg.encode();
    assert!(encoded.contains("enabled_test_cases : ,"), "{encoded}");
    assert!(encoded.contains("output path : C::/ws/out/,"), "{encoded}");
    assert!(encoded.contains("tb path : C::/ws/tb/,"), "{encoded}");
}

#[test]
fn generated_seeds_are_16_hex_digits_and_differ() {
    let seed = generate_seed();
    assert_eq!(seed.len(), 16);
    assert!(
        seed.chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    );
    assert_ne!(generate_seed(), seed);
}

// -------------------------------------------------------------------------------------------------
// Results
// -------------------------------------------------------------------------------------------------

#[test]
fn missing_results_fail() {
    assert!(!results_show_pass("", Some("test1")));
    assert!(!results_show_pass("", None));
}

#[test]
fn started_test_passes_when_the_suite_is_done() {
    let results = "test_start:test1\ntest_suite_done\n";
    assert!(results_show_pass(results, Some("test1")));
    // CRLF line endings are accepted.
    assert!(results_show_pass(
        "test_start:test1\r\ntest_suite_done\r\n",
        Some("test1")
    ));
}

#[test]
fn started_test_fails_when_the_suite_is_not_done() {
    assert!(!results_show_pass("test_start:test1\n", Some("test1")));
}

#[test]
fn test_that_never_started_fails() {
    // VUnit reports this test as skipped.
    let results = "test_start:test1\ntest_suite_done\n";
    assert!(!results_show_pass(results, Some("test2")));
    // Names are compared exactly.
    assert!(!results_show_pass(results, Some("Test1")));
    assert!(!results_show_pass(results, Some("test")));
}

#[test]
fn implicit_test_passes_when_the_suite_is_done() {
    assert!(results_show_pass("test_suite_done\n", None));
    assert!(!results_show_pass("\n", None));
}

#[test]
fn non_zero_exit_code_fails_from_vhdl_2008() {
    use TestOutcome::Failed;
    use TestOutcome::Passed;

    for (results_pass, exit_success, standard, expected) in [
        (true, true, VhdlStandard::Vhdl2008, Passed),
        (true, false, VhdlStandard::Vhdl2008, Failed),
        (true, false, VhdlStandard::Vhdl2019, Failed),
        (true, false, VhdlStandard::Vhdl2002, Passed),
        (true, false, VhdlStandard::Vhdl1993, Passed),
        (false, true, VhdlStandard::Vhdl2008, Failed),
        (false, true, VhdlStandard::Vhdl1993, Failed),
    ] {
        assert_eq!(
            outcome(results_pass, exit_success, standard),
            expected,
            "{results_pass} {exit_success} {standard:?}"
        );
    }
}

// -------------------------------------------------------------------------------------------------
// Planning
// -------------------------------------------------------------------------------------------------

struct Fixture {
    project: Project,
    layout: OutputLayout,
    sim_options: SimOptions,
    test_configs: Vec<TestConfigSpec>,
}

impl Fixture {
    fn new() -> Self {
        let mut project = Project::new();
        project
            .add_library("Lib", VhdlStandard::Vhdl2008, None)
            .unwrap();
        project
            .add_library("other", VhdlStandard::Vhdl2008, None)
            .unwrap();
        Self {
            project,
            layout: OutputLayout::new(Utf8Path::new("/ws")),
            sim_options: SimOptions::default(),
            test_configs: Vec::new(),
        }
    }

    fn add(&mut self, path: &str, code: &str) {
        add_vhdl(&mut self.project, "Lib", Utf8Path::new(path), code);
    }

    fn plan(&self, requests: &[(&str, bool)]) -> SimulationPlan {
        let discovery = discovery::discover(&self.project, &self.test_configs);
        assert_eq!(discovery.project_diagnostics, []);
        assert_eq!(discovery.config_diagnostics, []);
        let requests: Vec<SimulationRequest> = requests
            .iter()
            .map(|&(pattern, gui)| SimulationRequest {
                pattern: pattern.to_owned(),
                gui,
            })
            .collect();
        SimulationPlan::new(
            &SimulationInput {
                layout: &self.layout,
                project: &self.project,
                discovery: &discovery,
                sim_options: &self.sim_options,
            },
            &requests,
        )
    }
}

/// A testbench with the given extra generics and tests.
fn testbench(name: &str, generics: &str, tests: &[&str]) -> String {
    let body = if tests.is_empty() {
        String::new()
    } else {
        tests.iter().fold(String::new(), |mut body, test| {
            body.push_str("    if run(\"");
            body.push_str(test);
            body.push_str("\") then end if;\n");
            body
        })
    };
    format!(
        "entity {name} is generic (runner_cfg : string{generics}); end entity;\n\
         architecture A of {name} is\nbegin\n  main : process\n  begin\n    \
         test_runner_setup(runner, runner_cfg);\n{body}    test_runner_cleanup(runner);\n  \
         end process;\nend architecture;\n"
    )
}

fn generic_arg<'command>(command: &'command [String], name: &str) -> Option<&'command str> {
    let prefix = format!("-g{name}=");
    command
        .iter()
        .find_map(|arg| arg.strip_prefix(prefix.as_str()))
}

#[test]
fn plan_resolves_patterns_and_merges_gui() {
    let mut fixture = Fixture::new();
    fixture.add("/ws/tb_b.vhd", &testbench("tb_b", "", &[]));
    fixture.add("/ws/tb_a.vhd", &testbench("tb_a", "", &["t1", "t2"]));
    let plan = fixture.plan(&[("lib.tb_a.*", false), ("*.T1", true), ("nothing", false)]);
    let entries: Vec<(&str, bool)> = plan
        .tests
        .iter()
        .map(|test| (test.name.as_str(), test.gui))
        .collect();
    assert_eq!(entries, [("Lib.tb_a.t1", true), ("Lib.tb_a.t2", false)]);
    assert_eq!(plan.testcases(), ["Lib.tb_a.t1", "Lib.tb_a.t2"]);
    assert_eq!(plan.diagnostics.len(), 1);
    assert_eq!(plan.diagnostics[0].message, "no testcase matches 'nothing'");

    let implicit = fixture.plan(&[("lib.tb_b.all", false)]);
    assert_eq!(implicit.testcases(), ["Lib.tb_b.all"]);
}

#[test]
fn command_of_an_explicit_test() {
    let mut fixture = Fixture::new();
    fixture.add("/ws/tb/tb_a.vhd", &testbench("Tb_A", "", &["Test, 1"]));
    fixture.sim_options.elab_flags = Some(vec!["-frelaxed".to_owned()]);
    let plan = fixture.plan(&[("*", true)]);
    let test = &plan.tests[0];
    assert_eq!(test.name, "Lib.Tb_A.Test, 1");
    assert_eq!(
        test.paths.dir,
        fixture.layout.test_output_dir("Lib.Tb_A.Test, 1")
    );
    let command = test.command(&simulator(), "abc").unwrap();
    assert_eq!(
        command[..7],
        [
            "/bin/risim-ghdl",
            "--elab-run",
            "--std=08",
            "--work=Lib",
            "--workdir=/ws/risim-out/libraries/lib",
            "-P/ws/risim-out/libraries/lib",
            "-P/ws/risim-out/libraries/other",
        ]
    );
    assert_eq!(command[7..10], ["-frelaxed", "Tb_A", "a"]);
    assert_eq!(
        command[command.len() - 3..],
        ["--assert-level=error", "--wait", "--name=Lib.Tb_A.Test, 1",]
    );
    let runner_cfg = generic_arg(&command, "runner_cfg").unwrap();
    assert_eq!(
        runner_cfg,
        RunnerCfg {
            test: Some("Test, 1"),
            output_path: &test.paths.dir,
            seed: "abc",
            tb_path: Utf8Path::new("/ws/tb"),
        }
        .encode()
    );
    assert!(runner_cfg.contains("enabled_test_cases : Test,,,, 1,"));
}

#[test]
fn output_path_generic_is_filled_unless_set() {
    let mut fixture = Fixture::new();
    fixture.add(
        "/ws/tb_out.vhd",
        &testbench("tb_out", "; output_path : string; tb_path : string", &[]),
    );
    let plan = fixture.plan(&[("*", false)]);
    let test = &plan.tests[0];
    let command = test.command(&simulator(), "1").unwrap();
    let expected = directory_generic(&test.paths.dir);
    assert_eq!(
        generic_arg(&command, "output_path"),
        Some(expected.as_str())
    );
    assert_eq!(generic_arg(&command, "tb_path"), Some("/ws/"));
    assert!(!command.contains(&"--wait".to_owned()));

    fixture.test_configs.push(TestConfigSpec {
        target: "lib.tb_out".to_owned(),
        generics: BTreeMap::from([("Output_Path".to_owned(), "/custom/".to_owned())]),
        ..TestConfigSpec::default()
    });
    let overridden = fixture.plan(&[("*", false)]).tests[0]
        .command(&simulator(), "1")
        .unwrap();
    assert_eq!(generic_arg(&overridden, "Output_Path"), Some("/custom/"));
    assert_eq!(generic_arg(&overridden, "output_path"), None);
}

#[test]
fn configurations_set_options_seed_and_vhdl_configuration() {
    let mut fixture = Fixture::new();
    fixture.add(
        "/ws/tb_cfg.vhd",
        &testbench("tb_cfg", "; value : integer", &["t"]),
    );
    fixture.sim_options = SimOptions {
        sim_flags: Some(vec!["--stop-time=1ms".to_owned()]),
        disable_ieee_warnings: Some(true),
        seed: Some("project-seed".to_owned()),
        ..SimOptions::default()
    };
    fixture.test_configs.push(TestConfigSpec {
        target: "lib.tb_cfg".to_owned(),
        configurations: vec![
            ConfigurationSpec {
                name: "warn".to_owned(),
                generics: BTreeMap::from([("value".to_owned(), "7".to_owned())]),
                sim_options: SimOptions {
                    vhdl_assert_stop_level: Some(AssertLevel::Warning),
                    seed: Some("config-seed".to_owned()),
                    ..SimOptions::default()
                },
                ..ConfigurationSpec::default()
            },
            ConfigurationSpec {
                name: "vhdl_cfg".to_owned(),
                vhdl_configuration_name: Some("cfg1".to_owned()),
                ..ConfigurationSpec::default()
            },
        ],
        ..TestConfigSpec::default()
    });
    let plan = fixture.plan(&[("*", false)]);
    assert_eq!(
        plan.testcases(),
        ["Lib.tb_cfg.vhdl_cfg.t", "Lib.tb_cfg.warn.t"]
    );

    let vhdl_cfg = &plan.tests[0];
    assert_eq!(vhdl_cfg.seed(), "project-seed");
    let vhdl_cfg_command = vhdl_cfg.command(&simulator(), "s").unwrap();
    assert_eq!(vhdl_cfg_command[7], "cfg1");
    assert_eq!(vhdl_cfg_command[8], "--stop-time=1ms");
    assert!(vhdl_cfg_command.contains(&"--assert-level=error".to_owned()));
    assert!(vhdl_cfg_command.contains(&"--ieee-asserts=disable".to_owned()));

    let warn = &plan.tests[1];
    assert_eq!(warn.seed(), "config-seed");
    let warn_command = warn.command(&simulator(), "s").unwrap();
    assert_eq!(warn_command[7..9], ["tb_cfg", "a"]);
    assert_eq!(generic_arg(&warn_command, "value"), Some("7"));
    assert!(warn_command.contains(&"--assert-level=warning".to_owned()));
}

#[test]
fn unsupported_standard_fails_the_command() {
    let mut fixture = Fixture::new();
    fixture.project = Project::new();
    fixture
        .project
        .add_library("Lib", VhdlStandard::Vhdl2019, None)
        .unwrap();
    fixture.add("/ws/tb_19.vhd", &testbench("tb_19", "", &[]));
    let plan = fixture.plan(&[("*", false)]);
    let old =
        Simulator::from_identity(simulator_identity("GHDL 5.0.0 [simulation adapter]\n")).unwrap();
    plan.tests[0].command(&old, "1").unwrap_err();
    plan.tests[0].command(&simulator(), "1").unwrap();
}

// -------------------------------------------------------------------------------------------------
// Execution
// -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cancelled_simulation_starts_nothing_despite_free_permits() {
    let mut fixture = Fixture::new();
    fixture.add("/ws/tb_c.vhd", &testbench("tb_c", "", &["t1", "t2", "t3"]));
    let plan = fixture.plan(&[("*", false)]);
    let (events, mut receiver) = mpsc::unbounded_channel();
    let context = SimulationContext {
        workspace_root: "/ws".into(),
        simulator: simulator(),
        limit: ProcessLimit::from(Arc::new(Semaphore::new(8))),
        results: Arc::new(ResultStore::load(&fixture.layout)),
        testcase_locks: Arc::default(),
        events: events.into(),
        cancel: CancellationToken::new(),
    };
    context.cancel.cancel();
    // Nothing is dispatched, so `/ws` is never touched.
    let report = simulate(plan, &context).await;
    assert_eq!(report.counts().cancelled, 3);
    drop(context);
    let mut started = 0;
    while let Some(event) = receiver.recv().await {
        started += usize::from(matches!(event, SimulationEvent::TestStarted { .. }));
    }
    assert_eq!(started, 0);
}
