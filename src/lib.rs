// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Rust replacement for the `VUnit` Python frontend, for the riSim simulator.
//!
//! The VHDL libraries under `vunit/vhdl` stay upstream `VUnit` sources.
//! This crate has no command-line interface; event-cache drives it.
//!
//! Implemented so far: reading the configuration ([`config`], [`spec`]), collecting and parsing
//! sources ([`sources`], [`vhdl_parser`]), the builtin libraries ([`builtins`]), the project
//! model with dependency analysis ([`project`], [`dependency_graph`]), test discovery with
//! configurations and testcase patterns ([`discovery`], [`configuration`], [`pattern`]), the
//! `risim-out/` directory ([`store`]), the simulator interface ([`simulator`], [`process`]),
//! incremental parallel compilation ([`compile`]), and simulation runs with persisted results
//! ([`runner`]). The workspace actor is added in a later phase.
//!
//! AI NOTICE: Generated, minimally reviewed.

pub mod builtins;
pub mod compile;
pub mod config;
pub mod configuration;
pub mod dependency_graph;
pub mod diagnostics;
pub mod discovery;
pub mod pattern;
pub mod process;
pub mod project;
pub mod runner;
pub mod simulator;
pub mod sources;
pub mod spec;
pub mod store;
pub mod vhdl_parser;
pub mod vhdl_standard;
