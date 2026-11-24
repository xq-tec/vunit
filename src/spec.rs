// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The programmatic description of a project: libraries, features, options and configurations.
//!
//! This replaces the setters of VUnit's Python API (`ui/__init__.py`, `ui/library.py`,
//! `ui/testbench.py`). A `risim-config.toml` is turned into a [`ProjectSpec`] by the `config`
//! module; tests and other embedders can build one directly.
//!
//! [`Options`] are the options of analysis, of the project and of each library: a library's key
//! replaces the project's, except `defines`, which merge by name ([`Options::for_library`]).
//! The builtin libraries take `relaxed` only ([`Options::for_builtins`]), and a library without
//! SystemVerilog or Verilog files takes `relaxed` and `encoding` only
//! ([`Options::without_sv_keys`]).
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
use crate::simulator::RunOptions;
use crate::simulator::overlay_run_options;
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
    /// The options of the project (`[options]`), over which each library's own options go.
    pub options: Options,
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
            options: Options::default(),
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
        /// The library's own options (`[libraries.NAME.options]`), unmerged; those of the
        /// library's files are [`Options::for_library`] of the project's with them.
        options: Options,
    },
    /// A precompiled library. It is never compiled, only passed to the simulator with `-L`.
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

/// The location of a [`FilePattern`] or an [`IncludeDir`] in a configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PatternLocation {
    /// The configuration file.
    pub file: Utf8PathBuf,
    /// The range of the pattern string.
    pub range: Range,
}

/// The options of the project (`[options]`) or of a library (`[libraries.NAME.options]`).
///
/// They hold what analysis needs; the run options of the same table, such as `opt-level`, are
/// in [`SimOptions::run_options`].
/// Apart from `relaxed` and `encoding`, they are options of SystemVerilog and Verilog files
/// only, which reach `risim analyze` only for libraries with such files
/// ([`Options::without_sv_keys`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Relaxed analysis (`--relaxed`); project level only.
    ///
    /// Elaboration takes the run option `relaxed` instead: a [`ProjectSpec`] built in code sets
    /// that too for relaxed elaboration, as reading `relaxed = true` from `[options]` does.
    pub relaxed: bool,
    /// The directories that a quoted `` `include `` searches (`include-dirs`), in order; a
    /// library's replace the project's.
    pub include_dirs: Option<Vec<IncludeDir>>,
    /// The macros defined at the start of every compilation unit (`defines`), by their names
    /// without formal arguments; a library's go over the project's by name.
    pub defines: BTreeMap<String, Define>,
    /// How the library's files form compilation units (`compilation-unit`).
    pub compilation_unit: Option<CompilationUnit>,
    /// The keywords that the files start with (`keywords`), instead of those of their
    /// extension.
    pub keywords: Option<KeywordVersion>,
    /// The time unit and precision of the design elements that set none
    /// (`default-timescale`), instead of 1ns/1ns.
    pub default_timescale: Option<TimeScale>,
    /// The encoding of the library's files of both languages and of their include files
    /// (`encoding`).
    pub encoding: Option<Encoding>,
    /// The libraries that the library's SystemVerilog files search for packages and
    /// definitions (`sv-libraries`), in order; library level only.
    pub sv_libraries: Option<Vec<String>>,
}

