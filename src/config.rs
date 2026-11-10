// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Reading `risim-config.toml` into a [`ProjectSpec`].
//!
//! ```toml
//! # Options of `risim analyze` and of the simulations: `--relaxed`, or its alias `-frelaxed`, and
//! # `-fsynopsys`, which has no effect.
//! options = ["-frelaxed"]
//!
//! # Options of the simulations: the optimization preset, and rows or groups of optimizations
//! # over it, where dotted names are quoted.
//! [run]
//! opt-level = "full"
//!
//! [run.opt]
//! wake-guards = "off"
//! "mir.cse" = "off"
//!
//! # Optional VUnit features on top of the defaults (VUnit core, com, OSVVM).
//! [vunit]
//! features = ["random", "verification_components"]
//!
//! # One table per library, with source globs relative to the workspace root.
//! [libraries.my_lib]
//! files = ["src/**/*.vhd", "tb/*.vhd"]
//!
//! # The riSim toolchain that the launcher runs for the workspace; ignored here.
//! [toolchain]
//! version = "1.4"
//! ```
//!
//! `[run]` becomes the run options `opt-level` and `opt.<name>` of every simulation
//! ([`SimOptions::run_options`]), next to `relaxed` from `options`; the run options of a
//! configuration go over them ([`SimOptions::overridden_by`]).
//! A table in `[run.opt]`, such as `[run.opt.mir]`, is an error that names the quoted form.
//!
//! Problems become diagnostics with ranges in the file. Only syntax and type errors prevent a
//! [`ProjectSpec`]; invalid libraries, features and options are left out of it, also the run
//! options that the check of [`load`] rejects.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use thiserror::Error;
use toml::Spanned;
use toml::de::DeTable;
use toml::de::DeValue;

use crate::diagnostics::Diagnostic;
use crate::diagnostics::LineIndex;
use crate::diagnostics::Severity;
use crate::project::LibraryNames;
use crate::simulator::CheckRunOptions;
use crate::simulator::RunOptions;
use crate::spec::CompileOptions;
use crate::spec::Feature;
use crate::spec::FilePattern;
use crate::spec::LibrarySpec;
use crate::spec::PatternLocation;
use crate::spec::ProjectSpec;
use crate::spec::SimOptions;

/// The result of reading a configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The project, unless the file has syntax or type errors.
    pub spec: Option<ProjectSpec>,
    /// Errors and warnings about the file.
    pub diagnostics: Vec<Diagnostic>,
}

/// The configuration file can't be read at all.
#[derive(Debug, Error)]
#[error("failed to read {path}")]
pub struct ReadError {
    /// The configuration file.
    pub path: Utf8PathBuf,
    /// The underlying error.
    #[source]
    pub source: io::Error,
}

/// Reads and parses the configuration file at `path`, and checks the run options of its `[run]`
/// table with `check_run_options`, one key at a time.
///
/// # Errors
///
/// Fails only if the file can't be read; problems with its contents, including invalid UTF-8,
/// are diagnostics.
pub fn load(
    path: &Utf8Path,
    check_run_options: Option<CheckRunOptions>,
) -> Result<Config, ReadError> {
    let content = fs::read(path).map_err(|source| ReadError {
        path: path.to_owned(),
        source,
    })?;
    Ok(match str::from_utf8(&content) {
        Ok(content) => parse(path, content, check_run_options),
        Err(error) => {
            let offset = error.valid_up_to();
            let range = LineIndex::new(&content).range(offset..offset);
            Config {
                spec: None,
                diagnostics: vec![
                    Diagnostic::error("the file isn't valid UTF-8")
                        .in_file(path)
                        .at(Some(range)),
                ],
            }
        },
    })
}

/// Parses `content`, the configuration file at `path`, as [`load`] does.
pub fn parse(path: &Utf8Path, content: &str, check_run_options: Option<CheckRunOptions>) -> Config {
    let mut parser = Parser {
        path,
        lines: LineIndex::new(content.as_bytes()),
        diagnostics: Vec::new(),
        check_run_options,
    };
    let spec = parser.parse(content);
    Config {
        spec,
        diagnostics: parser.diagnostics,
    }
}

