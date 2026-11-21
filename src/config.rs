// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Reading `risim-config.toml` into a [`ProjectSpec`].
//!
//! ```toml
//! # The options of the project: relaxed analysis and elaboration, the optimization preset, and
//! # rows or groups of optimizations over it, where dotted names are quoted.
//! # Only tables follow, since bare keys after the header would belong to `[options]`.
//! [options]
//! relaxed = true
//! opt-level = "full"
//!
//! [options.opt]
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
//! `[options]` can also be written inline, `options = { relaxed = true }`, which stays correct
//! anywhere above the tables.
//! It becomes [`ProjectSpec::options`] for analysis, and the run options `relaxed`, `opt-level`,
//! and `opt.<name>` of every simulation ([`SimOptions::run_options`]); the run options of a
//! configuration go over them ([`SimOptions::overridden_by`]).
//! `relaxed = false` is the same as no key.
//! `[libraries.NAME.options]` has no valid keys yet; the project's options are project level
//! only.
//!
//! Unknown keys and wrong types in the options tables are errors at their spans, also a table
//! in `[options.opt]`, which names the quoted form.
//! The former list `options = ["-frelaxed"]` and the former `[run]` table are errors that give
//! the new spelling, and nothing of them is applied.
//! Unknown keys elsewhere are warnings.
//!
//! Problems become diagnostics with ranges in the file. Only syntax and type errors prevent a
//! [`ProjectSpec`]; invalid libraries, features, and options are left out of it, also the run
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
use crate::spec::Feature;
use crate::spec::FilePattern;
use crate::spec::LibrarySpec;
use crate::spec::Options;
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

/// Reads and parses the configuration file at `path`.
///
/// `check_run_options` checks the run options of its `[options]` table (`relaxed`,
/// `opt-level`, and `opt`), one key at a time; a key that it rejects is an error and isn't
/// applied.
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

/// The keys of `[options]`, all of them project level only.
const OPTION_KEYS: [&str; 3] = ["relaxed", "opt-level", "opt"];

/// The elements of the former list `options` that make the project relaxed.
const LEGACY_RELAXED: [&str; 3] = ["--relaxed", "-frelaxed", "-frelaxed-rules"];

/// The fields of the options tables and of the former `[run]` take any value, so that a wrong
/// type is a diagnostic of ours and not a serde error that drops the spec.
#[derive(Deserialize)]
struct RawConfig {
    /// `[options]`.
    options: Option<Spanned<toml::Value>>,
    #[serde(default)]
    vunit: RawVunit,
    #[serde(default)]
    libraries: BTreeMap<String, RawLibrary>,
    /// The former `[run]` table, which is an error.
    run: Option<Spanned<toml::Value>>,
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

#[derive(Deserialize)]
struct RawLibrary {
    files: Vec<Spanned<String>>,
    /// `[libraries.NAME.options]`.
    options: Option<Spanned<toml::Value>>,
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
            let options = match library.options {
                Some(options) => self.library_options(&name, options, table),
                None => Options::default(),
            };
            spec.libraries.push(LibrarySpec::Sources {
                name,
                files,
                vhdl_standard: None,
                options,
            });
        }

