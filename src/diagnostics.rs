// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Diagnostics reported to the client, conversion of byte offsets to positions, and parsing of
//! the JSON report lines of `risim analyze --message-format=json`.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::error::Error;
use std::fmt;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde::Serialize;

/// The severity of a [`Diagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Something is broken; the affected item is skipped or fails.
    Error,
    /// Something is suspicious, but processing continues.
    Warning,
    /// Additional information.
    Note,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        })
    }
}

/// The producer of a set of diagnostics.
///
/// Each source replaces only its own set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSource {
    /// The configuration file or the project specification.
    Config,
    /// Source collection, parsing, dependency analysis and test discovery.
    Project,
    /// Compilation of source files.
    Compile,
    /// Simulation runs.
    Simulation,
}

impl DiagnosticSource {
    /// All sources.
    pub const ALL: [Self; 4] = [Self::Config, Self::Project, Self::Compile, Self::Simulation];
}

/// A 1-based line and column.
///
/// Columns count UTF-16 code units if the line is valid UTF-8, and bytes otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub column: u32,
}

impl Position {
    /// The position at 1-based `line` and `column`.
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

/// A range in a file; `end` is the position of the last character, not after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Range {
    /// Position of the first character.
    pub start: Position,
    /// Position of the last character.
    pub end: Position,
}

/// A message about a file, or about the project as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Diagnostic {
    /// How serious the problem is.
    pub severity: Severity,
    /// The human-readable message.
    pub message: String,
    /// The file the message refers to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Utf8PathBuf>,
    /// The range in `file` the message refers to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

impl Diagnostic {
    /// Creates an error that isn't tied to a file.
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Severity::Error, message)
    }

    /// Creates a warning that isn't tied to a file.
    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, message)
    }

    /// Creates a diagnostic that isn't tied to a file.
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
            file: None,
            range: None,
        }
    }

    /// Attaches the diagnostic to `file`.
    #[must_use]
    pub fn in_file(mut self, file: impl Into<Utf8PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Attaches the diagnostic to `range` in its file.
    #[must_use]
    pub const fn at(mut self, range: Option<Range>) -> Self {
        self.range = range;
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{file}")?;
            if let Some(range) = &self.range {
                write!(f, ":{}:{}", range.start.line, range.start.column)?;
            }
            f.write_str(": ")?;
        }
        write!(f, "{}: {}", self.severity, self.message)
    }
}

/// Converts byte offsets in a text to [`Position`]s.
#[derive(Debug, Clone)]
pub struct LineIndex<'text> {
    text: &'text [u8],
    /// Byte offset of the start of every line.
    line_starts: Vec<usize>,
}

impl<'text> LineIndex<'text> {
    /// Indexes the line starts of `text`.
    pub fn new(text: &'text [u8]) -> Self {
        let line_starts = std::iter::once(0)
            .chain(
                text.iter()
                    .enumerate()
                    .filter(|&(_, &byte)| byte == b'\n')
                    .map(|(offset, _)| offset + 1),
            )
            .collect();
        Self { text, line_starts }
    }

    /// Returns the position of the byte at `offset`.
    ///
    /// Offsets past the end of the text are clamped to the end.
    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        let line_index = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts.get(line_index).copied().unwrap_or(0);
        let line_end = self
            .line_starts
            .get(line_index + 1)
            .copied()
            .unwrap_or(self.text.len());
        let line = self.text.get(line_start..line_end).unwrap_or_default();
        let prefix_len = offset - line_start;
        // An offset inside a character is the position of that character.
        let column = match std::str::from_utf8(line) {
            Ok(line) => line
                .char_indices()
                .take_while(|&(start, ch)| start + ch.len_utf8() <= prefix_len)
                .map(|(_, ch)| ch.len_utf16())
                .sum(),
            Err(_) => prefix_len,
        };
        Position {
            line: saturating_u32(line_index + 1),
            column: saturating_u32(column + 1),
        }
    }

    /// Returns the range covering the bytes in `span`; an empty span covers one character.
    pub fn range(&self, span: std::ops::Range<usize>) -> Range {
        Range {
            start: self.position(span.start),
            end: self.position(span.end.saturating_sub(1).max(span.start)),
        }
    }
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// `error` and its chain of sources on one line, separated by `: `.
///
/// The error types of this crate keep the cause out of their message, so this is the way to
/// turn one into a complete message.
pub fn error_chain(error: &dyn Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// A line of the output of `risim analyze --message-format=json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportLine {
    /// A diagnostic record.
    Diagnostic(AnalysisReport),
    /// A JSON record of another type, which a later risim-runner may print.
    Other,
    /// Not a JSON record, such as a crash message.
    Text,
}

