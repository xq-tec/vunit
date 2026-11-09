// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests ported from `tests/unit/test_test_suites.py`, adapted to one test per simulation, and
//! tests of the simulation plan.
//!
//! AI NOTICE: Generated, minimally reviewed.

use serde_json::Value;
use serde_json::json;
use tokio::sync::Semaphore;

use super::*;
use crate::discovery;
use crate::simulator::RUN_OPTIONS_FORMAT;
use crate::spec::ConfigurationSpec;
use crate::spec::TestConfigSpec;
use crate::test_support::add_vhdl;
use crate::test_support::simulator;

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
        let requests: Vec<SimulationRequest> = requests
            .iter()
            .map(|&(pattern, gui)| SimulationRequest {
                pattern: pattern.to_owned(),
                gui,
                ..SimulationRequest::default()
            })
            .collect();
        self.plan_requests(&requests)
    }

    fn plan_requests(&self, requests: &[SimulationRequest]) -> SimulationPlan {
        let discovery = discovery::discover(&self.project, &self.test_configs);
        assert_eq!(discovery.project_diagnostics, []);
        assert_eq!(discovery.config_diagnostics, []);
        SimulationPlan::new(
            &SimulationInput {
                layout: &self.layout,
                project: &self.project,
                discovery: &discovery,
                sim_options: &self.sim_options,
            },
            requests,
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

/// The run options of the command line of `test` with `seed`, without defaults of the runtime.
fn run_options(test: &PlannedTest, seed: &str) -> Value {
    let command = test
        .command(&simulator(), seed, &RunOptions::new())
        .unwrap();
    assert_eq!(command[..2], ["/bin/risim-runner", "run"]);
    assert_eq!(command.len(), 3, "{command:?}");
    serde_json::from_str(&command[2]).unwrap()
}

/// The value of the generic `name` in the run options `options`.
fn generic<'options>(options: &'options Value, name: &str) -> Option<&'options str> {
    let prefix = format!("{name}=");
    options["generics"]
        .as_array()?
        .iter()
        .find_map(|generic| generic.as_str()?.strip_prefix(prefix.as_str()))
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
    fixture.sim_options.run_options =
        Some(RunOptions::from_iter([("relaxed".to_owned(), true.into())]));
    let plan = fixture.plan(&[("*", true)]);
    let test = &plan.tests[0];
    assert_eq!(test.name, "Lib.Tb_A.Test, 1");
    assert_eq!(
        test.paths.dir,
        fixture.layout.test_output_dir("Lib.Tb_A.Test, 1")
    );
    let mut options = run_options(test, "abc");
    let runner_cfg = generic(&options, "runner_cfg").unwrap().to_owned();
    options.as_object_mut().unwrap().remove("generics");
    assert_eq!(
        options,
        json!({
            "format": RUN_OPTIONS_FORMAT,
            "work": "Lib",
            "workdir": "/ws/risim-out/libraries/lib",
            "library-paths": ["/ws/risim-out/libraries/lib", "/ws/risim-out/libraries/other"],
            "relaxed": true,
            "top": "Tb_A(a)",
            "assert-level": "error",
            "ieee-asserts": "enable",
            "paused": true,
            "name": "Lib.Tb_A.Test, 1",
        })
    );
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
    let options = run_options(test, "1");
    let expected = directory_generic(&test.paths.dir);
    assert_eq!(generic(&options, "output_path"), Some(expected.as_str()));
    assert_eq!(generic(&options, "tb_path"), Some("/ws/"));
    assert_eq!(options["paused"], false);

    fixture.test_configs.push(TestConfigSpec {
        target: "lib.tb_out".to_owned(),
        generics: BTreeMap::from([("Output_Path".to_owned(), "/custom/".to_owned())]),
        ..TestConfigSpec::default()
    });
    let overridden = run_options(&fixture.plan(&[("*", false)]).tests[0], "1");
    assert_eq!(generic(&overridden, "Output_Path"), Some("/custom/"));
    assert_eq!(generic(&overridden, "output_path"), None);
}