        let mut run_options = RunOptions::new();
        if let Some(options) = raw.options {
            spec.options = self.options(options, table, &mut run_options);
        }
        if raw.run.is_some() {
            self.diagnostic(
                Severity::Error,
                "[run] is part of [options] now: write opt-level there, and [options.opt] for \
                 [run.opt]"
                    .to_owned(),
                key_span(table, &[PathSegment::Key("run".to_owned())]),
            );
        }
        spec.sim_options = SimOptions {
            run_options: Some(run_options),
            ..SimOptions::default()
        };
        spec
    }

    /// Reads `[options]` from `value`: its analysis options, and its run options into
    /// `run_options`, those that the check accepts.
    fn options(
        &mut self,
        value: Spanned<toml::Value>,
        table: &DeTable<'_>,
        run_options: &mut RunOptions,
    ) -> Options {
        let mut options = Options::default();
        let span = value.span();
        let keys = match value.into_inner() {
            toml::Value::Table(keys) => keys,
            toml::Value::Array(list) => {
                self.legacy_options(&list, table);
                return options;
            },
            _ => {
                self.diagnostic(
                    Severity::Error,
                    "`options` is a table, such as options = { relaxed = true }".to_owned(),
                    Some(span),
                );
                return options;
            },
        };
        let path = |key: &str| -> Vec<PathSegment> {
            vec![
                PathSegment::Key("options".to_owned()),
                PathSegment::Key(key.to_owned()),
            ]
        };
        for (key, entry) in in_file_order(table, keys, path) {
            let key_span = key_span(table, &path(&key));
            let value_span = value_span(table, &path(&key));
            match key.as_str() {
                "relaxed" => match entry {
                    toml::Value::Boolean(true) => {
                        options.relaxed = self.run_option(
                            "relaxed".to_owned(),
                            serde_json::Value::Bool(true),
                            "'relaxed' in [options]",
                            value_span,
                            run_options,
                        );
                    },
                    // The same as no key, so that it can't mask a configuration's `relaxed`.
                    toml::Value::Boolean(false) => {},
                    other => {
                        let message =
                            format!("'relaxed' in [options] is true or false, not {other}");
                        self.diagnostic(Severity::Error, message, value_span);
                    },
                },
                "opt-level" => {
                    let level = match entry {
                        toml::Value::String(level) => Some(level),
                        // `-O 2` is a preset too.
                        toml::Value::Integer(level) => Some(level.to_string()),
                        other => {
                            let message = format!(
                                "'opt-level' in [options] is a preset, such as \"full\" or 2, \
                                 not {other}"
                            );
                            self.diagnostic(Severity::Error, message, value_span.clone());
                            None
                        },
                    };
                    if let Some(level) = level {
                        self.run_option(
                            "opt-level".to_owned(),
                            level.into(),
                            "'opt-level' in [options]",
                            value_span,
                            run_options,
                        );
                    }
                },
                "opt" => match entry {
                    toml::Value::Table(modes) => self.opt_modes(modes, table, run_options),
                    _ => self.diagnostic(
                        Severity::Error,
                        "'opt' in [options] is a table of modes, such as [options.opt] or opt = \
                         { wake-guards = \"off\" }"
                            .to_owned(),
                        value_span,
                    ),
                },
                _ => {
                    let mut message = format!(
                        "unknown option '{key}' in [options]; expected one of: {}",
                        OPTION_KEYS.join(", ")
                    );
                    if ["libraries", "vunit", "toolchain"].contains(&key.as_str()) {
                        message.push_str(
                            "; keys after the [options] header belong to it, so put them above \
                             the header, or write options = { … }",
                        );
                    }
                    self.diagnostic(Severity::Error, message, key_span);
                },
            }
        }
        options
    }

    /// Adds the run options `opt.<name>` of the modes `modes` of `[options.opt]` to
    /// `run_options`, those that the check accepts.
    fn opt_modes(&mut self, modes: toml::Table, table: &DeTable<'_>, run_options: &mut RunOptions) {
        let path = |keys: &[&str]| -> Vec<PathSegment> {
            ["options", "opt"]
                .iter()
                .chain(keys)
                .map(|key| PathSegment::Key((*key).to_owned()))
                .collect()
        };
        for (name, value) in in_file_order(table, modes, |name| path(&[name])) {
            let span = key_span(table, &path(&[&name]));
            match value {
                toml::Value::String(mode) => {
                    let what = format!("'{name}' in [options.opt]");
                    self.run_option(format!("opt.{name}"), mode.into(), &what, span, run_options);
                },
                nested @ toml::Value::Table(_) => {
                    let mut leaves = Vec::new();
                    nested_leaves(vec![name], &nested, &mut leaves);
                    for (keys, leaf) in leaves {
                        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                        let leaf_span = key_span(table, &path(&keys));
                        let dotted = keys.join(".");
                        // An empty table has no leaf to quote.
                        let message = match leaf {
                            toml::Value::Table(_) => format!(
                                "a dotted name in [options.opt] is quoted, such as \
                                 \"{dotted}.NAME\" = \"off\""
                            ),
                            _ => format!(
                                "a dotted name in [options.opt] is quoted: \"{dotted}\" = {leaf}"
                            ),
                        };
                        self.diagnostic(Severity::Error, message, leaf_span);
                    }
                },
                other => {
                    let message = format!(
                        "the mode of '{name}' in [options.opt] is a string, such as \"on\" or \
                         \"off\", not {other}"
                    );
                    self.diagnostic(Severity::Error, message, value_span(table, &path(&[&name])));
                },
            }
        }
    }

    /// Reads `[libraries.NAME.options]` of the library `library` from `value`.
    ///
    /// It has no valid keys yet: the keys of `[options]` are project level only.
    fn library_options(
        &mut self,
        library: &str,
        value: Spanned<toml::Value>,
        table: &DeTable<'_>,
    ) -> Options {
        let path = |key: &str| -> Vec<PathSegment> {
            ["libraries", library, "options", key]
                .iter()
                .map(|key| PathSegment::Key((*key).to_owned()))
                .collect()
        };
        let span = value.span();
        let toml::Value::Table(keys) = value.into_inner() else {
            self.diagnostic(
                Severity::Error,
                format!("`options` of library '{library}' is a table, such as options = {{}}"),
                Some(span),
            );
            return Options::default();
        };
        for (key, _) in in_file_order(table, keys, |key| path(key)) {
            let message = if OPTION_KEYS.contains(&key.as_str()) {
                format!("'{key}' is a project option; set it in [options]")
            } else {
                format!("unknown option '{key}' in [libraries.{library}.options]")
            };
            self.diagnostic(Severity::Error, message, key_span(table, &path(&key)));
        }
        Options::default()
    }

    /// Reports the former list `options`, with the table that replaces it; nothing of it is
    /// applied.
    fn legacy_options(&mut self, list: &[toml::Value], table: &DeTable<'_>) {
        let text = |element: &toml::Value| match element {
            toml::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let relaxed = list
            .iter()
            .any(|element| LEGACY_RELAXED.contains(&text(element).as_str()));
        let mut parts = vec![if relaxed {
            "`options` is a table now: replace this list with options = { relaxed = true }"
                .to_owned()
        } else {
            "`options` is a table now: delete this list".to_owned()
        }];
        let mut seen = Vec::new();
        for element in list {
            let element = text(element);
            if LEGACY_RELAXED.contains(&element.as_str()) || seen.contains(&element) {
                continue;
            }
            parts.push(if element == "-fsynopsys" {
                "'-fsynopsys' isn't needed, since the Synopsys packages are always in IEEE"
                    .to_owned()
            } else {
                format!("'{element}' has no equivalent")
            });
            seen.push(element);
        }
        self.diagnostic(
            Severity::Error,
            parts.join("; "),
            key_span(table, &[PathSegment::Key("options".to_owned())]),
        );
    }

    /// Adds the run option `key` with `value` to `run_options` if the check accepts it, and
    /// reports it as `what` at `span` otherwise; returns whether it was added.
    fn run_option(
        &mut self,
        key: String,
        value: serde_json::Value,
        what: &str,
        span: Option<ops::Range<usize>>,
        run_options: &mut RunOptions,
    ) -> bool {
        let option = RunOptions::from_iter([(key, value)]);
        if let Some(check) = self.check_run_options
            && let Err(error) = check(&option)
        {
            self.diagnostic(Severity::Error, format!("invalid {what}: {error}"), span);
            return false;
        }
        run_options.extend(option);
        true
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

/// Returns the entries of `keys`, a table at the key paths `path(key)` of `table`, in file order.
///
/// A table iterates its entries by name.
fn in_file_order(
    table: &DeTable<'_>,
    keys: toml::Table,
    path: impl Fn(&str) -> Vec<PathSegment>,
) -> Vec<(String, toml::Value)> {
    let mut entries: Vec<_> = keys
        .into_iter()
        .map(|(key, value)| (key_span(table, &path(&key)), key, value))
        .collect();
    entries.sort_by_key(|(span, _, _)| span.as_ref().map(|span| span.start));
    entries
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .collect()
}

/// Finds the span of the last key in `path`, or of the array element if it ends with an index.
fn key_span(table: &DeTable<'_>, path: &[PathSegment]) -> Option<ops::Range<usize>> {
    spans(table, path).map(|(key, _)| key)
}

/// Finds the span of the value at `path`.
fn value_span(table: &DeTable<'_>, path: &[PathSegment]) -> Option<ops::Range<usize>> {
    spans(table, path).map(|(_, value)| value)
}

/// Finds the spans of the last key in `path` and of its value, or of the array element twice if
/// it ends with an index.
fn spans(
    table: &DeTable<'_>,
    path: &[PathSegment],
) -> Option<(ops::Range<usize>, ops::Range<usize>)> {
    let mut value: Option<&DeValue<'_>> = None;
    let mut current_table = Some(table);
    let mut spans = None;
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                let (found_key, found_value) = current_table?
                    .iter()
                    .find(|(candidate, _)| candidate.get_ref().as_ref() == key.as_str())?;
                spans = Some((found_key.span(), found_value.span()));
                value = Some(found_value.get_ref());
            },
            PathSegment::Index(index) => {
                let element = value?.as_array()?.get(*index)?;
                spans = Some((element.span(), element.span()));
                value = Some(element.get_ref());
            },
        }
        current_table = value.and_then(DeValue::as_table);
    }
    spans
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
                    start: Position::new(9, 10),
                    end: Position::new(9, 26),
                },
            })
        );
        assert_eq!(spec.options, Options::default());
        assert_eq!(spec.sim_options.run_options, Some(RunOptions::new()));
        assert_eq!(
            spec.features.iter().copied().collect::<Vec<_>>(),
            [Feature::Random]
        );
    }

    /// The former list `options` is one error that gives the table, and nothing of it is
    /// applied.
    #[test]
    fn list_options_name_the_table() {
        let config = parse_str(
            "options = [\"-frelaxed\", \"-fsynopsys\", \"-Wno-hide\"]\n[libraries.lib]\nfiles = \
             []\n",
        );
        let spec = config.spec.clone().unwrap();
        assert_eq!(library_names(&spec), ["lib"]);
        assert_eq!(spec.options, Options::default());
        assert_eq!(spec.sim_options.run_options, Some(RunOptions::new()));
        assert_eq!(
            messages(&config),
            [(
                Severity::Error,
                "`options` is a table now: replace this list with options = { relaxed = true }; \
                 '-fsynopsys' isn't needed, since the Synopsys packages are always in IEEE; \
                 '-Wno-hide' has no equivalent",
                Some(Position::new(1, 1)),
            )]
        );

        let empty = parse_str("options = []\n");
        assert_eq!(
            messages(&empty),
            [(
                Severity::Error,
                "`options` is a table now: delete this list",
                Some(Position::new(1, 1)),
            )]
        );
    }

    #[test]
    fn options_table_sets_options_and_run_options() {
        let expected = RunOptions::from_iter([
            ("relaxed".to_owned(), true.into()),
            ("opt-level".to_owned(), "full".into()),
            ("opt.wake-guards".to_owned(), "off".into()),
            ("opt.mir.cse".to_owned(), "off".into()),
        ]);
        let config = parse_str(
            r#"
[options]
relaxed = true
opt-level = "full"

[options.opt]
wake-guards = "off"
"mir.cse" = "off"
"#,
        );
        assert_eq!(messages(&config), []);
        let spec = config.spec.unwrap();
        assert!(spec.options.relaxed);
        assert_eq!(spec.sim_options.run_options, Some(expected));

        // The inline form stays correct before dotted top-level keys.
        let inline = parse_str(
            r#"
options = { relaxed = true, opt = { wake-guards = "off" } }
libraries.lib.files = ["*.vhd"]
"#,
        );
        assert_eq!(messages(&inline), []);
        let inline_spec = inline.spec.unwrap();
        assert!(inline_spec.options.relaxed);
        assert_eq!(library_names(&inline_spec), ["lib"]);
        assert_eq!(
            inline_spec.sim_options.run_options,
            Some(RunOptions::from_iter([
                ("relaxed".to_owned(), true.into()),
                ("opt.wake-guards".to_owned(), "off".into()),
            ]))
        );
    }

    #[test]
    fn relaxed_false_is_like_no_key() {
        let config = parse_str("options = { relaxed = false }\n");
        assert_eq!(messages(&config), []);
        assert_eq!(config.spec, parse_str("").spec);
    }

    /// Each wrong key or value of `[options]` is an error at the key or value; the valid keys and
    /// the libraries still load.
    #[test]
    fn options_errors_have_ranges() {
        let config = parse_str(
            r#"[options]
colour = 1
top = "tb"
relaxed = "yes"
opt-level = true
[options.opt]
wake-guards = 1
"mir.cse" = "off"
[libraries.lib]
files = []
"#,
        );
        assert_eq!(
            messages(&config),
            [
                (
                    Severity::Error,
                    "unknown option 'colour' in [options]; expected one of: relaxed, opt-level, \
                     opt",
                    Some(Position::new(2, 1))
                ),
                (
                    Severity::Error,
                    "unknown option 'top' in [options]; expected one of: relaxed, opt-level, opt",
                    Some(Position::new(3, 1))
                ),
                (
                    Severity::Error,
                    "'relaxed' in [options] is true or false, not \"yes\"",
                    Some(Position::new(4, 11))
                ),
                (
                    Severity::Error,
                    "'opt-level' in [options] is a preset, such as \"full\" or 2, not true",
                    Some(Position::new(5, 13))
                ),
                (
                    Severity::Error,
                    "the mode of 'wake-guards' in [options.opt] is a string, such as \"on\" or \
                     \"off\", not 1",
                    Some(Position::new(7, 15))
                ),
            ]
        );
        let spec = config.spec.unwrap();
        assert_eq!(library_names(&spec), ["lib"]);
        assert_eq!(
            spec.sim_options.run_options,
            Some(RunOptions::from_iter([(
                "opt.mir.cse".to_owned(),
                "off".into()
            )]))
        );
    }

    /// The errors of `[options]` in the inline and dotted forms, and of a scalar `options`, are at
    /// their keys or values, and a key after the `[options]` header names the pitfall.
    #[test]
    fn options_errors_in_other_forms() {
        let inline = parse_str("options = { relaxed = true, opt = \"off\" }\noptions2 = 1\n");
        assert_eq!(
            messages(&inline),
            [
                (
                    Severity::Warning,
                    "unknown key 'options2' is ignored",
                    Some(Position::new(2, 1))
                ),
                (
                    Severity::Error,
                    "'opt' in [options] is a table of modes, such as [options.opt] or opt = { \
                     wake-guards = \"off\" }",
                    Some(Position::new(1, 35))
                ),
            ]
        );
        assert!(inline.spec.unwrap().options.relaxed);

        let dotted = parse_str("options.relaxed = 2\noptions.opt.mir.cse = \"off\"\n");
        assert_eq!(
            messages(&dotted),
            [
                (
                    Severity::Error,
                    "'relaxed' in [options] is true or false, not 2",
                    Some(Position::new(1, 19))
                ),
                (
                    Severity::Error,
                    "a dotted name in [options.opt] is quoted: \"mir.cse\" = \"off\"",
                    Some(Position::new(2, 17))
                ),
            ]
        );

        let scalar = parse_str("options = 1\n[libraries.lib]\nfiles = []\n");
        assert_eq!(
            messages(&scalar),
            [(
                Severity::Error,
                "`options` is a table, such as options = { relaxed = true }",
                Some(Position::new(1, 11))
            )]
        );
        assert_eq!(library_names(&scalar.spec.unwrap()), ["lib"]);

        // Keys after the header belong to the table.
        let after_header = parse_str("[options]\nrelaxed = true\nlibraries.lib.files = []\n");
        assert_eq!(
            messages(&after_header),
            [(
                Severity::Error,
                "unknown option 'libraries' in [options]; expected one of: relaxed, opt-level, \
                 opt; keys after the [options] header belong to it, so put them above the \
                 header, or write options = { … }",
                Some(Position::new(3, 1))
            )]
        );
    }

    #[test]
    fn run_table_is_an_error() {
        let config = parse_str("[run]\nopt-level = \"off\"\n[run.opt]\nwake-guards = \"off\"\n");
        assert_eq!(
            messages(&config),
            [(
                Severity::Error,
                "[run] is part of [options] now: write opt-level there, and [options.opt] for \
                 [run.opt]",
                Some(Position::new(1, 2))
            )]
        );
        assert_eq!(
            config.spec.unwrap().sim_options.run_options,
            Some(RunOptions::new())
        );
        let opt_only = parse_str("[run.opt]\nwake-guards = \"off\"\n");
        assert_eq!(opt_only.diagnostics.len(), 1);
        assert_eq!(
            opt_only.spec.unwrap().sim_options.run_options,
            Some(RunOptions::new())
        );
    }

    /// `[libraries.NAME.options]` has no valid keys; the library loads with the default options.
    #[test]
    fn library_options() {
        let config = parse_str(
            r#"
[libraries.lib]
files = []
[libraries.lib.options]
relaxed = true
opt-level = "full"
include = "inc"
[libraries.other]
files = []
options = 1
"#,
        );
        assert_eq!(
            messages(&config),
            [
                (
                    Severity::Error,
                    "'relaxed' is a project option; set it in [options]",
                    Some(Position::new(5, 1))
                ),
                (
                    Severity::Error,
                    "'opt-level' is a project option; set it in [options]",
                    Some(Position::new(6, 1))
                ),
                (
                    Severity::Error,
                    "unknown option 'include' in [libraries.lib.options]",
                    Some(Position::new(7, 1))
                ),
                (
                    Severity::Error,
                    "`options` of library 'other' is a table, such as options = {}",
                    Some(Position::new(10, 11))
                ),
            ]
        );
        let spec = config.spec.unwrap();
        assert_eq!(library_names(&spec), ["lib", "other"]);
        for library in &spec.libraries {
            let LibrarySpec::Sources { options, .. } = library else {
                panic!("expected a source library");
            };
            assert_eq!(*options, Options::default());
        }
        assert!(!spec.options.relaxed);
        assert_eq!(spec.sim_options.run_options, Some(RunOptions::new()));
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
        let path = temp.write("risim-config.toml", b"[options]\n# caf\xe9\n");
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
    fn options_table_becomes_run_options() {
        let config = parse_str(
            r#"
[options]
relaxed = true
opt-level = "full"
[options.opt]
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
    fn options_table_takes_an_integer_level() {
        let integer = parse_str("[options]\nopt-level = 2\n");
        assert_eq!(messages(&integer), []);
        assert_eq!(
            integer.spec.unwrap().sim_options.run_options,
            Some(RunOptions::from_iter([(
                "opt-level".to_owned(),
                "2".into()
            )]))
        );
        let boolean =
            parse_str("[options]\nopt-level = true\n[options.opt]\nwake-guards = \"off\"\n");
        assert_eq!(
            messages(&boolean),
            [(
                Severity::Error,
                "'opt-level' in [options] is a preset, such as \"full\" or 2, not true",
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

    /// A table in `[options.opt]` is an error that names the quoted form, also an empty one, and
    /// a mode that isn't a string is an error at the mode; other keys of `[options]` are unknown,
    /// also those of other run options.
    #[test]
    fn options_table_errors() {
        let config = parse_str(
            r#"
[options]
finish-after = "1 ms"
[options.opt]
wake-guards = false
mir.cse = "off"
[options.opt.mir.licm]
loops = "on"
[options.opt.empty]
"#,
        );
        assert_eq!(
            messages(&config),
            [
                (
                    Severity::Error,
                    "unknown option 'finish-after' in [options]; expected one of: relaxed, \
                     opt-level, opt",
                    Some(Position::new(3, 1))
                ),
                (
                    Severity::Error,
                    "the mode of 'wake-guards' in [options.opt] is a string, such as \"on\" or \
                     \"off\", not false",
                    Some(Position::new(5, 15))
                ),
                (
                    Severity::Error,
                    "a dotted name in [options.opt] is quoted: \"mir.cse\" = \"off\"",
                    Some(Position::new(6, 5))
                ),
                (
                    Severity::Error,
                    "a dotted name in [options.opt] is quoted: \"mir.licm.loops\" = \"on\"",
                    Some(Position::new(8, 1))
                ),
                (
                    Severity::Error,
                    "a dotted name in [options.opt] is quoted, such as \"empty.NAME\" = \"off\"",
                    Some(Position::new(9, 14))
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
            "[options]\nopt-level = \"o3\"\n[options.opt]\nbad = \"on\"\nmir = \"off\"\n",
            Some(check),
        );
        assert_eq!(
            messages(&config),
            [
                (
                    Severity::Error,
                    "invalid 'opt-level' in [options]: unknown variant `o3`",
                    Some(Position::new(2, 13))
                ),
                (
                    Severity::Error,
                    "invalid 'bad' in [options.opt]: unknown optimization `bad`",
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

        // A rejected `relaxed` makes neither the analysis nor the elaboration relaxed.
        let reject_relaxed = |options: &RunOptions| -> Result<(), String> {
            if options.contains_key("relaxed") {
                return Err("not here".to_owned());
            }
            Ok(())
        };
        let rejected = parse(
            Utf8Path::new(PATH),
            "options = { relaxed = true }\n",
            Some(reject_relaxed),
        );
        assert_eq!(
            messages(&rejected),
            [(
                Severity::Error,
                "invalid 'relaxed' in [options]: not here",
                Some(Position::new(1, 23))
            )]
        );
        let spec = rejected.spec.unwrap();
        assert!(!spec.options.relaxed);
        assert_eq!(spec.sim_options.run_options, Some(RunOptions::new()));
    }

    #[test]
    fn load_reports_missing_file() {
        let error = load(Utf8Path::new("/nonexistent/risim-config.toml"), None).unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::NotFound);
    }
}
