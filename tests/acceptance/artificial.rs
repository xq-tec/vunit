// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! `tests/acceptance/artificial/vhdl`, ported from `test_artificial.py`.
//!
//! AI NOTICE: Generated, minimally reviewed.

use risim_vunit_frontend::SimulatorKind;
use risim_vunit_frontend::TestOutcome;
use risim_vunit_frontend::spec::AssertLevel;
use risim_vunit_frontend::spec::ConfigurationSpec;
use risim_vunit_frontend::spec::ProjectSpec;
use risim_vunit_frontend::spec::SimOptions;
use risim_vunit_frontend::spec::TestConfigSpec;

use crate::harness;
use crate::harness::RisimDeviations;
use crate::harness::config;
use crate::harness::generics;
use crate::harness::target;

/// Compiles and runs all testcases and compares the outcomes with `test_artificial.py`.
pub async fn artificial(simulator: SimulatorKind) {
    let runtime = harness::runtime().await;
    let root = harness::Root::new();
    let mut spec = ProjectSpec::new();
    artificial_spec(&mut spec);
    let outcome = harness::simulate_all(&runtime, &root.path, &spec, simulator).await;
    // risim: access, enclosing unit.
    outcome.assert_outcomes(
        &harness::outcomes(ARTIFICIAL_EXPECTED),
        RisimDeviations::AllFailing,
    );
}

