// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests ported from `tests/unit/test_test_bench.py` and `tests/unit/test_test_bench_list.py`.
//!
//! `VUnit` raises exceptions where discovery reports diagnostics and skips the testbench, and
//! has no `run_all_in_same_sim` suites; those tests are adapted accordingly.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;

use super::*;
use crate::spec::ConfigurationSpec;
use crate::test_support::add_vhdl;

struct TestProject {
    project: Project,
}

impl TestProject {
    fn new() -> Self {
        let mut project = Project::new();
        project
            .add_library("lib", VhdlStandard::Vhdl2008, None)
            .unwrap();
        Self { project }
    }

    fn add(&mut self, path: &str, code: &str) -> FileId {
        add_vhdl(&mut self.project, "lib", Utf8Path::new(path), code)
    }

    fn discover(&self) -> Discovery {
        discover(&self.project, &[])
    }
}

/// A testbench entity `name` with the given generics after `runner_cfg`.
fn entity(name: &str, generics: &[&str]) -> String {
    let mut declarations = vec!["runner_cfg : string".to_owned()];
    declarations.extend(
        generics
            .iter()
            .map(|generic| format!("{generic} : integer")),
    );
    format!(
        "entity {name} is\n  generic ({});\nend entity;\n",
        declarations.join(";\n    ")
    )
}

/// An architecture of `name` with `body` in its main process.
fn architecture(name: &str, body: &str) -> String {
    format!(
        "architecture arch of {name} is\nbegin\n  main : process\n  begin\n    \
         test_runner_setup(runner, runner_cfg);\n{body}\n    test_runner_cleanup(runner);\n  end \
         process;\nend architecture;\n"
    )
}

fn testbench(name: &str, body: &str) -> String {
    format!("{}{}", entity(name, &[]), architecture(name, body))
}

fn names(discovery: &Discovery) -> Vec<&str> {
    discovery
        .runs
        .iter()
        .map(|run| run.testcase.name.as_str())
        .collect()
}

fn messages(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.severity, diagnostic.message))
        .collect()
}

fn single_project(code: &str) -> Discovery {
    let mut test = TestProject::new();
    test.add("/src/file.vhd", code);
    test.discover()
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn single_implicit_test_is_created() {
    let discovery = single_project(&testbench("tb_entity", ""));
    assert_eq!(names(&discovery), ["lib.tb_entity.all"]);
    assert_eq!(discovery.project_diagnostics, []);
    let testcase = &discovery.runs[0].testcase;
    assert_eq!(testcase.file, "/src/file.vhd");
    // `test_runner_setup` on line 8.
    assert_eq!(testcase.range.start, Position::new(8, 5));
    assert_eq!(testcase.range.end, Position::new(8, 21));
    assert_eq!(discovery.runs[0].test, None);
    let testbench = &discovery.testbenches[0];
    assert_eq!(testbench.entity, "tb_entity");
    assert_eq!(testbench.architecture, "arch");
}

#[test]
fn names_keep_declared_case() {
    let mut project = Project::new();
    project
        .add_library("MyLib", VhdlStandard::Vhdl2008, None)
        .unwrap();
    let code = testbench("TB_Entity", "if run(\"Test One\") then end if;");
    add_vhdl(&mut project, "MyLib", Utf8Path::new("/src/file.vhd"), &code);
    let discovery = discover(&project, &[]);
    assert_eq!(names(&discovery), ["MyLib.TB_Entity.Test One"]);
}

#[test]
fn no_architecture_is_an_error() {
    let discovery = single_project(&entity("tb_entity", &[]));
    assert_eq!(names(&discovery), Vec::<&str>::new());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        ["error: testbench 'tb_entity' has no architecture"]
    );
    assert_eq!(
        discovery.project_diagnostics[0].range.unwrap().start,
        Position::new(1, 8)
    );
}

#[test]
fn multiple_architectures_are_not_allowed_for_testbench() {
    let mut test = TestProject::new();
    test.add("/src/file.vhd", &testbench("tb_entity", ""));
    test.add(
        "/src/arch2.vhd",
        "architecture arch2 of tb_entity is begin end architecture;",
    );
    let discovery = test.discover();
    assert_eq!(names(&discovery), Vec::<&str>::new());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "error: testbench 'tb_entity' isn't allowed to have several architectures; it has \
         arch2:arch2.vhd, arch:file.vhd"
        ]
    );
}