/// The options of analysis and simulation.
///
/// `risim analyze` takes them as they are; a simulation gets the run option `relaxed` from
/// `--relaxed` and `-frelaxed`, and nothing from `-fsynopsys`.
const OPTIONS: [&str; 3] = ["--relaxed", "-frelaxed", "-fsynopsys"];

/// The options of `OPTIONS` that make the simulations relaxed.
const RELAXED: [&str; 2] = ["--relaxed", "-frelaxed"];

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default)]
    options: Vec<Spanned<String>>,
    #[serde(default)]
    vunit: RawVunit,
    #[serde(default)]
    libraries: BTreeMap<String, RawLibrary>,
    #[serde(default)]
    run: RawRun,
    /// Read by the riSim launcher, which selects the toolchain by it; accepted here so that it
    /// isn't an unknown key.
    #[serde(default)]
    #[expect(dead_code, reason = "only the launcher uses the toolchain version")]
    toolchain: Option<RawToolchain>,
}

/// The `[toolchain]` table.
#[derive(Deserialize)]
struct RawToolchain {
    /// The toolchain's version, version prefix or `latest`.
    #[expect(dead_code, reason = "only the launcher uses the toolchain version")]
    version: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawVunit {
    #[serde(default)]
    features: Vec<Spanned<String>>,
}

/// The `[run]` table.
#[derive(Default, Deserialize)]
struct RawRun {
    /// The optimization preset, the run option `opt-level`: a name, or the number of one; any
    /// value, so that another type is an error at the value.
    #[serde(rename = "opt-level")]
    opt_level: Option<Spanned<toml::Value>>,
    /// The modes of rows and groups, the run options `opt.<name>`; any value, so that a table
    /// gets the error that names the quoted form.
    #[serde(default)]
    opt: BTreeMap<String, Spanned<toml::Value>>,
}

#[derive(Deserialize)]
struct RawLibrary {
    files: Vec<Spanned<String>>,
}

/// A step in the path of an ignored key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Key(String),
    Index(usize),
}

struct Parser<'config> {
    path: &'config Utf8Path,
    lines: LineIndex<'config>,
    diagnostics: Vec<Diagnostic>,
    check_run_options: Option<CheckRunOptions>,
}

