// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Reading `risim-config.toml` into a [`ProjectSpec`].
//!
//! ```toml
//! # Flags for analysis (risim-ghdl.a_flags) and elaboration (risim-ghdl.elab_flags).
//! options = ["-fsynopsys", "-frelaxed"]
//!
//! # Optional VUnit features on top of the defaults (VUnit builtins, com, OSVVM).
//! [vunit]
//! features = ["random", "verification_components"]
//!
//! # One table per library, with source globs relative to the workspace root.
//! [libraries.my_lib]
//! files = ["src/**/*.vhd", "tb/*.vhd"]
//! ```
//!
//! Problems become diagnostics with ranges in the file. Only syntax and type errors prevent a
//! [`ProjectSpec`]; invalid libraries and features are left out of it.
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

/// Reads and parses the configuration file at `path`.
///
/// # Errors
///
/// Fails only if the file can't be read; problems with its contents, including invalid UTF-8,
/// are diagnostics.
pub fn load(path: &Utf8Path) -> Result<Config, ReadError> {
    let content = fs::read(path).map_err(|source| ReadError {
        path: path.to_owned(),
        source,
    })?;
    Ok(match str::from_utf8(&content) {
        Ok(content) => parse(path, content),
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

/// Parses `content`, the configuration file at `path`.
pub fn parse(path: &Utf8Path, content: &str) -> Config {
    let mut parser = Parser {
        path,
        lines: LineIndex::new(content.as_bytes()),
        diagnostics: Vec::new(),
    };
    let spec = parser.parse(content);
    Config {
        spec,
        diagnostics: parser.diagnostics,
    }
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    vunit: RawVunit,
    #[serde(default)]
    libraries: BTreeMap<String, RawLibrary>,
}

#[derive(Default, Deserialize)]
struct RawVunit {
    #[serde(default)]
    features: Vec<Spanned<String>>,
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

        spec.compile_options = CompileOptions {
            a_flags: raw.options.clone(),
        };
        spec.sim_options = SimOptions {
            elab_flags: Some(raw.options),
            ..SimOptions::default()
        };
        spec
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
        parse(Utf8Path::new(PATH), content)
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
        assert_eq!(
            spec.sim_options.elab_flags,
            Some(vec!["-fsynopsys".to_owned()])
        );
        assert_eq!(
            spec.features.iter().copied().collect::<Vec<_>>(),
            [Feature::Random]
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
        let config = load(&path).unwrap();
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
    fn load_reports_missing_file() {
        let error = load(Utf8Path::new("/nonexistent/risim-config.toml")).unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::NotFound);
    }
}