impl Options {
    /// The arguments of `risim analyze` for these options, in their single-argument long forms,
    /// without those of the defaults.
    ///
    /// The order is fixed, so that equal options give equal arguments: `--relaxed`,
    /// `--encoding`, `--keywords`, `--compilation-unit`, `--default-timescale`, each include
    /// directory, each define by name (without the removed ones), and each SystemVerilog
    /// library.
    pub fn analyze_args(&self) -> Vec<String> {
        let Self {
            relaxed,
            include_dirs,
            defines,
            compilation_unit,
            keywords,
            default_timescale,
            encoding,
            sv_libraries,
        } = self;
        let mut args = Vec::new();
        if *relaxed {
            args.push("--relaxed".to_owned());
        }
        if let Some(encoding) = encoding {
            args.push(format!("--encoding={}", encoding.name()));
        }
        if let Some(keywords) = keywords {
            args.push(format!("--keywords={keywords}"));
        }
        if *compilation_unit == Some(CompilationUnit::Library) {
            args.push("--compilation-unit=library".to_owned());
        }
        if let Some(time_scale) = default_timescale {
            args.push(format!("--default-timescale={time_scale}"));
        }
        for directory in include_dirs.iter().flatten() {
            args.push(format!("--include-dir={}", directory.path));
        }
        for (name, define) in defines {
            match define {
                Define::Defined { formals, text } => {
                    let formals = formals.as_deref().unwrap_or_default();
                    if text.is_empty() {
                        args.push(format!("--define={name}{formals}"));
                    } else {
                        args.push(format!("--define={name}{formals}={text}"));
                    }
                },
                Define::Removed => {},
            }
        }
        for library in sv_libraries.iter().flatten() {
            args.push(format!("--sv-library={library}"));
        }
        args
    }

    /// The options of a library whose own options are `library`, with these as the project's.
    ///
    /// A key of the library replaces the project's, except `defines`, which go over the
    /// project's by name, where [`Define::Removed`] drops the project's define of that name.
    /// `relaxed` is the project's, and `sv_libraries` the library's, since each is set at one
    /// level only.
    #[must_use]
    pub fn for_library(&self, library: &Self) -> Self {
        let Self {
            relaxed: _,
            include_dirs,
            defines,
            compilation_unit,
            keywords,
            default_timescale,
            encoding,
            sv_libraries,
        } = library;
        let mut merged = self.defines.clone();
        for (name, define) in defines {
            match define {
                Define::Defined { .. } => {
                    merged.insert(name.clone(), define.clone());
                },
                Define::Removed => {
                    merged.remove(name);
                },
            }
        }
        Self {
            relaxed: self.relaxed,
            include_dirs: include_dirs.clone().or_else(|| self.include_dirs.clone()),
            defines: merged,
            compilation_unit: compilation_unit.or(self.compilation_unit),
            keywords: keywords.or(self.keywords),
            default_timescale: default_timescale.or(self.default_timescale),
            encoding: encoding.or(self.encoding),
            sv_libraries: sv_libraries.clone(),
        }
    }

    /// The options of the builtin libraries (VUnit's and OSVVM) with these as the project's:
    /// `relaxed` only.
    #[must_use]
    pub fn for_builtins(&self) -> Self {
        Self {
            relaxed: self.relaxed,
            ..Self::default()
        }
    }

    /// These options without the keys of SystemVerilog and Verilog files, such as `keywords`
    /// and `default-timescale`: `relaxed` and `encoding` only.
    ///
    /// A library without such files is analyzed with these, so that its arguments, and with
    /// them its compile keys, don't depend on the other keys.
    #[must_use]
    pub fn without_sv_keys(&self) -> Self {
        Self {
            relaxed: self.relaxed,
            encoding: self.encoding,
            ..Self::default()
        }
    }
}

/// A directory of `include-dirs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeDir {
    /// The directory, as written, relative to the workspace root, or resolved against it in the
    /// options of a collected library.
    pub path: Utf8PathBuf,
    /// Where the directory is written in the configuration file, for diagnostics.
    pub location: Option<PatternLocation>,
}

impl IncludeDir {
    /// Creates a directory that isn't from a configuration file.
    pub fn new(path: impl Into<Utf8PathBuf>) -> Self {
        Self {
            path: path.into(),
            location: None,
        }
    }
}

/// A macro of `defines`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Define {
    /// A defined macro: `true` or `""` for an empty one, or its text.
    Defined {
        /// The formal arguments in their parentheses, such as `(a,b)`, if the macro has them.
        formals: Option<String>,
        /// The text of the macro, empty for an empty one.
        text: String,
    },
    /// `false` in a library: the project's macro of the name isn't defined for it.
    Removed,
}