#[test]
fn configurations_set_options_seed_and_vhdl_configuration() {
    let mut fixture = Fixture::new();
    fixture.add(
        "/ws/tb_cfg.vhd",
        &testbench("tb_cfg", "; value : integer", &["t"]),
    );
    fixture.sim_options = SimOptions {
        run_options: Some(RunOptions::from_iter([(
            "finish-after".to_owned(),
            "1 ms".into(),
        )])),
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
    let vhdl_cfg_options = run_options(vhdl_cfg, "s");
    assert_eq!(vhdl_cfg_options["top"], "cfg1");
    assert_eq!(vhdl_cfg_options["finish-after"], "1 ms");
    assert_eq!(vhdl_cfg_options["assert-level"], "error");
    assert_eq!(vhdl_cfg_options["ieee-asserts"], "disable");

    let warn = &plan.tests[1];
    assert_eq!(warn.seed(), "config-seed");
    let warn_options = run_options(warn, "s");
    assert_eq!(warn_options["top"], "tb_cfg(a)");
    assert_eq!(generic(&warn_options, "value"), Some("7"));
    assert_eq!(warn_options["assert-level"], "warning");
}

/// The run options of a configuration override the defaults of the runtime by the layer rule of
/// `overlay_run_options`, and can't set a key of the simulation.
#[test]
fn run_options_override_the_defaults() {
    let mut fixture = Fixture::new();
    fixture.add("/ws/tb_a.vhd", &testbench("tb_a", "", &[]));
    fixture.sim_options.run_options = Some(RunOptions::from_iter([
        ("jit".to_owned(), "eager".into()),
        ("opt.wake-guards".to_owned(), "on".into()),
    ]));
    let run_defaults = RunOptions::from_iter([
        ("jit".to_owned(), "off".into()),
        ("opt-level".to_owned(), "off".into()),
        ("opt.wake-guards".to_owned(), "off".into()),
    ]);
    let plan = fixture.plan(&[("*", false)]);
    let command = plan.tests[0]
        .command(&simulator(), "1", &run_defaults)
        .unwrap();
    let options: Value = serde_json::from_str(&command[2]).unwrap();
    assert_eq!(options["jit"], "eager");
    assert_eq!(options["opt-level"], "off");
    assert_eq!(options["opt.wake-guards"], "on");

    fixture.sim_options.run_options =
        Some(RunOptions::from_iter([("paused".to_owned(), true.into())]));
    let paused = fixture.plan(&[("*", false)]);
    assert_eq!(
        paused.tests[0].command(&simulator(), "1", &run_defaults),
        Err(CommandError::SimulationKey { key: "paused" })
    );
}

/// The run options of a request go over those of the configuration by the layer rule, and a
/// testcase that requests with different options match runs with those of the first of them.
#[test]
fn request_options_override_the_configuration() {
    let mut fixture = Fixture::new();
    fixture.add("/ws/tb_a.vhd", &testbench("tb_a", "", &["t1", "t2"]));
    fixture.sim_options.run_options = Some(RunOptions::from_iter([
        ("relaxed".to_owned(), true.into()),
        ("opt.wake-guards".to_owned(), "off".into()),
    ]));
    let request = |pattern: &str, options: &[(&str, &str)]| SimulationRequest {
        pattern: pattern.to_owned(),
        gui: false,
        options: options
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.into()))
            .collect(),
    };
    let plan = fixture.plan_requests(&[
        request("lib.tb_a.t1", &[("opt-level", "off")]),
        request("*", &[("opt.mir", "off")]),
    ]);
    let [t1, t2] = [&plan.tests[0], &plan.tests[1]].map(|test| run_options(test, "1"));
    assert_eq!(t1["relaxed"], true);
    assert_eq!(t1["opt-level"], "off");
    // The preset of the request drops the configuration's rows.
    assert_eq!(t1.get("opt.wake-guards"), None);
    assert_eq!(t1.get("opt.mir"), None);
    assert_eq!(t2.get("opt-level"), None);
    assert_eq!(t2["opt.wake-guards"], "off");
    assert_eq!(t2["opt.mir"], "off");
    assert_eq!(
        plan.diagnostics,
        [Diagnostic::warning(
            "requests with different run options match 'Lib.tb_a.t1'; it runs once, with those of \
             'lib.tb_a.t1'"
        )]
    );
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
    assert_eq!(
        plan.tests[0].command(&simulator(), "1", &RunOptions::new()),
        Err(CommandError::UnsupportedStandard {
            standard: VhdlStandard::Vhdl2019
        })
    );
}

#[test]
fn gui_requests_start_risim_paused() {
    let mut fixture = Fixture::new();
    fixture.add("/ws/tb_a.vhd", &testbench("tb_a", "", &["t1", "t2"]));
    let plan = fixture.plan(&[("lib.tb_a.t1", true), ("lib.tb_a.t2", false)]);
    assert_eq!(plan.diagnostics, []);
    let paused: Vec<Value> = plan
        .tests
        .iter()
        .map(|test| run_options(test, "1")["paused"].clone())
        .collect();
    assert_eq!(paused, [true, false]);
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
        run_defaults: Arc::default(),
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