impl ReportLine {
    /// Parses a line of output.
    pub fn parse(line: &str) -> Self {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            return Self::Text;
        };
        if !record.is_object() {
            return Self::Text;
        }
        if record.get("type").and_then(serde_json::Value::as_str) != Some("diagnostic") {
            return Self::Other;
        }
        serde_json::from_value(record).map_or(Self::Other, Self::Diagnostic)
    }
}

/// A diagnostic record of hydrolyze's JSON report format (see `hydrolyze/doc/diagnostics.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AnalysisReport {
    /// The severity.
    pub severity: Severity,
    /// The code, such as `type-mismatch`.
    #[serde(default)]
    pub code: Option<String>,
    /// The message.
    pub message: String,
    /// Where the problem is; absent for problems of a library or an archive.
    #[serde(default)]
    pub location: Option<ReportLocation>,
    /// Further information, each with an optional location.
    #[serde(default)]
    pub notes: Vec<ReportNote>,
}

/// A location of a JSON report.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReportLocation {
    /// The file as given to `risim analyze`, or `<builtin:name>` or `<memory:name>` for a file
    /// that isn't on disk.
    pub file: String,
    /// The first character.
    #[serde(default)]
    pub start: Option<ReportPosition>,
    /// The position after the last character.
    #[serde(default)]
    pub end: Option<ReportPosition>,
}

/// A position of a JSON report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ReportPosition {
    /// The 1-based line.
    pub line: u32,
    /// The 1-based column, in characters.
    pub column: u32,
    /// The byte offset in the file.
    pub offset: usize,
}

/// A note of a JSON report.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReportNote {
    /// The message.
    pub message: String,
    /// Where the note points to.
    #[serde(default)]
    pub location: Option<ReportLocation>,
}

impl ReportLocation {
    /// The file on disk, resolved against `cwd`; `None` for a file that isn't on disk.
    pub fn path(&self, cwd: &Utf8Path) -> Option<Utf8PathBuf> {
        let is_virtual = self.file.starts_with('<') && self.file.ends_with('>');
        (!is_virtual && !self.file.is_empty()).then(|| cwd.join(&self.file))
    }

    /// The range in the file with the contents `source`, from the byte offsets; without the
    /// contents, from the lines and columns. `None` if the location has no positions.
    pub fn range(&self, source: Option<&[u8]>) -> Option<Range> {
        let (start, end) = (self.start?, self.end?);
        if let Some(source) = source {
            return Some(LineIndex::new(source).range(start.offset..end.offset));
        }
        let start_position = Position::new(start.line, start.column);
        // `end` is exclusive.
        let end_position = if (end.line, end.column) > (start.line, start.column + 1) {
            Position::new(end.line, end.column.saturating_sub(1).max(1))
        } else {
            start_position
        };
        Some(Range {
            start: start_position,
            end: end_position,
        })
    }
}

