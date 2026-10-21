// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Acceptance tests: real projects compiled and simulated with risim-runner through the
//! workspace API, mostly ported from VUnit's `tests/acceptance` and the `run.py` scripts of its
//! VHDL libraries.
//!
//! The trials use the risim-runner named by `RISIM_RUNNER` and are ignored without it. With
//! `ACCEPTANCE_KEEP` set, the temporary workspaces aren't deleted, for debugging.
//!
//! AI NOTICE: Generated, minimally reviewed.

mod artificial;
mod component_binding;
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

fn main() -> ExitCode {
    let args = Arguments::from_args();
    let trials = [
        trials("artificial_vhdl", artificial::artificial),
        trials("component_binding", component_binding::component_binding),
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
    libtest_mimic::run(&args, trials.into_iter().collect()).exit_code()
}

/// The trial `name`, ignored unless risim-runner is available.
fn trials<F: Future<Output = ()> + 'static>(name: &str, test: fn() -> F) -> Trial {
    trial::trial(name, test).with_ignored_flag(env::var_os(harness::RISIM_RUNNER).is_none())
}