impl Parser<'_> {
    fn diagnostic(&mut self, severity: Severity, message: String, span: Option<ops::Range<usize>>) {
        self.diagnostics.push(
            Diagnostic::new(severity, message)
                .in_file(self.path)
                .at(span.map(|span| self.lines.range(span))),
        );
    }

    fn parse(&mut self, content: &str) -> Option<ProjectSpec> {
        let table = match DeTable::parse(content) {
            Ok(table) => table,
            Err(error) => {
                self.diagnostic(Severity::Error, error.message().to_owned(), error.span());
                return None;
            },
        };

        let mut ignored = Vec::new();
        let raw: RawConfig =
            match serde_ignored::deserialize(toml::Deserializer::from(table.clone()), |path| {
                ignored.push(path_segments(&path));
            }) {
                Ok(raw) => raw,
                Err(error) => {
                    self.diagnostic(Severity::Error, error.message().to_owned(), error.span());
                    return None;
                },
            };
        // The table iterates keys by name; report them in file order.
        let mut ignored: Vec<_> = ignored
            .into_iter()
            .map(|path| (key_span(table.get_ref(), &path), path))
            .collect();
        ignored.sort_by_key(|(span, _)| span.as_ref().map(|span| span.start));
        for (span, path) in ignored {
            let name = path
                .iter()
                .map(|segment| match segment {
                    PathSegment::Key(key) => key.clone(),
                    PathSegment::Index(index) => index.to_string(),
                })
                .collect::<Vec<_>>()
                .join(".");
            self.diagnostic(
                Severity::Warning,
                format!("unknown key '{name}' is ignored"),
                span,
            );
        }

        Some(self.build_spec(raw, table.get_ref()))
    }

    fn build_spec(&mut self, raw: RawConfig, table: &DeTable<'_>) -> ProjectSpec {
        let mut spec = ProjectSpec::new();

        for feature in raw.vunit.features {
            match feature.get_ref().parse::<Feature>() {
                Ok(parsed) => {
                    spec.features.insert(parsed);
                },
                Err(error) => {
                    let message = format!(
                        "{error}; expected one of: {}",
                        Feature::ALL.map(Feature::name).join(", ")
                    );
                    self.diagnostic(Severity::Error, message, Some(feature.span()));
                },
            }
        }

        // Keep the libraries in the order of the file; the map sorts them by name.
        let mut libraries: Vec<_> = raw
            .libraries
            .into_iter()
            .map(|(name, library)| {
                let span = key_span(
                    table,
                    &[
                        PathSegment::Key("libraries".to_owned()),
                        PathSegment::Key(name.clone()),
                    ],
                );
                (span, name, library)
            })
            .collect();
        libraries.sort_by_key(|(span, _, _)| span.as_ref().map(|span| span.start));
        let mut names = LibraryNames::new();
        for (span, name, library) in libraries {
            if let Err(error) = names.insert(&name) {
                self.diagnostic(Severity::Error, error.to_string(), span);
                continue;
            }

            let files = library
                .files
                .into_iter()
                .map(|pattern| {
                    let range = self.lines.range(pattern.span());
                    FilePattern {
                        pattern: pattern.into_inner(),
                        location: Some(PatternLocation {
                            file: self.path.to_owned(),
                            range,
                        }),
                    }
                })
                .collect();
            spec.libraries.push(LibrarySpec::Sources {
                name,
                files,
                vhdl_standard: None,
            });
        }

        let mut options = Vec::new();
        for option in raw.options {
            if OPTIONS.contains(&option.get_ref().as_str()) {
                options.push(option.into_inner());
            } else {
                let message = format!(
                    "unsupported option '{}'; expected one of: {}",
                    option.get_ref(),
                    OPTIONS.join(", ")
                );
                self.diagnostic(Severity::Error, message, Some(option.span()));
            }
        }
        let mut run_options = RunOptions::new();
        if options
            .iter()
            .any(|option| RELAXED.contains(&option.as_str()))
        {
            run_options.insert("relaxed".to_owned(), true.into());
        }
        self.run_options(raw.run, table, &mut run_options);
        spec.compile_options = CompileOptions { a_flags: options };
        spec.sim_options = SimOptions {
            run_options: Some(run_options),
            ..SimOptions::default()
        };
        spec
    }

    /// Adds the run options of the `[run]` table `run` to `run_options`, those that the check
    /// accepts.
    fn run_options(&mut self, run: RawRun, table: &DeTable<'_>, run_options: &mut RunOptions) {
        let key_path = |keys: &[&str]| -> Vec<PathSegment> {
            ["run", "opt"]
                .iter()
                .chain(keys)
                .map(|key| PathSegment::Key((*key).to_owned()))
                .collect()
        };
        if let Some(level) = run.opt_level {
            let span = level.span();
            let level = match level.into_inner() {
                toml::Value::String(level) => Some(level),
                // `-O 2` is a preset too.
                toml::Value::Integer(level) => Some(level.to_string()),
                other => {
                    let message = format!(
                        "'opt-level' in [run] is a preset, such as \"full\" or 2, not {other}"
                    );
                    self.diagnostic(Severity::Error, message, Some(span.clone()));
                    None
                },
            };
            if let Some(level) = level {
                self.run_option(
                    "opt-level".to_owned(),
                    level,
                    "'opt-level' in [run]",
                    Some(span),
                    run_options,
                );
            }
        }
        // In file order; the map sorts them by name.
        let mut settings: Vec<_> = run
            .opt
            .into_iter()
            .map(|(name, value)| (key_span(table, &key_path(&[&name])), name, value))
            .collect();
        settings.sort_by_key(|(span, _, _)| span.as_ref().map(|span| span.start));
        for (span, name, value) in settings {
            let value_span = value.span();
            match value.into_inner() {
                toml::Value::String(mode) => {
                    let what = format!("'{name}' in [run.opt]");
                    self.run_option(format!("opt.{name}"), mode, &what, span, run_options);
                },
                nested @ toml::Value::Table(_) => {
                    let mut leaves = Vec::new();
                    nested_leaves(vec![name], &nested, &mut leaves);
                    for (keys, leaf) in leaves {
                        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                        let leaf_span = key_span(table, &key_path(&keys));
                        let dotted = keys.join(".");
                        // An empty table has no leaf to quote.
                        let message = match leaf {
                            toml::Value::Table(_) => format!(
                                "a dotted name in [run.opt] is quoted, such as \"{dotted}.NAME\" = \
                                 \"off\""
                            ),
                            _ => format!(
                                "a dotted name in [run.opt] is quoted: \"{dotted}\" = {leaf}"
                            ),
                        };
                        self.diagnostic(Severity::Error, message, leaf_span);
                    }
                },
                other => {
                    let message = format!(
                        "the mode of '{name}' in [run.opt] is a string, such as \"on\" or \"off\", \
                         not {other}"
                    );
                    self.diagnostic(Severity::Error, message, Some(value_span));
                },
            }
        }
    }

    /// Adds the run option `key` with the text `value` to `run_options` if the check accepts it,
    /// and reports it as `what` at `span` otherwise.
    fn run_option(
        &mut self,
        key: String,
        value: String,
        what: &str,
        span: Option<ops::Range<usize>>,
        run_options: &mut RunOptions,
    ) {
        let option = RunOptions::from_iter([(key, value.into())]);
        if let Some(check) = self.check_run_options
            && let Err(error) = check(&option)
        {
            self.diagnostic(Severity::Error, format!("invalid {what}: {error}"), span);
            return;
        }
        run_options.extend(option);
    }
}