/// `tests/acceptance/artificial/vhdl/run.py` as a project spec, without hooks, test history,
/// `scan_tests_from_file` and `add_package`.
#[expect(clippy::too_many_lines, reason = "follows run.py function by function")]
fn artificial_spec(spec: &mut ProjectSpec) {
    let root = harness::fixtures_dir().join("artificial/vhdl");
    // `tb_vunit_pkg` needs `add_package`, which isn't supported.
    let lib = harness::vhd_files(&root, |name| {
        name == "tb_set_generic.vhd" || name == "tb_vunit_pkg.vhd"
    });
    spec.add_library("lib", lib)
        .add_library("lib2", [root.join("tb_set_generic.vhd").to_string()]);

    let configs = &mut spec.test_configs;
    // configure_tb_with_generic_config
    configs.push(TestConfigSpec {
        generics: generics(&[("set_generic", "set-for-entity")]),
        ..target("lib.tb_with_generic_config")
    });
    configs.push(TestConfigSpec {
        configurations: vec![ConfigurationSpec {
            generics: generics(&[("config_generic", "set-from-config")]),
            ..config("cfg")
        }],
        ..target("lib.tb_with_generic_config.Test 1")
    });
    configs.push(TestConfigSpec {
        generics: generics(&[("set_generic", "set-for-test")]),
        ..target("lib.tb_with_generic_config.Test 2")
    });
    configs.push(TestConfigSpec {
        configurations: vec![ConfigurationSpec {
            generics: generics(&[
                ("set_generic", "set-for-test"),
                ("config_generic", "set-from-config"),
            ]),
            ..config("cfg")
        }],
        ..target("lib.tb_with_generic_config.Test 3")
    });
    configs.push(TestConfigSpec {
        configurations: vec![ConfigurationSpec {
            generics: generics(&[
                ("set_generic", "set-from-config"),
                ("config_generic", "set-from-config"),
            ]),
            ..config("cfg")
        }],
        ..target("lib.tb_with_generic_config.Test 4")
    });
    // configure_tb_same_sim_all_pass
    configs.push(TestConfigSpec {
        configurations: vec![config("cfg")],
        ..target("lib.tb_same_sim_all_pass")
    });
    configs.push(TestConfigSpec {
        configurations: vec![ConfigurationSpec {
            attributes: generics(&[("run_all_in_same_sim", "true")]),
            ..config("cfg")
        }],
        ..target("lib.tb_same_sim_from_python_all_pass")
    });
    // configure_tb_set_generic; risim-ghdl is GHDL, which can't override real and time generics.
    let long_value = "0123456789abcdef".repeat(512);
    configs.push(TestConfigSpec {
        generics: generics(&[
            ("is_ghdl", "True"),
            ("true_boolean", "True"),
            ("false_boolean", "False"),
            ("negative_integer", "-10000"),
            ("positive_integer", "99999"),
            ("str_val", "4ns"),
            ("str_space_val", "1 2 3"),
            ("str_quote_val", "a\"b"),
            ("str_long_num", "512"),
            ("str_long_val", &long_value),
        ]),
        ..target("lib2.tb_set_generic")
    });
    // configure_tb_assert_stop_level
    let levels = [
        ("warning", AssertLevel::Warning),
        ("error", AssertLevel::Error),
        ("failure", AssertLevel::Failure),
    ];
    for (stop_level, level) in levels {
        for (report_level, _) in levels {
            configs.push(TestConfigSpec {
                sim_options: SimOptions {
                    vhdl_assert_stop_level: Some(level),
                    ..SimOptions::default()
                },
                ..target(&format!(
                    "lib.tb_assert_stop_level.Report {report_level} when VHDL assert stop level \
                     = {stop_level}"
                ))
            });
        }
    }
    // configure_tb_with_vhdl_configuration
    configs.push(TestConfigSpec {
        vhdl_configuration_name: Some("cfg1".to_owned()),
        ..target("lib.tb_with_vhdl_configuration")
    });
    configs.push(TestConfigSpec {
        vhdl_configuration_name: Some("cfg2".to_owned()),
        configurations: vec![config("cfg2")],
        ..target("lib.tb_with_vhdl_configuration.test 2")
    });
    configs.push(TestConfigSpec {
        configurations: vec![ConfigurationSpec {
            vhdl_configuration_name: Some("cfg3".to_owned()),
            ..config("cfg3")
        }],
        ..target("lib.tb_with_vhdl_configuration.test 3")
    });
    // configure_tb_no_fail_on_warning
    configs.push(TestConfigSpec {
        configurations: vec![
            ConfigurationSpec {
                attributes: generics(&[("fail_on_warning", "False")]),
                ..config("cfg1")
            },
            config("cfg2"),
        ],
        ..target("lib.tb_no_fail_on_warning")
    });
    // configure_tb_test_prio, without the `pre_config` hooks
    configs.push(TestConfigSpec {
        configurations: (1..=4)
            .map(|index| config(&format!("test_{index}")))
            .collect(),
        ..target("lib.tb_test_prio_1")
    });
    configs.push(TestConfigSpec {
        configurations: (1..=2)
            .map(|index| config(&format!("test_{index}")))
            .collect(),
        ..target("lib.tb_test_prio_2")
    });
    configs.push(TestConfigSpec {
        generics: generics(&[("g_val", "False")]),
        ..target("lib.tb_no_generic_override")
    });
    configs.push(TestConfigSpec {
        sim_options: SimOptions {
            disable_ieee_warnings: Some(true),
            ..SimOptions::default()
        },
        ..target("lib.tb_ieee_warning.pass")
    });
    // `set_attribute("fail_on_warning", True)` on the testbench.
    configs.push(TestConfigSpec {
        sim_options: SimOptions {
            vhdl_assert_stop_level: Some(AssertLevel::Warning),
            ..SimOptions::default()
        },
        ..target("lib.tb_fail_on_warning_from_python")
    });
}

