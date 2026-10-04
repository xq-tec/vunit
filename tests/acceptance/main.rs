// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Acceptance tests: real projects compiled and simulated with risim-ghdl through the workspace
//! API, ported from `VUnit`'s `tests/acceptance` and the `run.py` scripts of its VHDL libraries.
//!
//! The trials use the risim-ghdl named by `RISIM_GHDL` and are ignored without it. With
//! `ACCEPTANCE_KEEP` set, the temporary workspaces aren't deleted, for debugging.
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

fn main() -> ExitCode {
    let args = Arguments::from_args();
    let trials = vec![
        trial("artificial_vhdl", artificial::artificial),
        trial(
            "package_body_dependencies",
            dependencies::package_body_dependencies,
        ),
        trial("vhdl_lib_check", vhdl_libraries::check_lib),
        trial("vhdl_lib_com", vhdl_libraries::com),
        trial("vhdl_lib_data_types", vhdl_libraries::data_types),
        trial("vhdl_lib_dictionary", vhdl_libraries::dictionary),
        trial("vhdl_lib_logging", vhdl_libraries::logging),
        trial("vhdl_lib_path", vhdl_libraries::path),
        trial("vhdl_lib_random", vhdl_libraries::random),
        trial("vhdl_lib_run", vhdl_libraries::run),
        trial("vhdl_lib_string_ops", vhdl_libraries::string_ops),
        trial(
            "vhdl_lib_verification_components",
            vhdl_libraries::verification_components,
        ),
    ];
    libtest_mimic::run(&args, trials).exit_code()
}

/// A trial that is ignored unless risim-ghdl is available.
fn trial<F: Future<Output = ()>>(name: &str, test: impl FnOnce() -> F + Send + 'static) -> Trial {
    trial::trial(name, test).with_ignored_flag(env::var_os(harness::RISIM_GHDL).is_none())
}