/// Collects the values in `value` at the key path `keys`, with their key paths, by name: the
/// values in its tables, nested or not, where an empty table counts as a value.
fn nested_leaves<'value>(
    keys: Vec<String>,
    value: &'value toml::Value,
    leaves: &mut Vec<(Vec<String>, &'value toml::Value)>,
) {
    match value {
        toml::Value::Table(table) if !table.is_empty() => {
            for (key, nested) in table {
                let mut path = keys.clone();
                path.push(key.clone());
                nested_leaves(path, nested, leaves);
            }
        },
        _ => leaves.push((keys, value)),
    }
}

fn path_segments(path: &serde_ignored::Path<'_>) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut current = path;
    loop {
        current = match current {
            serde_ignored::Path::Root => break,
            serde_ignored::Path::Seq { parent, index } => {
                segments.push(PathSegment::Index(*index));
                parent
            },
            serde_ignored::Path::Map { parent, key } => {
                segments.push(PathSegment::Key(key.clone()));
                parent
            },
            serde_ignored::Path::Some { parent }
            | serde_ignored::Path::NewtypeStruct { parent }
            | serde_ignored::Path::NewtypeVariant { parent } => parent,
        };
    }
    segments.reverse();
    segments
}

/// Finds the span of the last key in `path`, or of the array element if it ends with an index.
fn key_span(table: &DeTable<'_>, path: &[PathSegment]) -> Option<ops::Range<usize>> {
    let mut value: Option<&DeValue<'_>> = None;
    let mut current_table = Some(table);
    let mut span = None;
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                let (found_key, found_value) = current_table?
                    .iter()
                    .find(|(candidate, _)| candidate.get_ref().as_ref() == key.as_str())?;
                span = Some(found_key.span());
                value = Some(found_value.get_ref());
            },
            PathSegment::Index(index) => {
                let element = value?.as_array()?.get(*index)?;
                span = Some(element.span());
                value = Some(element.get_ref());
            },
        }
        current_table = value.and_then(DeValue::as_table);
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Position;
    use crate::diagnostics::Range;
    use crate::test_support::TempRoot;

    const PATH: &str = "/proj/risim-config.toml";

    fn parse_str(content: &str) -> Config {
        parse(Utf8Path::new(PATH), content, None)
    }

    fn library_names(spec: &ProjectSpec) -> Vec<&str> {
        spec.libraries.iter().map(LibrarySpec::name).collect()
    }

    #[test]
    fn parses_libraries_in_file_order() {
        let config = parse_str(
            r#"
options = ["-fsynopsys"]

[vunit]
features = ["random"]

[libraries.zeta]
files = ["src/**/*.vhd"]

[libraries.alpha]
files = ["sim/model/*.vhd", "tb/*.vhd"]
"#,
        );
        assert_eq!(config.diagnostics, []);
        let spec = config.spec.unwrap();
        assert_eq!(library_names(&spec), ["zeta", "alpha"]);
        let LibrarySpec::Sources { files, .. } = &spec.libraries[1] else {
            panic!("expected a source library");
        };
        let patterns: Vec<_> = files.iter().map(|file| file.pattern.as_str()).collect();
        assert_eq!(patterns, ["sim/model/*.vhd", "tb/*.vhd"]);
        assert_eq!(
            files[0].location,
            Some(PatternLocation {
                file: PATH.into(),
                range: Range {
                    start: Position::new(11, 10),
                    end: Position::new(11, 26),
                },
            })
        );
        assert_eq!(spec.compile_options.a_flags, ["-fsynopsys"]);
        assert_eq!(spec.sim_options.run_options, Some(RunOptions::new()));
        assert_eq!(
            spec.features.iter().copied().collect::<Vec<_>>(),
            [Feature::Random]
        );
    }

    #[test]
    fn unsupported_options_are_errors() {
        let config = parse_str(
            "options = [\"-frelaxed\", \"-Wno-hide\", \"--relaxed\", \"--no-vital-checks\"]\n",
        );
        let spec = config.spec.unwrap();
        assert_eq!(spec.compile_options.a_flags, ["-frelaxed", "--relaxed"]);
        assert_eq!(
            spec.sim_options.run_options,
            Some(RunOptions::from_iter([("relaxed".to_owned(), true.into())]))
        );
        let errors: Vec<_> = config
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.severity,
                    diagnostic.message.as_str(),
                    diagnostic.range.unwrap(),
                )
            })
            .collect();
        assert_eq!(
            errors,
            [
                (
                    Severity::Error,
                    "unsupported option '-Wno-hide'; expected one of: --relaxed, -frelaxed, \
                     -fsynopsys",
                    Range {
                        start: Position::new(1, 25),
                        end: Position::new(1, 35),
                    }
                ),
                (
                    Severity::Error,
                    "unsupported option '--no-vital-checks'; expected one of: --relaxed, \
                     -frelaxed, -fsynopsys",
                    Range {
                        start: Position::new(1, 51),
                        end: Position::new(1, 69),
                    }
                ),
            ]
        );
    }

    #[test]
    fn empty_file_is_an_empty_project() {
        let config = parse_str("");
        assert_eq!(config.diagnostics, []);
        assert_eq!(config.spec.unwrap().libraries, []);
    }

    #[test]
    fn syntax_error_has_range() {
        let config = parse_str("[libraries.foo]\nfiles = [\n");
        assert!(config.spec.is_none());
        assert_eq!(config.diagnostics.len(), 1);
        let diagnostic = &config.diagnostics[0];
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.file.as_deref(), Some(Utf8Path::new(PATH)));
        assert!(diagnostic.range.is_some());
    }

    #[test]
    fn type_error_has_range() {
        let config = parse_str("[libraries.foo]\nfiles = \"src/*.vhd\"\n");
        assert!(config.spec.is_none());
        assert_eq!(config.diagnostics.len(), 1);
        assert!(config.diagnostics[0].range.is_some());
    }

    #[test]
    fn invalid_library_names_are_errors() {
        let config = parse_str(
            r"
[libraries.Work]
files = []
[libraries.Lib]
files = []
[libraries.lib]
files = []
",
        );
        let spec = config.spec.unwrap();
        assert_eq!(library_names(&spec), ["Lib"]);
        let errors: Vec<_> = config
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.severity, diagnostic.range.unwrap().start))
            .collect();
        assert_eq!(
            errors,
            [
                (Severity::Error, Position::new(2, 12)),
                (Severity::Error, Position::new(6, 12))
            ]
        );
        assert!(config.diagnostics[1].message.contains("'Lib'"));
    }

    #[test]
    fn invalid_utf8_is_a_diagnostic() {
        let temp = TempRoot::new();
        let path = temp.write("risim-config.toml", b"options = []\n# caf\xe9\n");
        let config = load(&path, None).unwrap();
        assert!(config.spec.is_none());
        assert_eq!(config.diagnostics.len(), 1);
        assert_eq!(config.diagnostics[0].severity, Severity::Error);
        assert_eq!(
            config.diagnostics[0].range.unwrap().start,
            Position::new(2, 6)
        );
    }

    #[test]
    fn unknown_feature_is_an_error() {
        let config = parse_str("[vunit]\nfeatures = [\"random\", \"array_util\"]\n");
        assert_eq!(
            config
                .spec
                .unwrap()
                .features
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [Feature::Random]
        );
        assert_eq!(config.diagnostics.len(), 1);
        let diagnostic = &config.diagnostics[0];
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(
            diagnostic.range,
            Some(Range {
                start: Position::new(2, 23),
                end: Position::new(2, 34),
            })
        );
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let config = parse_str(
            r#"
optons = []
[libraries.lib]
files = []
standard = "2008"
"#,
        );
        assert!(config.spec.is_some());
        let warnings: Vec<_> = config
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.severity,
                    diagnostic.message.as_str(),
                    diagnostic.range.map(|range| range.start),
                )
            })
            .collect();
        assert_eq!(
            warnings,
            [
                (
                    Severity::Warning,
                    "unknown key 'optons' is ignored",
                    Some(Position::new(2, 1))
                ),
                (
                    Severity::Warning,
                    "unknown key 'libraries.lib.standard' is ignored",
                    Some(Position::new(5, 1))
                ),
            ]
        );
    }

    #[test]
    fn toolchain_version_is_accepted() {
        let config = parse_str(
            r#"
[toolchain]
version = "1.4"
other = 1
[libraries.lib]
files = []
"#,
        );
        assert!(config.spec.is_some());
        let warnings: Vec<_> = config
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.severity, diagnostic.message.as_str()))
            .collect();
        assert_eq!(
            warnings,
            [(
                Severity::Warning,
                "unknown key 'toolchain.other' is ignored"
            )]
        );
    }

    /// The diagnostics of `config` as severity, message and start position.
    fn messages(config: &Config) -> Vec<(Severity, &str, Option<Position>)> {
        config
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.severity,
                    diagnostic.message.as_str(),
                    diagnostic.range.map(|range| range.start),
                )
            })
            .collect()
    }

    #[test]
    fn run_table_becomes_run_options() {
        let config = parse_str(
            r#"
options = ["-frelaxed"]
[run]
opt-level = "full"
[run.opt]
wake-guards = "off"
"mir.cse" = "on"
"#,
        );
        assert_eq!(messages(&config), []);
        assert_eq!(
            config.spec.unwrap().sim_options.run_options,
            Some(RunOptions::from_iter([
                ("relaxed".to_owned(), true.into()),
                ("opt-level".to_owned(), "full".into()),
                ("opt.wake-guards".to_owned(), "off".into()),
                ("opt.mir.cse".to_owned(), "on".into()),
            ]))
        );
    }

    /// An integer `opt-level` is the preset of that number, as with `-O 2`; another type is an
    /// error at the value, and the other keys still count.
    #[test]
    fn run_table_takes_an_integer_level() {
        let integer = parse_str("[run]\nopt-level = 2\n");
        assert_eq!(messages(&integer), []);
        assert_eq!(
            integer.spec.unwrap().sim_options.run_options,
            Some(RunOptions::from_iter([(
                "opt-level".to_owned(),
                "2".into()
            )]))
        );
        let boolean = parse_str("[run]\nopt-level = true\n[run.opt]\nwake-guards = \"off\"\n");
        assert_eq!(
            messages(&boolean),
            [(
                Severity::Error,
                "'opt-level' in [run] is a preset, such as \"full\" or 2, not true",
                Some(Position::new(2, 13)),
            )]
        );
        assert_eq!(
            boolean.spec.unwrap().sim_options.run_options,
            Some(RunOptions::from_iter([(
                "opt.wake-guards".to_owned(),
                "off".into()
            )]))
        );
    }

    /// A table in `[run.opt]` is an error that names the quoted form, also an empty one, and a
    /// mode that isn't a string is an error at the mode; other keys of `[run]` are unknown.
    #[test]
    fn run_table_errors() {
        let config = parse_str(
            r#"
[run]
finish-after = "1 ms"
[run.opt]
wake-guards = false
mir.cse = "off"
[run.opt.mir.licm]
loops = "on"
[run.opt.empty]
"#,
        );
        assert_eq!(
            messages(&config),
            [
                (
                    Severity::Warning,
                    "unknown key 'run.finish-after' is ignored",
                    Some(Position::new(3, 1))
                ),
                (
                    Severity::Error,
                    "the mode of 'wake-guards' in [run.opt] is a string, such as \"on\" or \"off\", \
                     not false",
                    Some(Position::new(5, 15))
                ),
                (
                    Severity::Error,
                    "a dotted name in [run.opt] is quoted: \"mir.cse\" = \"off\"",
                    Some(Position::new(6, 5))
                ),
                (
                    Severity::Error,
                    "a dotted name in [run.opt] is quoted: \"mir.licm.loops\" = \"on\"",
                    Some(Position::new(8, 1))
                ),
                (
                    Severity::Error,
                    "a dotted name in [run.opt] is quoted, such as \"empty.NAME\" = \"off\"",
                    Some(Position::new(9, 10))
                ),
            ]
        );
        assert_eq!(
            config.spec.unwrap().sim_options.run_options,
            Some(RunOptions::new())
        );
    }

    /// Rejects `opt-level` `o3` and the row `bad`, as riSim's hub rejects unknown values.
    fn check(options: &RunOptions) -> Result<(), String> {
        if options.get("opt-level").is_some_and(|level| level == "o3") {
            return Err("unknown variant `o3`".to_owned());
        }
        if options.contains_key("opt.bad") {
            return Err("unknown optimization `bad`".to_owned());
        }
        Ok(())
    }

    /// The check sees one key at a time; a key that it rejects is an error at the key, and is
    /// left out.
    #[test]
    fn run_options_are_checked() {
        let config = parse(
            Utf8Path::new(PATH),
            "[run]\nopt-level = \"o3\"\n[run.opt]\nbad = \"on\"\nmir = \"off\"\n",
            Some(check),
        );
        assert_eq!(
            messages(&config),
            [
                (
                    Severity::Error,
                    "invalid 'opt-level' in [run]: unknown variant `o3`",
                    Some(Position::new(2, 13))
                ),
                (
                    Severity::Error,
                    "invalid 'bad' in [run.opt]: unknown optimization `bad`",
                    Some(Position::new(4, 1))
                ),
            ]
        );
        assert_eq!(
            config.spec.unwrap().sim_options.run_options,
            Some(RunOptions::from_iter([(
                "opt.mir".to_owned(),
                "off".into()
            )]))
        );
    }

    #[test]
    fn load_reports_missing_file() {
        let error = load(Utf8Path::new("/nonexistent/risim-config.toml"), None).unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::NotFound);
    }
}
