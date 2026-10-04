// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Diagnostics reported to the client, conversion of byte offsets to positions, and parsing of
//! GHDL messages (ported from event-cache's `compile_output.rs`).
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

/// The producer of a set of diagnostics. Each source replaces only its own set.
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
        let column = match std::str::from_utf8(line) {
            Ok(line) => line
                .get(..prefix_len)
                .map_or(prefix_len, |prefix| prefix.encode_utf16().count()),
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

/// A diagnostic line of GHDL output: `<file>:<line>:<column>:<severity>:<message>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhdlMessage<'line> {
    /// The file as GHDL printed it; relative paths are relative to GHDL's working directory.
    pub file: &'line str,
    /// The 1-based position.
    pub position: Position,
    /// The severity.
    pub severity: Severity,
    /// The trimmed message.
    pub message: &'line str,
}

impl GhdlMessage<'_> {
    /// Parses a GHDL diagnostic line; returns `None` for any other line.
    ///
    /// Example: `C:\proj\tb.vhd:7:32:warning: example warning`.
    pub fn parse(line: &str) -> Option<GhdlMessage<'_>> {
        [
            (":error:", Severity::Error),
            (":warning:", Severity::Warning),
            (":note:", Severity::Note),
        ]
        .into_iter()
        .find_map(|(marker, severity)| {
            let (location, message) = line.split_once(marker)?;
            let message = message.trim();
            let (location, column) = location.rsplit_once(':')?;
            let (file, line_number) = location.rsplit_once(':')?;
            let column: u32 = column.parse().ok()?;
            let line_number: u32 = line_number.parse().ok()?;
            (!message.is_empty() && !file.is_empty() && line_number > 0 && column > 0).then_some(
                GhdlMessage {
                    file,
                    position: Position {
                        line: line_number,
                        column,
                    },
                    severity,
                    message,
                },
            )
        })
    }

    /// Converts the message into a [`Diagnostic`], resolving a relative file against `cwd`.
    pub fn to_diagnostic(&self, cwd: &Utf8Path) -> Diagnostic {
        Diagnostic::new(self.severity, self.message)
            .in_file(cwd.join(self.file))
            .at(Some(Range {
                start: self.position,
                end: self.position,
            }))
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
    fn ghdl_message_with_location() {
        let line = "/proj/tb_bad_syntax.vhd:4:28:error: missing \";\" at end of use clause";
        assert_eq!(
            GhdlMessage::parse(line),
            Some(GhdlMessage {
                file: "/proj/tb_bad_syntax.vhd",
                position: Position::new(4, 28),
                severity: Severity::Error,
                message: "missing \";\" at end of use clause",
            })
        );
    }

    #[test]
    fn ghdl_message_with_windows_path() {
        let line = r"C:\proj\tb.vhd:7:32:warning: example warning";
        let message = GhdlMessage::parse(line).unwrap();
        assert_eq!(message.file, r"C:\proj\tb.vhd");
        assert_eq!(message.severity, Severity::Warning);
        assert_eq!(message.message, "example warning");
    }

    #[test]
    fn ghdl_message_rejects_other_lines() {
        for line in [
            "use ieee.std_logic_1164.all",
            "                           ^",
            "risim-ghdl:error: compilation error",
            "/a.vhd:0:3:error: zero line",
            "/a.vhd:x:3:error: no line",
            "/a.vhd:3:4:error:   ",
            ":3:4:note: no file",
        ] {
            assert_eq!(GhdlMessage::parse(line), None, "{line}");
        }
    }

    #[test]
    fn ghdl_message_resolves_relative_paths() {
        let message = GhdlMessage::parse("src/a.vhd:1:2:note: hello").unwrap();
        let diagnostic = message.to_diagnostic(Utf8Path::new("/root"));
        assert_eq!(diagnostic.to_string(), "/root/src/a.vhd:1:2: note: hello");
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