#[test]
fn creates_explicit_tests() {
    let discovery = single_project(&testbench(
        "tb_entity",
        r#"
if run("Test 1")
--if run("Test 2")
if run("Test 3")
if run("Test 4")
if run("Test 5") or run("Test 6")
if run  ("Test 7")
if run( "Test 8"  )
if ((run("Test 9")))
if my_protected_variable.run("Test 10")
"#,
    ));
    assert_eq!(
        names(&discovery),
        [
            "lib.tb_entity.Test 1",
            "lib.tb_entity.Test 3",
            "lib.tb_entity.Test 4",
            "lib.tb_entity.Test 5",
            "lib.tb_entity.Test 6",
            "lib.tb_entity.Test 7",
            "lib.tb_entity.Test 8",
            "lib.tb_entity.Test 9",
        ]
    );
    assert_eq!(discovery.runs[0].test.as_deref(), Some("Test 1"));
}

#[test]
fn tests_are_scanned_from_the_architecture_file() {
    let mut test = TestProject::new();
    test.add("/src/entity.vhd", &entity("tb_entity", &[]));
    test.add(
        "/src/arch.vhd",
        &architecture(
            "tb_entity",
            "if run(\"Test_1\")\n--if run(\"Test_2\")\nif run(\"Test_3\")",
        ),
    );
    let discovery = test.discover();
    assert_eq!(
        names(&discovery),
        ["lib.tb_entity.Test_1", "lib.tb_entity.Test_3"]
    );
    assert_eq!(discovery.runs[0].testcase.file, "/src/arch.vhd");
    let testbench = &discovery.testbenches[0];
    assert_eq!(
        test.project.file(testbench.entity_file).path,
        "/src/entity.vhd"
    );
    assert_eq!(
        test.project.file(testbench.architecture_file).path,
        "/src/arch.vhd"
    );
    // `tb_path` is the directory of the entity file.
    assert_eq!(discovery.runs[0].configuration.tb_path, "/src");
}

fn assert_test_1_location(code: &str) {
    let lines = LineIndex::new(code.as_bytes());
    let scan = scan_tests(code.as_bytes(), code.as_bytes(), &lines);
    let offset = code.find("Test_1").unwrap();
    assert_eq!(scan.tests[0].offset, offset);
    assert_eq!(
        scan.tests[0].range,
        lines.range(offset..offset + "Test_1".len())
    );
    assert_eq!(scan.tests[0].range.start, Position::new(3, 10));
}

#[test]
fn test_location_unix() {
    assert_test_1_location("foo \n bar \n if run(\"Test_1\")");
}

#[test]
fn test_location_dos() {
    assert_test_1_location("foo \r\n bar \r\n if run(\"Test_1\")");
}

#[test]
fn named_configurations_replace_the_default() {
    let mut test = TestProject::new();
    test.add("/src/file.vhd", &testbench("tb_entity", ""));
    assert_eq!(names(&test.discover()), ["lib.tb_entity.all"]);

    let configs = [TestConfigSpec {
        target: "lib.tb_entity".to_owned(),
        configurations: vec![ConfigurationSpec {
            name: "config".to_owned(),
            ..ConfigurationSpec::default()
        }],
        ..TestConfigSpec::default()
    }];
    let discovery = discover(&test.project, &configs);
    assert_eq!(names(&discovery), ["lib.tb_entity.config"]);
    assert_eq!(discovery.config_diagnostics, []);
}

#[test]
fn run_all_in_same_sim_is_ignored() {
    let discovery = single_project(&testbench(
        "tb_entity",
        "-- vunit: run_all_in_same_sim\nif run(\"Test_1\")\nif run(\"Test_2\")\n--if run(\"Test_3\")",
    ));
    assert_eq!(
        names(&discovery),
        ["lib.tb_entity.Test_1", "lib.tb_entity.Test_2"]
    );
    assert_eq!(discovery.project_diagnostics, []);
}

