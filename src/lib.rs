// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Rust replacement for the `VUnit` Python frontend, for the riSim simulator.
//!
//! The VHDL libraries under `vunit/vhdl` stay upstream `VUnit` sources.
//! This crate has no command-line interface; event-cache drives it.
//! Parsing, discovery, and compile/simulate orchestration are added in later phases.
//!
//! AI NOTICE: Generated, minimally reviewed.
