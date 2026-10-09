// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Acceptance tests: real projects compiled and simulated with risim-ghdl through the workspace
//! API, ported from VUnit's `tests/acceptance` and the `run.py` scripts of its VHDL libraries.
//!
//! The trials use the risim-ghdl named by `RISIM_GHDL` and are ignored without it. Every trial
//! also runs on the `risim` backend as `<name>_risim`, with the risim-runner named by
//! `RISIM_RUNNER`; it is ignored unless both variables are set. With `ACCEPTANCE_KEEP` set, the
//! temporary workspaces aren't deleted, for debugging.
//!
//! AI NOTICE: Generated, minimally reviewed.

mod artificial;
mod dependencies;
mod harness;
#[path = "../common/trial.rs"]
mod trial;
mod vhdl_libraries;

use std::env;
use std::future::Future;
use std::process::ExitCode;

use libtest_mimic::Arguments;
use libtest_mimic::Trial;
use risim_vunit_frontend::SimulatorKind;

fn main() -> ExitCode {
    let args = Arguments::from_args();
    let trials = [
        trials("artificial_vhdl", artificial::artificial),
        trials(
            "package_body_dependencies",
            dependencies::package_body_dependencies,
        ),
        trials("vhdl_lib_check", vhdl_libraries::check_lib),
        trials("vhdl_lib_com", vhdl_libraries::com),
        trials("vhdl_lib_data_types", vhdl_libraries::data_types),
        trials("vhdl_lib_dictionary", vhdl_libraries::dictionary),
        trials("vhdl_lib_logging", vhdl_libraries::logging),
        trials("vhdl_lib_path", vhdl_libraries::path),
        trials("vhdl_lib_random", vhdl_libraries::random),
        trials("vhdl_lib_run", vhdl_libraries::run),
        trials("vhdl_lib_string_ops", vhdl_libraries::string_ops),
        trials(
            "vhdl_lib_verification_components",
            vhdl_libraries::verification_components,
        ),
    ];
    libtest_mimic::run(&args, trials.into_iter().flatten().collect()).exit_code()
}

/// The trial `name` on the `ghdl` backend and the trial `<name>_risim` on the `risim` backend.
///
/// The first is ignored unless risim-ghdl is available, the second unless risim-runner is
/// available too.
fn trials<F: Future<Output = ()> + 'static>(
    name: &str,
    test: fn(SimulatorKind) -> F,
) -> [Trial; 2] {
    let ghdl_missing = env::var_os(harness::RISIM_GHDL).is_none();
    let runner_missing = env::var_os(harness::RISIM_RUNNER).is_none();
    [
        trial::trial(name, move || test(SimulatorKind::Ghdl)).with_ignored_flag(ghdl_missing),
        trial::trial(&format!("{name}_risim"), move || test(SimulatorKind::Risim))
            .with_ignored_flag(ghdl_missing || runner_missing),
    ]
}