impl Define {
    /// Creates a macro without formal arguments, with the text `text`.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Defined {
            formals: None,
            text: text.into(),
        }
    }
}

/// How the SystemVerilog files of a library form compilation units (`compilation-unit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompilationUnit {
    /// Each file is a compilation unit of its own; the default.
    File,
    /// The files of the library form one compilation unit, in the order of collection.
    Library,
}

impl CompilationUnit {
    /// All modes.
    pub const ALL: [Self; 2] = [Self::File, Self::Library];

    /// The name used in configuration files and by `--compilation-unit`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Library => "library",
        }
    }
}

/// The encoding of source files (`encoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// UTF-8, the default of SystemVerilog and Verilog files.
    Utf8,
    /// ISO 8859-1, the default of VHDL files.
    Latin1,
}

impl Encoding {
    /// All encodings.
    pub const ALL: [Self; 2] = [Self::Utf8, Self::Latin1];

    /// The name used in configuration files and by `--encoding`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Latin1 => "latin-1",
        }
    }
}

/// The version specifiers of `` `begin_keywords `` (IEEE 1800 22.14), the values of `keywords`.
pub const KEYWORD_VERSIONS: [&str; 9] = [
    "1364-1995",
    "1364-2001",
    "1364-2001-noconfig",
    "1364-2005",
    "1800-2005",
    "1800-2009",
    "1800-2012",
    "1800-2017",
    "1800-2023",
];

/// The directives of IEEE 1800-2023 clause 22, as 22.1 lists them, without `` ` ``; no macro
/// of `defines` may be named after one (22.5.1).
///
/// A copy of hydrolyze's list (`hydrolyze::is_sv_directive`), since this crate builds without
/// hydrolyze; riSim's tests check that the two agree.
pub const SV_DIRECTIVES: [&str; 22] = [
    "__FILE__",
    "__LINE__",
    "begin_keywords",
    "celldefine",
    "default_nettype",
    "define",
    "else",
    "elsif",
    "end_keywords",
    "endcelldefine",
    "endif",
    "ifdef",
    "ifndef",
    "include",
    "line",
    "nounconnected_drive",
    "pragma",
    "resetall",
    "timescale",
    "unconnected_drive",
    "undef",
    "undefineall",
];

/// A version of `keywords`, one of [`KEYWORD_VERSIONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeywordVersion(&'static str);

impl KeywordVersion {
    /// The version specifier, such as `1800-2017`.
    pub const fn text(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for KeywordVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A string that isn't one of [`KEYWORD_VERSIONS`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown keyword version '{0}'")]
pub struct UnknownKeywordVersion(pub String);

impl FromStr for KeywordVersion {
    type Err = UnknownKeywordVersion;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        KEYWORD_VERSIONS
            .into_iter()
            .find(|version| *version == text)
            .map(Self)
            .ok_or_else(|| UnknownKeywordVersion(text.to_owned()))
    }
}

/// The time units of `` `timescale `` (IEEE 1800 22.7), from the coarsest.
pub const TIME_UNITS: [&str; 6] = ["s", "ms", "us", "ns", "ps", "fs"];

/// A time unit and precision of `default-timescale`, such as `1ns/1ps`: each 1, 10, or 100 of
/// one of [`TIME_UNITS`], the precision not coarser than the unit (IEEE 1800 3.14, 22.7).
///
/// [`FromStr`] takes the arguments of `` `timescale ``, also with spaces between number and
/// unit and around the `/`, as in `1 ns / 1 ps`; [`Display`](fmt::Display) writes the
/// normalized form without spaces, which `--default-timescale` gets.
/// It is a copy of hydrolyze's grammar (`hydrolyze::TimeScale`), since this crate builds
/// without hydrolyze; riSim's tests check that the two agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeScale {
    /// The time unit.
    unit: TimeValue,
    /// The time precision.
    precision: TimeValue,
}

/// A time unit or precision: a magnitude of 1, 10, or 100 and a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TimeValue {
    /// 1, 10, or 100.
    magnitude: u8,
    /// The index of the unit in [`TIME_UNITS`].
    unit: u8,
}

