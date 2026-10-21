// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! `tests/acceptance/component_binding`: component instantiations bound to an entity in another
//! library.
//!
//! AI NOTICE: Generated, minimally reviewed.

use risim_vunit_frontend::TestOutcome;
use risim_vunit_frontend::spec::ProjectSpec;

use crate::harness;
use crate::harness::RisimDeviations;

/// A testbench in library `top` instantiates the component `cell` of library `cells`, once
/// through a use clause and once as `cells.cells_pkg.cell`.
///
/// The entity `cells.cell` is reached only through the default binding of these instances, so it
/// is compiled only if component resolution looks beyond the testbench's own library. An unbound
/// instance leaves its output at `'U'`, which fails the testbench.
pub async fn component_binding() {
    let runtime = harness::runtime().await;
    let root = harness::Root::new();
    let dir = harness::fixtures_dir().join("component_binding");
    let mut spec = ProjectSpec::new();
    spec.add_library(
        "cells",
        [
            dir.join("cells_pkg.vhd").to_string(),
            dir.join("cell.vhd").to_string(),
        ],
    );
    spec.add_library("top", [dir.join("tb_component_binding.vhd").to_string()]);
    let outcome = harness::simulate_all(&runtime, &root.path, &spec).await;
    outcome.assert_outcomes(
        &harness::outcomes(&[("top.tb_component_binding.all", TestOutcome::Passed)]),
        RisimDeviations::None,
    );
}
