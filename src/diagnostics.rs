// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Diagnostics reported to the client, and conversion of byte offsets to positions.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fmt;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, column: u32) -> Position {
        Position { line, column }
    }

    #[test]
    fn positions_are_one_based() {
        let index = LineIndex::new(b"ab\ncd\n");
        assert_eq!(index.position(0), pos(1, 1));
        assert_eq!(index.position(1), pos(1, 2));
        assert_eq!(index.position(3), pos(2, 1));
        assert_eq!(index.position(5), pos(2, 3));
        assert_eq!(index.position(6), pos(3, 1));
        assert_eq!(index.position(100), pos(3, 1));
    }

    #[test]
    fn utf8_lines_count_utf16_code_units() {
        // "ä" is two bytes and one UTF-16 code unit; "𝄞" is four bytes and two code units.
        let text = "äx𝄞y\n".as_bytes();
        let index = LineIndex::new(text);
        assert_eq!(index.position(2), pos(1, 2));
        assert_eq!(index.position(7), pos(1, 5));
    }

    #[test]
    fn non_utf8_lines_count_bytes() {
        let text = b"\xe4x\ny";
        let index = LineIndex::new(text);
        assert_eq!(index.position(1), pos(1, 2));
        assert_eq!(index.position(3), pos(2, 1));
    }

    #[test]
    fn range_end_is_last_character() {
        let index = LineIndex::new(b"entity foo is");
        let range = index.range(7..10);
        assert_eq!(range.start, pos(1, 8));
        assert_eq!(range.end, pos(1, 10));
    }

    #[test]
    fn display_includes_location() {
        let diagnostic = Diagnostic::warning("something")
            .in_file("/a/b.vhd")
            .at(Some(Range {
                start: pos(3, 4),
                end: pos(3, 5),
            }));
        assert_eq!(diagnostic.to_string(), "/a/b.vhd:3:4: warning: something");
    }
}