impl TimeValue {
    /// Parses a time value with optional spaces around it and between number and unit, such
    /// as ` 10 ns`.
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let split = text.find(|char: char| char.is_ascii_alphabetic())?;
        let (magnitude, unit) = text.split_at(split);
        let magnitude = match magnitude.trim_end() {
            "1" => 1,
            "10" => 10,
            "100" => 100,
            _ => return None,
        };
        let unit = TIME_UNITS.iter().position(|known| *known == unit)?;
        Some(Self {
            magnitude,
            unit: u8::try_from(unit).ok()?,
        })
    }

    /// The power of ten of the value in seconds, such as -8 for `10ns`.
    fn exponent(self) -> i32 {
        let digits = match self.magnitude {
            100 => 2,
            10 => 1,
            _ => 0,
        };
        // the units are a factor of 1000 apart, from 1 s
        digits - 3 * i32::from(self.unit)
    }
}

impl fmt::Display for TimeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.magnitude,
            TIME_UNITS[usize::from(self.unit)]
        )
    }
}

impl fmt::Display for TimeScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.unit, self.precision)
    }
}

/// A string that isn't a [`TimeScale`]: no time unit and precision as `` `timescale `` takes
/// them, or a precision coarser than the unit.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid time scale '{0}'")]
pub struct InvalidTimeScale(pub String);

impl FromStr for TimeScale {
    type Err = InvalidTimeScale;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidTimeScale(text.to_owned());
        let (unit, precision) = text.split_once('/').ok_or_else(invalid)?;
        let unit = TimeValue::parse(unit).ok_or_else(invalid)?;
        let precision = TimeValue::parse(precision).ok_or_else(invalid)?;
        if precision.exponent() > unit.exponent() {
            return Err(invalid());
        }
        Ok(Self { unit, precision })
    }
}

/// Options for elaboration and simulation (`risim-runner run`).
///
/// A test configuration overrides the project, and still-unset fields take their defaults when
/// the command line is built: no run options, assert stop level `error`, IEEE assertions left
/// enabled, and a new seed per run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimOptions {
    /// Extra options of the runner, keys of its JSON object such as `relaxed` or `opt-level`,
    /// which override the runtime's defaults.
    ///
    /// They can't set the keys that every simulation sets
    /// ([`SIMULATION_KEYS`](crate::simulator::SIMULATION_KEYS)); those of a configuration go
    /// over those of the project by the layer rule of [`overlay_run_options`].
    pub run_options: Option<RunOptions>,
    /// The lowest assertion severity that stops the simulation; default `error`.
    pub vhdl_assert_stop_level: Option<AssertLevel>,
    /// Whether to disable assertions from the IEEE libraries; default `false`.
    pub disable_ieee_warnings: Option<bool>,
    /// The seed; default is a new random seed for every run.
    pub seed: Option<String>,
}