#[test]
fn testbench_configurations() {
    let mut test = TestProject::new();
    test.add(
        "/src/file.vhd",
        &format!(
            "{}{}",
            entity("tb_entity", &["value", "global_value"]),
            architecture("tb_entity", "")
        ),
    );
    let configs = [TestConfigSpec {
        target: "LIB.TB_ENTITY".to_owned(),
        configurations: vec![
            ConfigurationSpec {
                name: "value=1".to_owned(),
                generics: map(&[("value", "1"), ("global_value", "local value")]),
                ..ConfigurationSpec::default()
            },
            ConfigurationSpec {
                name: "value=2".to_owned(),
                generics: map(&[("value", "2")]),
                attributes: map(&[(".foo", "bar")]),
                vhdl_configuration_name: Some("cfg".to_owned()),
                ..ConfigurationSpec::default()
            },
            ConfigurationSpec {
                name: "c3".to_owned(),
                attributes: map(&[("foo", "bar")]),
                ..ConfigurationSpec::default()
            },
        ],
        generics: map(&[("global_value", "global value")]),
        ..TestConfigSpec::default()
    }];
    let discovery = discover(&test.project, &configs);
    assert_eq!(
        names(&discovery),
        ["lib.tb_entity.value=1", "lib.tb_entity.value=2"]
    );
    assert_eq!(
        messages(&discovery.config_diagnostics),
        [
            "error: configuration target 'LIB.TB_ENTITY': invalid attribute 'foo': attributes of \
         configurations must start with '.'"
        ]
    );

    let value1 = &discovery.runs[0];
    assert_eq!(
        value1.configuration.generics,
        map(&[("global_value", "local value"), ("value", "1")])
    );
    assert!(value1.testcase.attributes.is_empty());
    assert_eq!(value1.configuration.vhdl_configuration_name, None);
    let value2 = &discovery.runs[1];
    assert_eq!(
        value2.configuration.generics,
        map(&[("global_value", "global value"), ("value", "2")])
    );
    assert_eq!(value2.testcase.attributes, [".foo"]);
    assert_eq!(
        value2.configuration.vhdl_configuration_name.as_deref(),
        Some("cfg")
    );
}

#[test]
fn test_configurations() {
    let mut test = TestProject::new();
    test.add(
        "/src/file.vhd",
        &format!(
            "{}{}",
            entity("tb_entity", &["value", "global_value"]),
            architecture(
                "tb_entity",
                "if run(\"test 1\")\nif run(\"test 2\") -- vunit: .slow",
            )
        ),
    );
    let configs = [
        TestConfigSpec {
            target: "lib.tb_entity".to_owned(),
            generics: map(&[("global_value", "global value")]),
            ..TestConfigSpec::default()
        },
        TestConfigSpec {
            target: "lib.tb_entity.test 2".to_owned(),
            configurations: vec![
                ConfigurationSpec {
                    name: "c1".to_owned(),
                    generics: map(&[("value", "1"), ("global_value", "local value")]),
                    ..ConfigurationSpec::default()
                },
                ConfigurationSpec {
                    name: "c2".to_owned(),
                    generics: map(&[("value", "2")]),
                    sim_options: SimOptions {
                        disable_ieee_warnings: Some(false),
                        ..SimOptions::default()
                    },
                    attributes: map(&[(".foo", "bar")]),
                    ..ConfigurationSpec::default()
                },
            ],
            ..TestConfigSpec::default()
        },
    ];
    let discovery = discover(&test.project, &configs);
    assert_eq!(
        names(&discovery),
        [
            "lib.tb_entity.test 1",
            "lib.tb_entity.c1.test 2",
            "lib.tb_entity.c2.test 2",
        ]
    );
    assert_eq!(discovery.config_diagnostics, []);
    let [test1, c1_test2, c2_test2] = discovery.runs.as_slice() else {
        panic!("expected three runs");
    };
    assert_eq!(
        test1.configuration.generics,
        map(&[("global_value", "global value")])
    );
    assert_eq!(
        c1_test2.configuration.generics,
        map(&[("global_value", "local value"), ("value", "1")])
    );
    assert_eq!(c1_test2.testcase.attributes, [".slow"]);
    assert_eq!(
        c2_test2.configuration.generics,
        map(&[("global_value", "global value"), ("value", "2")])
    );
    assert_eq!(c2_test2.testcase.attributes, [".foo", ".slow"]);
    assert_eq!(
        c2_test2.configuration.sim_options.disable_ieee_warnings,
        Some(false)
    );
    assert_eq!(c2_test2.test.as_deref(), Some("test 2"));
    assert_eq!(c2_test2.configuration.name.as_deref(), Some("c2"));
}

