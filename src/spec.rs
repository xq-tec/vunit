// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The programmatic description of a project: libraries, features, options and configurations.
//!
//! This replaces the setters of VUnit's Python API (`ui/__init__.py`, `ui/library.py`,
//! `ui/testbench.py`). A `risim-config.toml` is turned into a [`ProjectSpec`] by the `config`
//! module; tests and other embedders can build one directly.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use camino::Utf8PathBuf;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::diagnostics::Range;
use crate::vhdl_standard::VhdlStandard;

/// The whole project: libraries and their sources, options and test configurations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSpec {
    /// The standard for all files that don't override it.
    pub vhdl_standard: VhdlStandard,
    /// Optional VUnit features. The VUnit core libraries, com and OSVVM are always added.
    pub features: BTreeSet<Feature>,
    /// User libraries, in the order they are added to the project.
    pub libraries: Vec<LibrarySpec>,
    /// Options for analysis.
    pub compile_options: CompileOptions,
    /// Options for elaboration and simulation.
    pub sim_options: SimOptions,
    /// Configurations of testbenches and tests.
    pub test_configs: Vec<TestConfigSpec>,
}

impl ProjectSpec {
    /// Creates an empty VHDL-2008 project.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a library with sources matching `patterns`.
    pub fn add_library<P: Into<String>>(
        &mut self,
        name: impl Into<String>,
        patterns: impl IntoIterator<Item = P>,
    ) -> &mut Self {
        self.libraries.push(LibrarySpec::Sources {
            name: name.into(),
            files: patterns.into_iter().map(FilePattern::new).collect(),
            vhdl_standard: None,
        });
        self
    }

    /// Adds a precompiled library.
    pub fn add_external_library(
        &mut self,
        name: impl Into<String>,
        path: impl Into<Utf8PathBuf>,
    ) -> &mut Self {
        self.libraries.push(LibrarySpec::External {
            name: name.into(),
            path: path.into(),
        });
        self
    }

    /// Enables an optional VUnit feature.
    pub fn add_feature(&mut self, feature: Feature) -> &mut Self {
        self.features.insert(feature);
        self
    }
}

/// An optional part of VUnit, on top of the core libraries, com and OSVVM.
///
/// `array_util` and `json4vhdl` don't exist anymore in VUnit 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// The random package (`vunit_lib.random_pkg`), which needs OSVVM.
    Random,
    /// The verification component library, which needs com and OSVVM.
    VerificationComponents,
}

impl Feature {
    /// All features.
    pub const ALL: [Self; 2] = [Self::Random, Self::VerificationComponents];

    /// The name used in configuration files.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::VerificationComponents => "verification_components",
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A string that doesn't name a [`Feature`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown VUnit feature '{0}'")]
pub struct UnknownFeature(pub String);

impl FromStr for Feature {
    type Err = UnknownFeature;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|feature| feature.name() == name)
            .ok_or_else(|| UnknownFeature(name.to_owned()))
    }
}

/// A library of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibrarySpec {
    /// A library compiled from source files. For `vunit_lib`, the files are added to the
    /// VUnit library.
    Sources {
        /// The library name.
        name: String,
        /// Patterns of the source files.
        files: Vec<FilePattern>,
        /// The standard of the library's files, if it differs from the project's.
        vhdl_standard: Option<VhdlStandard>,
    },
    /// A precompiled library. It is never compiled, only passed to the simulator with `-P`.
    External {
        /// The library name.
        name: String,
        /// The directory of the compiled library.
        path: Utf8PathBuf,
    },
}

impl LibrarySpec {
    /// The library name.
    pub fn name(&self) -> &str {
        match self {
            Self::Sources { name, .. } | Self::External { name, .. } => name,
        }
    }
}

/// A glob pattern for source files.
///
/// Relative patterns are resolved against the workspace root. Supported syntax: `*`, `?`,
/// `[…]` and `**` for any number of directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePattern {
    /// The pattern as written.
    pub pattern: String,
    /// Where the pattern is written in the configuration file, for diagnostics.
    pub location: Option<PatternLocation>,
}

impl FilePattern {
    /// Creates a pattern that isn't from a configuration file.
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            location: None,
        }
    }
}

/// The location of a [`FilePattern`] in a configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternLocation {
    /// The configuration file.
    pub file: Utf8PathBuf,
    /// The range of the pattern string.
    pub range: Range,
}

/// Options for analysis (`risim-ghdl -a`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompileOptions {
    /// Extra analysis flags (`risim-ghdl.a_flags`).
    pub a_flags: Vec<String>,
}

