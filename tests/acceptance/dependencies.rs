// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! `tests/acceptance/dependencies`, ported from `test_dependencies.py`.
//!
//! AI NOTICE: Generated, minimally reviewed.

use risim_vunit_frontend::TestOutcome;
use risim_vunit_frontend::spec::ProjectSpec;
use risim_vunit_frontend::spec::TestConfigSpec;

use crate::harness;
use crate::harness::generics;
use crate::harness::target;

/// Some simulators require package users to be recompiled when only the package body changed.
/// The second run swaps the package body in the same workspace and must see the new body.
pub async fn package_body_dependencies() {
    let runtime = harness::runtime().await;
    let root = harness::Root::new();
    let dir = harness::fixtures_dir().join("dependencies");
    for value in [1, 2] {
        let mut spec = ProjectSpec::new();
        spec.add_library(
            "lib",
            [
                dir.join("tb_pkg.vhd").to_string(),
                dir.join("pkg.vhd").to_string(),
                dir.join(format!("pkg_body{value}.vhd")).to_string(),
            ],
        );
        spec.test_configs.push(TestConfigSpec {
            generics: generics(&[("value", &value.to_string())]),
            ..target("lib.tb_pkg")
        });
        let outcome = harness::simulate_all(&runtime, &root.path, &spec).await;
        outcome.assert_outcomes(&harness::outcomes(&[(
            "lib.tb_pkg.all",
            TestOutcome::Passed,
        )]));
    }
}