#[test]
fn testbench_configurations_apply_to_every_test() {
    let mut test = TestProject::new();
    test.add(
        "/src/file.vhd",
        &testbench("tb_entity", "if run(\"Test 1\")\nif run(\"Test 2\")"),
    );
    let configs = [TestConfigSpec {
        target: "lib.tb_entity".to_owned(),
        configurations: vec![ConfigurationSpec {
            name: "cfg".to_owned(),
            ..ConfigurationSpec::default()
        }],
        ..TestConfigSpec::default()
    }];
    let discovery = discover(&test.project, &configs);
    assert_eq!(
        names(&discovery),
        ["lib.tb_entity.cfg.Test 1", "lib.tb_entity.cfg.Test 2"]
    );
}

#[test]
fn failing_configurations_are_added_to_no_test() {
    let mut test = TestProject::new();
    test.add(
        "/src/file.vhd",
        &testbench("tb_entity", "if run(\"Test 1\")\nif run(\"Test 2\")"),
    );
    let config = |target: &str, names: &[&str]| TestConfigSpec {
        target: target.to_owned(),
        configurations: names
            .iter()
            .map(|&name| ConfigurationSpec {
                name: name.to_owned(),
                ..ConfigurationSpec::default()
            })
            .collect(),
        ..TestConfigSpec::default()
    };
    let configs = [
        config("lib.tb_entity.Test 2", &["cfg"]),
        // `cfg` fails for `Test 2`, so `Test 1` doesn't get it either.
        config("lib.tb_entity", &["cfg", "other"]),
    ];
    let discovery = discover(&test.project, &configs);
    assert_eq!(
        names(&discovery),
        [
            "lib.tb_entity.other.Test 1",
            "lib.tb_entity.cfg.Test 2",
            "lib.tb_entity.other.Test 2",
        ]
    );
    assert_eq!(
        messages(&discovery.config_diagnostics),
        ["error: configuration target 'lib.tb_entity': configuration name 'cfg' already defined"]
    );
}

#[test]
fn invalid_configuration_targets() {
    let mut test = TestProject::new();
    test.add("/src/implicit.vhd", &testbench("tb_implicit", ""));
    test.add(
        "/src/explicit.vhd",
        &testbench("tb_explicit", "if run(\"Test 1\")"),
    );
    test.add("/src/broken.vhd", &entity("tb_broken", &[]));
    let target = |target: &str| TestConfigSpec {
        target: target.to_owned(),
        generics: map(&[("unknown", "1")]),
        ..TestConfigSpec::default()
    };
    let configs = [
        target("lib"),
        target("other.tb_explicit"),
        target("lib.tb_missing"),
        target("lib.tb_implicit.test"),
        target("lib.tb_explicit.Test 2"),
        target("lib.tb_explicit.test 1"),
        // Skipped because of errors, which are reported already.
        target("lib.tb_broken"),
        target("lib.tb_explicit"),
    ];
    let discovery = discover(&test.project, &configs);
    assert_eq!(
        messages(&discovery.config_diagnostics),
        [
            "warning: configuration target 'lib': expected 'library.testbench' or \
         'library.testbench.test'",
            "warning: configuration target 'other.tb_explicit': no such library",
            "warning: configuration target 'lib.tb_missing': no such testbench",
            "warning: configuration target 'lib.tb_implicit.test': the testbench has no explicit tests",
            "warning: configuration target 'lib.tb_explicit.Test 2': no such test",
            "warning: configuration target 'lib.tb_explicit.test 1': no such test",
            "warning: generic 'unknown' set to value '1' not found in entity 'lib.tb_explicit'; \
         possible values are [runner_cfg]",
        ]
    );
}