impl AnalysisReport {
    /// Converts the report into diagnostics: the report itself, then one note per note with a
    /// location on disk. Notes without such a location are appended to the message.
    ///
    /// Relative paths are resolved against `cwd`; `source` returns the contents of a file, for
    /// converting byte offsets. A report without a location on disk is attached to `file`, the
    /// file that was analysed, without a range.
    pub fn to_diagnostics<'source>(
        &self,
        cwd: &Utf8Path,
        file: &Utf8Path,
        mut source: impl FnMut(&Utf8Path) -> Option<&'source [u8]>,
    ) -> Vec<Diagnostic> {
        let mut message = self.message.clone();
        let mut notes = Vec::new();
        for note in &self.notes {
            let located = note.location.as_ref().and_then(|location| {
                let path = location.path(cwd)?;
                let range = location.range(source(&path));
                Some((path, range))
            });
            if let Some((path, range)) = located {
                notes.push(
                    Diagnostic::new(Severity::Note, note.message.clone())
                        .in_file(path)
                        .at(range),
                );
            } else {
                message.push('\n');
                message.push_str(&note.message);
            }
        }
        let diagnostic = Diagnostic::new(self.severity, message);
        let diagnostic = match self
            .location
            .as_ref()
            .and_then(|location| Some((location, location.path(cwd)?)))
        {
            Some((location, path)) => {
                let range = location.range(source(&path));
                diagnostic.in_file(path).at(range)
            },
            None => diagnostic.in_file(file),
        };
        std::iter::once(diagnostic).chain(notes).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_chain_joins_the_sources() {
        let error = crate::config::ReadError {
            path: "a.toml".into(),
            source: std::io::Error::other("gone"),
        };
        assert_eq!(error_chain(&error), "failed to read a.toml: gone");
    }

    #[test]
    fn positions_are_one_based() {
        let index = LineIndex::new(b"ab\ncd\n");
        assert_eq!(index.position(0), Position::new(1, 1));
        assert_eq!(index.position(1), Position::new(1, 2));
        assert_eq!(index.position(3), Position::new(2, 1));
        assert_eq!(index.position(5), Position::new(2, 3));
        assert_eq!(index.position(6), Position::new(3, 1));
        assert_eq!(index.position(100), Position::new(3, 1));
    }

    #[test]
    fn utf8_lines_count_utf16_code_units() {
        // "ä" is two bytes and one UTF-16 code unit; "𝄞" is four bytes and two code units.
        let text = "äx𝄞y\n".as_bytes();
        let index = LineIndex::new(text);
        assert_eq!(index.position(2), Position::new(1, 2));
        assert_eq!(index.position(7), Position::new(1, 5));
    }

    #[test]
    fn non_utf8_lines_count_bytes() {
        let text = b"\xe4x\ny";
        let index = LineIndex::new(text);
        assert_eq!(index.position(1), Position::new(1, 2));
        assert_eq!(index.position(3), Position::new(2, 1));
    }

    #[test]
    fn range_end_is_last_character() {
        let index = LineIndex::new(b"entity foo is");
        let range = index.range(7..10);
        assert_eq!(range.start, Position::new(1, 8));
        assert_eq!(range.end, Position::new(1, 10));
    }

    #[test]
    fn report_lines() {
        assert_eq!(
            ReportLine::parse("thread 'main' panicked"),
            ReportLine::Text
        );
        assert_eq!(ReportLine::parse("42"), ReportLine::Text);
        assert_eq!(
            ReportLine::parse(r#"{"type":"summary"}"#),
            ReportLine::Other
        );
        assert_eq!(
            ReportLine::parse(r#"{"type":"diagnostic","message":"no severity"}"#),
            ReportLine::Other
        );
        assert_eq!(
            ReportLine::parse(
                r#"{"type":"diagnostic","severity":"warning","code":"x","message":"m","relaxable":false}"#
            ),
            ReportLine::Diagnostic(AnalysisReport {
                severity: Severity::Warning,
                code: Some("x".to_owned()),
                message: "m".to_owned(),
                location: None,
                notes: Vec::new(),
            })
        );
    }

    fn parse_report(line: &str) -> AnalysisReport {
        match ReportLine::parse(line) {
            ReportLine::Diagnostic(report) => report,
            other => panic!("not a diagnostic: {other:?}"),
        }
    }

    #[test]
    fn report_ranges_come_from_offsets() {
        // The tab and the "ä" make the columns of the report differ from those of the client.
        let source = "entity e is\n\tconstant \u{e4}: x;\nend;\n";
        let report = parse_report(
            r#"{"type":"diagnostic","severity":"error","code":"c","message":"bad","relaxable":false,"unit":"work.e","location":{"file":"src/e.vhd","start":{"line":2,"column":11,"offset":22},"end":{"line":2,"column":12,"offset":24}},"notes":[{"message":"declared here","location":{"file":"/abs/p.vhd","start":{"line":1,"column":1,"offset":0},"end":{"line":1,"column":3,"offset":2}}},{"message":"in the standard","location":{"file":"<builtin:standard>","start":{"line":1,"column":1,"offset":0},"end":{"line":1,"column":2,"offset":1}}},{"message":"no location"}]}"#,
        );
        let mut read = Vec::new();
        let diagnostics = report.to_diagnostics(
            Utf8Path::new("/root"),
            Utf8Path::new("/root/src/e.vhd"),
            |path| {
                read.push(path.to_owned());
                (path == "/root/src/e.vhd").then_some(source.as_bytes())
            },
        );
        assert_eq!(
            diagnostics,
            [
                Diagnostic::error("bad\nin the standard\nno location")
                    .in_file("/root/src/e.vhd")
                    .at(Some(Range {
                        start: Position::new(2, 11),
                        end: Position::new(2, 11),
                    })),
                Diagnostic::new(Severity::Note, "declared here")
                    .in_file("/abs/p.vhd")
                    .at(Some(Range {
                        start: Position::new(1, 1),
                        end: Position::new(1, 2),
                    })),
            ]
        );
        assert_eq!(read, ["/abs/p.vhd", "/root/src/e.vhd"]);
    }

    #[test]
    fn reports_without_location_belong_to_the_analysed_file() {
        let report = parse_report(
            r#"{"type":"diagnostic","severity":"error","code":"io","message":"cannot read","relaxable":false}"#,
        );
        let diagnostics =
            report.to_diagnostics(Utf8Path::new("/root"), Utf8Path::new("/root/a.vhd"), |_| {
                None
            });
        assert_eq!(
            diagnostics,
            [Diagnostic::error("cannot read").in_file("/root/a.vhd")]
        );
    }

    #[test]
    fn report_ranges_without_source_use_lines_and_columns() {
        let location = |start: (u32, u32), end: (u32, u32)| ReportLocation {
            file: "a.vhd".to_owned(),
            start: Some(ReportPosition {
                line: start.0,
                column: start.1,
                offset: 0,
            }),
            end: Some(ReportPosition {
                line: end.0,
                column: end.1,
                offset: 0,
            }),
        };
        let range = |start: (u32, u32), end: (u32, u32)| {
            Some(Range {
                start: Position::new(start.0, start.1),
                end: Position::new(end.0, end.1),
            })
        };
        assert_eq!(location((2, 3), (2, 7)).range(None), range((2, 3), (2, 6)));
        assert_eq!(location((2, 3), (2, 4)).range(None), range((2, 3), (2, 3)));
        assert_eq!(location((2, 3), (3, 1)).range(None), range((2, 3), (3, 1)));
        let no_positions = ReportLocation {
            file: "a.vhd".to_owned(),
            start: None,
            end: None,
        };
        assert_eq!(no_positions.range(None), None);
    }

    #[test]
    fn display_includes_location() {
        let diagnostic = Diagnostic::warning("something")
            .in_file("/a/b.vhd")
            .at(Some(Range {
                start: Position::new(3, 4),
                end: Position::new(3, 5),
            }));
        assert_eq!(diagnostic.to_string(), "/a/b.vhd:3:4: warning: something");
    }
}