impl SimOptions {
    /// Returns these options with every field that `overrides` sets replaced, except the run
    /// options, where those of `overrides` go over these by [`overlay_run_options`].
    #[must_use]
    pub fn overridden_by(&self, overrides: &Self) -> Self {
        let run_options = match (&self.run_options, &overrides.run_options) {
            (Some(earlier), Some(layer)) => {
                let mut options = earlier.clone();
                overlay_run_options(&mut options, layer);
                Some(options)
            },
            (earlier, layer) => layer.clone().or_else(|| earlier.clone()),
        };
        Self {
            run_options,
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
    /// The name used in configuration files and by the run option `assert-level`.
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
        let relaxed = RunOptions::from_iter([("relaxed".to_owned(), true.into())]);
        let base = SimOptions {
            run_options: Some(relaxed),
            seed: Some("1".to_owned()),
            ..SimOptions::default()
        };
        let overrides = SimOptions {
            disable_ieee_warnings: Some(true),
            seed: Some("2".to_owned()),
            ..SimOptions::default()
        };
        let merged = base.overridden_by(&overrides);
        assert_eq!(merged.run_options, base.run_options);
        assert_eq!(merged.seed.as_deref(), Some("2"));
        assert_eq!(merged.disable_ieee_warnings, Some(true));
        assert_eq!(merged.vhdl_assert_stop_level, None);

        // A configuration's run options go over the project's by the layer rule: its
        // `opt-level` drops the project's `opt.*` keys.
        let project = SimOptions {
            run_options: Some(RunOptions::from_iter([
                ("relaxed".to_owned(), true.into()),
                ("opt.wake-guards".to_owned(), "off".into()),
            ])),
            ..SimOptions::default()
        };
        let configuration = SimOptions {
            run_options: Some(RunOptions::from_iter([
                ("finish-after".to_owned(), "1 ms".into()),
                ("opt-level".to_owned(), "on".into()),
            ])),
            ..SimOptions::default()
        };
        assert_eq!(
            project.overridden_by(&configuration).run_options,
            Some(RunOptions::from_iter([
                ("relaxed".to_owned(), true.into()),
                ("finish-after".to_owned(), "1 ms".into()),
                ("opt-level".to_owned(), "on".into()),
            ]))
        );
        assert_eq!(
            SimOptions::default()
                .overridden_by(&configuration)
                .run_options,
            configuration.run_options
        );
    }

    fn relaxed() -> Options {
        Options {
            relaxed: true,
            ..Options::default()
        }
    }

    /// Options with every key set, `library` level ones included.
    fn every_key() -> Options {
        Options {
            relaxed: true,
            include_dirs: Some(vec![IncludeDir::new("/inc/b"), IncludeDir::new("/inc/a")]),
            defines: BTreeMap::from([
                ("WIDTH".to_owned(), Define::text("8")),
                ("DEBUG".to_owned(), Define::text("")),
                (
                    "MAX".to_owned(),
                    Define::Defined {
                        formals: Some("(a,b)".to_owned()),
                        text: "((a)>(b)?(a):(b))".to_owned(),
                    },
                ),
                ("GONE".to_owned(), Define::Removed),
            ]),
            compilation_unit: Some(CompilationUnit::Library),
            keywords: Some("1800-2017".parse().unwrap()),
            default_timescale: Some("10ns/1ps".parse().unwrap()),
            encoding: Some(Encoding::Latin1),
            sv_libraries: Some(vec!["b".to_owned(), "a".to_owned()]),
        }
    }

    #[test]
    fn options_become_analyze_args() {
        assert_eq!(Options::default().analyze_args(), Vec::<String>::new());
        assert_eq!(relaxed().analyze_args(), ["--relaxed"]);
        assert_eq!(
            every_key().analyze_args(),
            [
                "--relaxed",
                "--encoding=latin-1",
                "--keywords=1800-2017",
                "--compilation-unit=library",
                "--default-timescale=10ns/1ps",
                "--include-dir=/inc/b",
                "--include-dir=/inc/a",
                "--define=DEBUG",
                "--define=MAX(a,b)=((a)>(b)?(a):(b))",
                "--define=WIDTH=8",
                "--sv-library=b",
                "--sv-library=a",
            ]
        );
        // The defaults are omitted.
        let defaults = Options {
            include_dirs: Some(Vec::new()),
            compilation_unit: Some(CompilationUnit::File),
            sv_libraries: Some(Vec::new()),
            ..Options::default()
        };
        assert_eq!(defaults.analyze_args(), Vec::<String>::new());
    }

    #[test]
    fn libraries_and_builtins_keep_relaxed() {
        assert_eq!(relaxed().for_builtins(), relaxed());
        assert_eq!(every_key().for_builtins(), relaxed());
        assert_eq!(relaxed().for_library(&Options::default()), relaxed());
        assert_eq!(
            Options::default().for_library(&Options::default()),
            Options::default()
        );
    }

    #[test]
    fn library_keys_go_over_the_project() {
        let project = Options {
            relaxed: true,
            include_dirs: Some(vec![IncludeDir::new("project")]),
            defines: BTreeMap::from([
                ("WIDTH".to_owned(), Define::text("8")),
                ("DEBUG".to_owned(), Define::text("")),
                ("KEEP".to_owned(), Define::text("1")),
            ]),
            compilation_unit: Some(CompilationUnit::Library),
            keywords: Some("1800-2017".parse().unwrap()),
            default_timescale: Some("1ns/1ps".parse().unwrap()),
            encoding: Some(Encoding::Latin1),
            sv_libraries: None,
        };

        // Without keys of its own, a library takes the project's.
        assert_eq!(project.for_library(&Options::default()), project);

        let library = Options {
            include_dirs: Some(vec![IncludeDir::new("library")]),
            defines: BTreeMap::from([
                ("WIDTH".to_owned(), Define::text("16")),
                ("DEBUG".to_owned(), Define::Removed),
                ("UNKNOWN".to_owned(), Define::Removed),
                ("NEW".to_owned(), Define::text("")),
            ]),
            keywords: Some("1364-2005".parse().unwrap()),
            default_timescale: Some("10ns/10ps".parse().unwrap()),
            sv_libraries: Some(vec!["other".to_owned()]),
            ..Options::default()
        };
        assert_eq!(
            project.for_library(&library),
            Options {
                relaxed: true,
                include_dirs: Some(vec![IncludeDir::new("library")]),
                defines: BTreeMap::from([
                    ("WIDTH".to_owned(), Define::text("16")),
                    ("KEEP".to_owned(), Define::text("1")),
                    ("NEW".to_owned(), Define::text("")),
                ]),
                compilation_unit: Some(CompilationUnit::Library),
                keywords: Some("1364-2005".parse().unwrap()),
                default_timescale: Some("10ns/10ps".parse().unwrap()),
                encoding: Some(Encoding::Latin1),
                sv_libraries: Some(vec!["other".to_owned()]),
            }
        );

        // `relaxed` stays the project's.
        assert!(!Options::default().for_library(&relaxed()).relaxed);
    }

    #[test]
    fn without_sv_keys_keeps_relaxed_and_encoding() {
        assert_eq!(
            every_key().without_sv_keys(),
            Options {
                relaxed: true,
                encoding: Some(Encoding::Latin1),
                ..Options::default()
            }
        );
        assert_eq!(
            every_key().without_sv_keys().analyze_args(),
            ["--relaxed", "--encoding=latin-1"]
        );
    }

    #[test]
    fn keyword_versions_parse() {
        for version in KEYWORD_VERSIONS {
            assert_eq!(version.parse::<KeywordVersion>().unwrap().text(), version);
        }
        assert_eq!(
            "1800-2020".parse::<KeywordVersion>(),
            Err(UnknownKeywordVersion("1800-2020".to_owned()))
        );
    }

    #[test]
    fn time_scales_parse_and_normalize() {
        for (text, normalized) in [
            ("1ns/1ps", "1ns/1ps"),
            ("1 ns / 1 ps", "1ns/1ps"),
            (" 100us/10fs ", "100us/10fs"),
            ("10ms/10ms", "10ms/10ms"),
            ("1s/1fs", "1s/1fs"),
            ("100ps/1ps", "100ps/1ps"),
        ] {
            assert_eq!(
                text.parse::<TimeScale>().unwrap().to_string(),
                normalized,
                "{text}"
            );
        }
        for invalid in [
            "",
            "1ns",
            "1ns/",
            "/1ns",
            "2ns/1ns",
            "1000ps/1ps",
            "1.0ns/1ps",
            "1ns/1us",
            "10ns/100ns",
            "1 0ns/1ns",
            "1NS/1ps",
            "1ns/1ps/1fs",
            "1min/1s",
        ] {
            assert_eq!(
                invalid.parse::<TimeScale>(),
                Err(InvalidTimeScale(invalid.to_owned())),
                "{invalid}"
            );
        }
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