#[test]
fn sim_options_and_vhdl_configuration_of_a_target_apply_to_its_configurations() {
    let mut test = TestProject::new();
    test.add(
        "/src/tb.vhd",
        &testbench("tb_entity", "if run(\"Test 1\")\nif run(\"Test 2\")"),
    );
    let warning = SimOptions {
        vhdl_assert_stop_level: Some(AssertLevel::Warning),
        ..SimOptions::default()
    };
    let configs = [
        TestConfigSpec {
            target: "lib.tb_entity".to_owned(),
            vhdl_configuration_name: Some("cfg1".to_owned()),
            ..TestConfigSpec::default()
        },
        TestConfigSpec {
            target: "lib.tb_entity.Test 2".to_owned(),
            configurations: vec![ConfigurationSpec {
                name: "copy".to_owned(),
                ..ConfigurationSpec::default()
            }],
            sim_options: warning.clone(),
            ..TestConfigSpec::default()
        },
    ];
    let discovery = discover(&test.project, &configs);
    assert_eq!(discovery.config_diagnostics, []);
    let configuration = |name: &str| &discovery.run(name).unwrap().configuration;
    let first = configuration("lib.tb_entity.Test 1");
    assert_eq!(first.vhdl_configuration_name.as_deref(), Some("cfg1"));
    assert_eq!(first.sim_options, SimOptions::default());
    // The added configuration copies the default one, including the options set before.
    let copy = configuration("lib.tb_entity.copy.Test 2");
    assert_eq!(copy.vhdl_configuration_name.as_deref(), Some("cfg1"));
    assert_eq!(copy.sim_options, warning);
}

#[test]
fn global_user_attributes_are_not_supported_yet() {
    let discovery = single_project(&testbench(
        "tb_entity",
        "-- vunit: .attr0\nif run(\"Test 1\")\nif run(\"Test 2\")",
    ));
    assert_eq!(names(&discovery), Vec::<&str>::new());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        ["error: file global attributes are not yet supported: .attr0"]
    );
    assert_eq!(
        discovery.project_diagnostics[0].range.unwrap().start,
        Position::new(9, 11)
    );
}

#[test]
fn builtin_attributes_on_tests_are_errors() {
    let discovery = single_project(&testbench(
        "tb_entity",
        "if run(\"Test 1\")\n-- vunit: run_all_in_same_sim\nif run(\"Test 2\")",
    ));
    assert_eq!(names(&discovery), Vec::<&str>::new());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "error: attribute run_all_in_same_sim is global and can't be associated with test 'Test 1'"
        ]
    );
}

#[test]
fn builtin_attributes_after_the_implicit_test_are_errors() {
    let discovery = single_project(&testbench("tb_entity", "-- vunit: fail_on_warning"));
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "error: attribute fail_on_warning is global and can't be associated with the implicit test"
        ]
    );
}

#[test]
fn user_attributes_after_the_implicit_test_belong_to_it() {
    let discovery = single_project(&testbench("tb_entity", "-- vunit: .slow"));
    assert_eq!(discovery.runs[0].testcase.attributes, [".slow"]);
}

#[test]
fn fail_on_warning_sets_the_assert_stop_level() {
    let code = format!(
        "-- vunit: fail_on_warning\n{}",
        testbench("tb_entity", "if run(\"Test 1\")\nif run(\"Test 2\")")
    );
    let discovery = single_project(&code);
    assert_eq!(discovery.project_diagnostics, []);
    for run in &discovery.runs {
        assert_eq!(
            run.configuration.vhdl_assert_stop_level(),
            Some(AssertLevel::Warning)
        );
    }
    let without = single_project(&testbench("tb_entity", "if run(\"Test 1\")"));
    assert_eq!(without.runs[0].configuration.vhdl_assert_stop_level(), None);
}

#[test]
fn legacy_pragmas_are_global() {
    let code = format!(
        "vunit_pragma fail_on_warning\n{}",
        testbench(
            "tb_entity",
            "if run(\"test1\")\n-- vunit_pragma run_all_in_same_sim"
        )
    );
    let discovery = single_project(&code);
    assert_eq!(discovery.project_diagnostics, []);
    assert_eq!(
        discovery.runs[0].configuration.vhdl_assert_stop_level(),
        Some(AssertLevel::Warning)
    );
    assert!(discovery.runs[0].testcase.attributes.is_empty());
}