/// `EXPECTED_REPORT` of `test_artificial.py`, adapted to the intentional deviations.
const ARTIFICIAL_EXPECTED: &[(&str, TestOutcome)] = {
    use TestOutcome::Failed;
    use TestOutcome::Passed;
    &[
        // `scan_tests_from_file` isn't supported, so the tests of `other_file_tests.vhd` aren't
        // found; the testbench runs as one test without enabled tests, which passes.
        ("lib.tb_other_file_tests.all", Passed),
        ("lib.tb_pass.all", Passed),
        ("lib.tb_fail.all", Failed),
        ("lib.tb_infinite_events.all", Passed),
        ("lib.tb_fail_on_warning.all", Failed),
        ("lib.tb_fail_on_warning_from_python.all", Failed),
        ("lib.tb_no_fail_on_warning.cfg1", Passed),
        ("lib.tb_no_fail_on_warning.cfg2", Passed),
        ("lib.tb_with_vhdl_runner.pass", Passed),
        ("lib.tb_with_vhdl_runner.Test with spaces", Passed),
        ("lib.tb_with_vhdl_runner.fail", Failed),
        ("lib.tb_with_vhdl_runner.Test that timeouts", Failed),
        // There is no run script, so `run_script_path(runner_cfg)` is empty.
        ("lib.tb_magic_paths.all", Failed),
        ("lib.tb_no_fail_after_cleanup.all", Passed),
        ("lib.tb_elab_fail.all", Failed),
        ("lib.tb_same_sim_all_pass.cfg.Test 1", Passed),
        // Tests 2 and 3 rely on Test 1 running before them in the same simulation.
        ("lib.tb_same_sim_all_pass.cfg.Test 2", Failed),
        ("lib.tb_same_sim_all_pass.cfg.Test 3", Failed),
        ("lib.tb_same_sim_some_fail.Test 1", Passed),
        ("lib.tb_same_sim_some_fail.Test 2", Failed),
        // Runs in its own simulation, so the failure of Test 2 doesn't skip it.
        ("lib.tb_same_sim_some_fail.Test 3", Passed),
        ("lib.tb_same_sim_from_python_all_pass.cfg.Test 1", Passed),
        // Tests 2 and 3 rely on Test 1 running before them in the same simulation.
        ("lib.tb_same_sim_from_python_all_pass.cfg.Test 2", Failed),
        ("lib.tb_same_sim_from_python_all_pass.cfg.Test 3", Failed),
        ("lib.tb_same_sim_from_python_some_fail.Test 1", Passed),
        ("lib.tb_same_sim_from_python_some_fail.Test 2", Failed),
        ("lib.tb_same_sim_from_python_some_fail.Test 3", Passed),
        ("lib.tb_with_checks.Test passing check", Passed),
        ("lib.tb_with_checks.Test failing check", Failed),
        ("lib.tb_with_checks.Test non-stopping failing check", Failed),
        ("lib2.tb_set_generic.all", Passed),
        ("lib.tb_with_generic_config.Test 0", Passed),
        ("lib.tb_with_generic_config.cfg.Test 1", Passed),
        ("lib.tb_with_generic_config.Test 2", Passed),
        ("lib.tb_with_generic_config.cfg.Test 3", Passed),
        ("lib.tb_with_generic_config.cfg.Test 4", Passed),
        ("lib.tb_no_generic_override.all", Passed),
        ("lib.tb_ieee_warning.pass", Passed),
        ("lib.tb_ieee_warning.fail", Failed),
        (
            "lib.tb_assert_stop_level.Report warning when VHDL assert stop level = warning",
            Failed,
        ),
        (
            "lib.tb_assert_stop_level.Report error when VHDL assert stop level = warning",
            Failed,
        ),
        (
            "lib.tb_assert_stop_level.Report failure when VHDL assert stop level = warning",
            Failed,
        ),
        (
            "lib.tb_assert_stop_level.Report warning when VHDL assert stop level = error",
            Passed,
        ),
        (
            "lib.tb_assert_stop_level.Report error when VHDL assert stop level = error",
            Failed,
        ),
        (
            "lib.tb_assert_stop_level.Report failure when VHDL assert stop level = error",
            Failed,
        ),
        (
            "lib.tb_assert_stop_level.Report warning when VHDL assert stop level = failure",
            Passed,
        ),
        (
            "lib.tb_assert_stop_level.Report error when VHDL assert stop level = failure",
            Passed,
        ),
        (
            "lib.tb_assert_stop_level.Report failure when VHDL assert stop level = failure",
            Failed,
        ),
        ("lib.tb_with_vhdl_configuration.test 1", Passed),
        ("lib.tb_with_vhdl_configuration.cfg2.test 2", Passed),
        ("lib.tb_with_vhdl_configuration.cfg3.test 3", Passed),
        // Without the `pre_config` hooks, the tests that VUnit fails there pass.
        ("lib.tb_test_prio_1.test_1", Passed),
        ("lib.tb_test_prio_1.test_2", Passed),
        ("lib.tb_test_prio_1.test_3", Passed),
        ("lib.tb_test_prio_1.test_4", Passed),
        ("lib.tb_test_prio_2.test_1", Passed),
        ("lib.tb_test_prio_2.test_2", Passed),
        ("lib.tb_seed.test_1", Passed),
        ("lib.tb_seed.test_2", Passed),
    ]
};