/// Options for elaboration and simulation (`risim-ghdl --elab-run`).
///
/// A test configuration overrides the project, and still-unset fields take their defaults when
/// the command line is built: empty flag lists, assert stop level `error`, IEEE assertions left
/// enabled, and a new seed per run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimOptions {
    /// Extra elaboration flags (`risim-ghdl.elab_flags`), passed to risim-ghdl by both backends.
    pub elab_flags: Option<Vec<String>>,
    /// Extra simulation flags (`risim-ghdl.sim_flags`), passed to the simulator of the backend:
    /// GHDL runtime options for `ghdl`, risim-runner options for `risim`.
    pub sim_flags: Option<Vec<String>>,
    /// The lowest assertion severity that stops the simulation; default `error`.
    pub vhdl_assert_stop_level: Option<AssertLevel>,
    /// Whether to disable assertions from the IEEE libraries; default `false`.
    pub disable_ieee_warnings: Option<bool>,
    /// The seed; default is a new random seed for every run.
    pub seed: Option<String>,
}

impl SimOptions {
    /// Returns these options with every field that `overrides` sets replaced.
    #[must_use]
    pub fn overridden_by(&self, overrides: &Self) -> Self {
        Self {
            elab_flags: overrides
                .elab_flags
                .clone()
                .or_else(|| self.elab_flags.clone()),
            sim_flags: overrides
                .sim_flags
                .clone()
                .or_else(|| self.sim_flags.clone()),
            vhdl_assert_stop_level: overrides
                .vhdl_assert_stop_level
                .or(self.vhdl_assert_stop_level),
            disable_ieee_warnings: overrides
                .disable_ieee_warnings
                .or(self.disable_ieee_warnings),
            seed: overrides.seed.clone().or_else(|| self.seed.clone()),
        }
    }
}

/// The assertion severity at which a simulation stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertLevel {
    /// Stop on warnings and above.
    Warning,
    /// Stop on errors and failures.
    Error,
    /// Stop on failures only.
    Failure,
}

impl AssertLevel {
    /// The name used in configuration files and by GHDL's `--assert-level`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Failure => "failure",
        }
    }
}

impl fmt::Display for AssertLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Configurations of a testbench (`lib.tb`) or of one of its tests (`lib.tb.test`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestConfigSpec {
    /// `lib.tb` or `lib.tb.test`.
    pub target: String,
    /// Named configurations; if there are any, the default configuration isn't run.
    pub configurations: Vec<ConfigurationSpec>,
    /// Generics set in all configurations of the target before `configurations` are added, so
    /// the added configurations inherit them.
    pub generics: BTreeMap<String, String>,
    /// Simulation options set in all configurations of the target, like `generics`.
    pub sim_options: SimOptions,
    /// A VHDL configuration to elaborate in all configurations of the target, like `generics`.
    pub vhdl_configuration_name: Option<String>,
}

/// A named configuration of a testbench or test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationSpec {
    /// The configuration name, which becomes part of the testcase names.
    pub name: String,
    /// Generic values.
    pub generics: BTreeMap<String, String>,
    /// Simulation options on top of the project's.
    pub sim_options: SimOptions,
    /// User attributes (names starting with `.`) and their values. The builtin attribute
    /// `fail_on_warning` sets the assert stop level to warning, or to error if its value is
    /// `false`; `run_all_in_same_sim` is ignored.
    pub attributes: BTreeMap<String, String>,
    /// A VHDL configuration to elaborate instead of the testbench entity.
    pub vhdl_configuration_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_names_round_trip() {
        for feature in Feature::ALL {
            assert_eq!(feature.name().parse(), Ok(feature));
        }
        assert_eq!(
            "array_util".parse::<Feature>(),
            Err(UnknownFeature("array_util".to_owned()))
        );
    }

    #[test]
    fn sim_options_override_set_fields() {
        let base = SimOptions {
            elab_flags: Some(vec!["-fsynopsys".to_owned()]),
            seed: Some("1".to_owned()),
            ..SimOptions::default()
        };
        let overrides = SimOptions {
            disable_ieee_warnings: Some(true),
            seed: Some("2".to_owned()),
            ..SimOptions::default()
        };
        let merged = base.overridden_by(&overrides);
        assert_eq!(merged.elab_flags, base.elab_flags);
        assert_eq!(merged.seed.as_deref(), Some("2"));
        assert_eq!(merged.disable_ieee_warnings, Some(true));
        assert_eq!(merged.vhdl_assert_stop_level, None);
    }

    #[test]
    fn builder_adds_libraries_in_order() {
        let mut spec = ProjectSpec::new();
        spec.add_library("b", ["b/*.vhd"])
            .add_library("a", ["a/*.vhd"])
            .add_external_library("ext", "/ext")
            .add_feature(Feature::Random);
        let names: Vec<_> = spec.libraries.iter().map(LibrarySpec::name).collect();
        assert_eq!(names, ["b", "a", "ext"]);
        assert!(spec.features.contains(&Feature::Random));
        assert_eq!(spec.vhdl_standard, VhdlStandard::Vhdl2008);
    }
}