#[test]
fn invalid_attributes_are_errors() {
    let discovery = single_project(&testbench("tb_entity", "\n\n// vunit: invalid"));
    assert_eq!(names(&discovery), Vec::<&str>::new());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        ["error: invalid attribute 'invalid'"]
    );
    assert_eq!(
        discovery.project_diagnostics[0].range.unwrap().start,
        Position::new(11, 11)
    );
}

#[test]
fn duplicate_tests_are_errors() {
    let discovery = single_project(&testbench(
        "tb_entity",
        r#"if run("Test_1")
--if run("Test_1")
if run("Test_3")
if run("Test_2")
if run("Test_3")
if run("Test_3")
if run("Test_2")"#,
    ));
    assert_eq!(names(&discovery), Vec::<&str>::new());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "error: duplicate test \"Test_3\", previously defined on line 11",
            "error: duplicate test \"Test_3\", previously defined on line 11",
            "error: duplicate test \"Test_2\", previously defined on line 12",
        ]
    );
    let lines: Vec<u32> = discovery
        .project_diagnostics
        .iter()
        .map(|diagnostic| diagnostic.range.unwrap().start.line)
        .collect();
    assert_eq!(lines, [13, 14, 15]);
}

#[test]
fn tests_differing_in_case_produce_a_warning() {
    let discovery = single_project(&testbench(
        "tb_entity",
        "if run(\"test\")\nif run(\"Test\")",
    ));
    assert_eq!(
        names(&discovery),
        ["lib.tb_entity.test", "lib.tb_entity.Test"]
    );
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "warning: test \"Test\" differs from test \"test\" on line 9 only in case; testcase \
         patterns can't tell them apart"
        ]
    );
}

#[test]
fn attributes_are_associated_with_the_preceding_test() {
    let discovery = single_project(&testbench(
        "tb_entity",
        r#"        if run("test1")
// vunit: .arg1
// vunit: .arg1b
        if run("test2") // vunit: .arg2
// vunit: .arg2b
"#,
    ));
    assert_eq!(discovery.project_diagnostics, []);
    let attributes: Vec<&[String]> = discovery
        .runs
        .iter()
        .map(|run| run.testcase.attributes.as_slice())
        .collect();
    assert_eq!(
        attributes,
        [&[".arg1", ".arg1b"][..], &[".arg2", ".arg2b"][..]]
    );
}

#[test]
fn duplicate_attributes_of_different_tests_are_ok() {
    let discovery = single_project(&testbench(
        "tb_entity",
        "if run(\"test1\")\n// vunit: .arg0\nif run(\"test2\")\n// vunit: .arg0",
    ));
    assert_eq!(discovery.project_diagnostics, []);
    assert_eq!(discovery.runs[1].testcase.attributes, [".arg0"]);
}

#[test]
fn duplicate_test_attributes_are_errors() {
    let discovery = single_project(&testbench(
        "tb_entity",
        "if run(\"test1\")\n// vunit: .arg0\n// vunit: .arg0",
    ));
    assert_eq!(
        messages(&discovery.project_diagnostics),
        ["error: duplicate attribute .arg0 of test 'test1', previously defined on line 10"]
    );
}

#[test]
fn duplicate_global_attributes_are_errors() {
    let code = format!(
        "-- vunit: fail_on_warning\n-- vunit: fail_on_warning\n{}",
        testbench("tb_entity", "if run(\"test1\")")
    );
    let discovery = single_project(&code);
    assert_eq!(
        messages(&discovery.project_diagnostics),
        ["error: duplicate attribute fail_on_warning, previously defined on line 1"]
    );
}

#[test]
fn missing_test_suite_is_a_warning() {
    let code = format!(
        "{}architecture arch of tb_entity is begin end;",
        entity("tb_entity", &[])
    );
    let discovery = single_project(&code);
    assert_eq!(names(&discovery), ["lib.tb_entity.all"]);
    assert_eq!(
        messages(&discovery.project_diagnostics),
        ["warning: found no tests or test suite (test_runner_setup)"]
    );
    assert_eq!(discovery.runs[0].testcase.range, FILE_START);
}

#[test]
fn scan_finds_tests_suite_and_attributes() {
    let code = "\
        -- if run(\"No test\")
        if run(\"Test 1\")
        if RUN(\"Test 2\") -- vunit: .attr
        Test_Runner_Setup (
-- VUNIT_PRAGMA fail_on_warning
";
    let source = code.as_bytes();
    let lines = LineIndex::new(source);
    let scan = scan_tests(source, &crate::vhdl_parser::remove_comments(source), &lines);
    let tests: Vec<(&str, u32)> = scan
        .tests
        .iter()
        .map(|test| (test.name.as_str(), test.range.start.line))
        .collect();
    assert_eq!(tests, [("Test 1", 2), ("Test 2", 3)]);
    let suite = scan.suite.unwrap();
    assert_eq!(suite.name, "Test_Runner_Setup");
    assert_eq!(suite.range.start.line, 4);
    let attributes: Vec<(&str, bool, u32)> = scan
        .attributes
        .iter()
        .map(|attribute| {
            (
                attribute.name.as_str(),
                attribute.legacy,
                attribute.range.start.line,
            )
        })
        .collect();
    assert_eq!(
        attributes,
        [("fail_on_warning", true, 5), (".attr", false, 3)]
    );
}

#[test]
fn tb_filter_requires_runner_cfg() {
    let mut test = TestProject::new();
    test.add(
        "/src/file.vhd",
        "entity tb_entity is generic (other : integer); end entity;\n\
         architecture arch of tb_entity is begin end;",
    );
    let discovery = test.discover();
    assert!(discovery.testbenches.is_empty());
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "warning: entity tb_entity matches the testbench name pattern ^(tb_.*)|(.*_tb)$ but has \
         no generic runner_cfg and will therefore not be run"
        ]
    );
}

#[test]
fn tb_filter_matches_prefix_and_suffix_only() {
    let discovery = single_project(
        "entity mul_tbl_scale is end entity;\narchitecture a of mul_tbl_scale is begin end;",
    );
    assert!(discovery.testbenches.is_empty());
    assert_eq!(discovery.project_diagnostics, []);
}

#[test]
fn tb_filter_warns_about_runner_cfg_without_testbench_name() {
    let discovery = single_project(&testbench("entity_ok_but_warning", ""));
    assert_eq!(names(&discovery), ["lib.entity_ok_but_warning.all"]);
    assert_eq!(
        messages(&discovery.project_diagnostics),
        [
            "warning: entity entity_ok_but_warning has a generic runner_cfg, but its name doesn't \
         match the testbench name pattern ^(tb_.*)|(.*_tb)$"
        ]
    );
    // Suffix match, case-insensitive.
    let suffix = single_project(&testbench("Entity_TB", ""));
    assert_eq!(suffix.project_diagnostics, []);
}

#[test]
fn redefined_testbench_keeps_its_position() {
    let mut test = TestProject::new();
    test.add("/src/a.vhd", &testbench("tb_a", "if run(\"old\")"));
    test.add("/src/b.vhd", &testbench("tb_b", ""));
    test.add("/src/a2.vhd", &testbench("tb_a", "if run(\"new\")"));
    let discovery = test.discover();
    // The second architecture `arch` of `tb_a` replaces the first one.
    assert_eq!(names(&discovery), ["lib.tb_a.new", "lib.tb_b.all"]);
}

#[test]
fn tb_path_generic_is_set() {
    let code = format!(
        "{}{}",
        entity("tb_entity", &[]).replace(
            "runner_cfg : string",
            "runner_cfg : string; tb_path : string"
        ),
        architecture("tb_entity", "")
    );
    let mut test = TestProject::new();
    test.add("/some/dir/file.vhd", &code);
    let discovery = test.discover();
    assert_eq!(
        discovery.runs[0].configuration.generics,
        map(&[("tb_path", "/some/dir/")])
    );
}

#[test]
fn testcases_and_lookup() {
    let discovery = single_project(&testbench("tb_entity", "if run(\"a\")\nif run(\"b\")"));
    let testcases = discovery.testcases();
    assert_eq!(testcases.len(), 2);
    assert_eq!(testcases[1].name, "lib.tb_entity.b");
    assert_eq!(
        discovery.run("lib.tb_entity.a").unwrap().test.as_deref(),
        Some("a")
    );
    assert!(discovery.run("lib.tb_entity.c").is_none());
}
