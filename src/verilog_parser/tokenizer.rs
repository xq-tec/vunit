// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The lexer of the Verilog scanner, its keyword table, and the token stream.
//!
//! A port of `vunit/parsing/verilog/tokenizer.py` and `tokens.py`, with the parts of
//! `vunit/parsing/tokenizer.py` that they use (`Token`, `TokenStream`, and the locations).
//! The lexer works on bytes and tries the token kinds in upstream's order: a directive, a string
//! literal, a line comment, an identifier, an escaped newline (dropped), a newline, white space,
//! a block comment, punctuation, and any other character.
//! Like upstream, which reads files with universal newlines, `\r\n` and a lone `\r` are
//! newlines.
//!
//! A token refers to its text in its [`Source`] by a byte range, and its [`Location`] chains the
//! macro calls and `` `include `` directives that brought it in, as upstream's locations do.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use camino::Utf8PathBuf;

/// The keywords that the lexer recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeywordSet {
    /// The keywords of IEEE 1800, for SystemVerilog files.
    #[default]
    Sv,
    /// The keywords of IEEE 1364-2005, for Verilog files.
    ///
    /// The keywords that only IEEE 1800 reserves, such as `package`, `import`, and `interface`,
    /// are identifiers.
    Verilog1364,
}

impl KeywordSet {
    /// Returns the kind of the identifier or keyword `text`.
    pub(crate) fn classify(self, text: &[u8]) -> Kind {
        let Ok(index) = KEYWORDS.binary_search_by(|(keyword, _)| keyword.as_bytes().cmp(text))
        else {
            return Kind::Identifier;
        };
        let (keyword, verilog) = KEYWORDS[index];
        if self == Self::Verilog1364 && !verilog {
            return Kind::Identifier;
        }
        Kind::Keyword(Keyword::from_text(keyword))
    }
}

/// A keyword that the scanner treats specially; all others are [`Keyword::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keyword {
    /// `automatic`
    Automatic,
    /// `begin`
    Begin,
    /// `class`
    Class,
    /// `end`
    End,
    /// `endinterface`
    Endinterface,
    /// `endmodule`
    Endmodule,
    /// `endprimitive`
    Endprimitive,
    /// `endprogram`
    Endprogram,
    /// `extern`
    Extern,
    /// `import`
    Import,
    /// `interface`
    Interface,
    /// `macromodule`
    Macromodule,
    /// `module`
    Module,
    /// `package`
    Package,
    /// `parameter`
    Parameter,
    /// `primitive`
    Primitive,
    /// `program`
    Program,
    /// `static`
    Static,
    /// `virtual`
    Virtual,
    /// Any other keyword.
    Other,
}

impl Keyword {
    /// Returns the keyword of the reserved word `text`.
    fn from_text(text: &str) -> Self {
        match text {
            "automatic" => Self::Automatic,
            "begin" => Self::Begin,
            "class" => Self::Class,
            "end" => Self::End,
            "endinterface" => Self::Endinterface,
            "endmodule" => Self::Endmodule,
            "endprimitive" => Self::Endprimitive,
            "endprogram" => Self::Endprogram,
            "extern" => Self::Extern,
            "import" => Self::Import,
            "interface" => Self::Interface,
            "macromodule" => Self::Macromodule,
            "module" => Self::Module,
            "package" => Self::Package,
            "parameter" => Self::Parameter,
            "primitive" => Self::Primitive,
            "program" => Self::Program,
            "static" => Self::Static,
            "virtual" => Self::Virtual,
            _ => Self::Other,
        }
    }
}

/// The kind of a token.
///
/// Upstream gives every keyword a kind of its own; here, the keywords that the scanner doesn't
/// treat specially share [`Keyword::Other`], and the text tells them apart.
/// Unlike upstream, where the keyword `string` and string literals share a kind, the keyword is
/// a keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A compiler directive or macro call: `` ` `` and an identifier.
    Preprocessor,
    /// A string literal.
    String,
    /// A line comment.
    Comment,
    /// An identifier that isn't a keyword.
    Identifier,
    /// A newline.
    Newline,
    /// Spaces and tabs.
    Whitespace,
    /// A block comment.
    MultiComment,
    /// `:`
    Colon,
    /// `;`
    SemiColon,
    /// `::`
    DoubleColon,
    /// `#`
    Hash,
    /// `=`
    Equal,
    /// `(`
    LPar,
    /// `)`
    RPar,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `,`
    Comma,
    /// Any other character.
    Other,
    /// A keyword of the [`KeywordSet`] of the lexer.
    Keyword(Keyword),
}

impl Kind {
    /// Returns `true` for identifiers and keywords, the tokens that can name a design element.
    pub(crate) const fn is_name(self) -> bool {
        matches!(self, Self::Identifier | Self::Keyword(_))
    }
}

/// A text that tokens refer to: a file, or the text of a macro defined by an option.
#[derive(Debug)]
pub(crate) struct Source {
    /// The path of the file, or `None` for the text of a macro defined by an option.
    pub(crate) path: Option<Utf8PathBuf>,
    /// The bytes of the text.
    pub(crate) contents: Arc<[u8]>,
}

impl Source {
    /// Returns `true` if `self` and `other` are the same text: the same file, or the same macro
    /// text.
    fn is_same(&self, other: &Self) -> bool {
        match (&self.path, &other.path) {
            (Some(path), Some(other_path)) => path == other_path,
            _ => std::ptr::eq(self, other),
        }
    }
}

/// Where a token is: its bytes in its source, and where the macro call or `` `include ``
/// directive that brought it in is (upstream's `previous`).
#[derive(Debug, Clone)]
pub(crate) struct Location {
    /// The source of the token.
    pub(crate) source: Arc<Source>,
    /// The bytes of the token in its source.
    pub(crate) range: Range<usize>,
    /// The location of the macro call or `` `include `` directive that brought the token in.
    pub(crate) previous: Option<Arc<Self>>,
}

impl Location {
    /// Returns the location with `previous` appended to the end of its chain (upstream's
    /// `add_previous`).
    pub(crate) fn with_previous(&self, previous: &Arc<Self>) -> Self {
        Self {
            source: Arc::clone(&self.source),
            range: self.range.clone(),
            previous: Some(match self.previous {
                None => Arc::clone(previous),
                Some(ref own) => Arc::new(own.with_previous(previous)),
            }),
        }
    }

    /// Returns `true` if `self` and `other` are at the same bytes of the same source, regardless
    /// of their chains (upstream compares `strip_previous` of both).
    pub(crate) fn is_same_point(&self, other: &Self) -> bool {
        self.range == other.range && self.source.is_same(&other.source)
    }

    /// Returns `true` if the token is spelled in `source` itself, not brought in by a macro call
    /// or an `` `include `` directive.
    pub(crate) fn is_spelled_in(&self, source: &Arc<Source>) -> bool {
        self.previous.is_none() && Arc::ptr_eq(&self.source, source)
    }

    /// Describes the location and its chain, innermost first, as `path:line` items.
    pub(crate) fn describe(&self) -> String {
        let mut description = self.describe_one();
        let mut previous = self.previous.as_deref();
        while let Some(location) = previous {
            description.push_str(" from ");
            description.push_str(&location.describe_one());
            previous = location.previous.as_deref();
        }
        description
    }

    /// Describes the location without its chain as `path:line`.
    fn describe_one(&self) -> String {
        let contents = &self.source.contents;
        let end = self.range.start.min(contents.len());
        let line = contents[..end]
            .split(|&character| character == b'\n')
            .count();
        match self.source.path {
            Some(ref path) => format!("{path}:{line}"),
            None => format!("<macro defined by an option>:{line}"),
        }
    }
}

/// A token: its kind and its location.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    /// The kind of the token.
    pub(crate) kind: Kind,
    /// Where the token is.
    pub(crate) location: Location,
}

impl Token {
    /// Returns the raw bytes of the value of the token.
    ///
    /// The value of a directive leaves out the `` ` ``, that of a string literal its quotes,
    /// that of a comment its delimiters; newlines and punctuation have an empty value.
    pub(crate) fn text(&self) -> &[u8] {
        let Range { start, end } = self.location.range;
        let (start, end) = match self.kind {
            Kind::Preprocessor => (start + 1, end),
            Kind::String => (start + 1, end - 1),
            Kind::Comment => (start + 2, end),
            Kind::MultiComment => (start + 2, end - 2),
            Kind::Newline
            | Kind::Colon
            | Kind::SemiColon
            | Kind::DoubleColon
            | Kind::Hash
            | Kind::Equal
            | Kind::LPar
            | Kind::RPar
            | Kind::LBracket
            | Kind::RBracket
            | Kind::LBrace
            | Kind::RBrace
            | Kind::Comma => (start, start),
            Kind::Identifier | Kind::Whitespace | Kind::Other | Kind::Keyword(_) => (start, end),
        };
        &self.location.source.contents[start..end]
    }

    /// Returns the value of the token as upstream has it.
    ///
    /// In a string literal, escaped newlines are removed and `\"` becomes `"`.
    pub(crate) fn value(&self) -> Cow<'_, str> {
        let text = self.text();
        if self.kind == Kind::String && text.contains(&b'\\') {
            let text = replace(text, b"\\\r\n", b"");
            let text = replace(&text, b"\\\n", b"");
            let text = replace(&text, b"\\\r", b"");
            let text = replace(&text, b"\\\"", b"\"");
            return Cow::Owned(String::from_utf8_lossy(&text).into_owned());
        }
        String::from_utf8_lossy(text)
    }

    /// Returns `true` if the token is the directive or macro call `name`.
    pub(crate) fn is_directive(&self, name: &str) -> bool {
        self.kind == Kind::Preprocessor && self.text() == name.as_bytes()
    }
}

/// Returns `text` with every occurrence of `from` replaced by `to`.
fn replace(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if rest.starts_with(from) {
            result.extend_from_slice(to);
            rest = &rest[from.len()..];
        } else {
            result.push(rest[0]);
            rest = &rest[1..];
        }
    }
    result
}

/// Splits the text of `source` into tokens, with `previous` as the end of each token's chain.
///
/// Escaped newlines are dropped, as upstream drops them.
pub(crate) fn tokenize(
    source: &Arc<Source>,
    keywords: KeywordSet,
    previous: Option<&Arc<Location>>,
) -> Vec<Token> {
    let contents = &*source.contents;
    let mut tokens = Vec::new();
    let mut position = 0;
    let mut comments_end = true;
    while position < contents.len() {
        let (kind, end) = lex(contents, position, keywords, &mut comments_end);
        if let Some(kind) = kind {
            tokens.push(Token {
                kind,
                location: Location {
                    source: Arc::clone(source),
                    range: position..end,
                    previous: previous.map(Arc::clone),
                },
            });
        }
        position = end;
    }
    tokens
}

/// Lexes the token at `start` of `contents`, and returns its kind, `None` for an escaped
/// newline, and its end.
///
/// `comments_end` is `false` once a `/*` had no `*/` after it, so that the lexer doesn't search
/// the rest of the text again for the next `/*`.
fn lex(
    contents: &[u8],
    start: usize,
    keywords: KeywordSet,
    comments_end: &mut bool,
) -> (Option<Kind>, usize) {
    let first = contents[start];
    let next = contents.get(start + 1).copied();
    let word_end = |from: usize| {
        from + contents[from..]
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
            .count()
    };
    match first {
        b'`' if next.is_some_and(|next| next.is_ascii_alphabetic()) => {
            return (Some(Kind::Preprocessor), word_end(start + 1));
        },
        b'"' if start == 0 || contents[start - 1] != b'\\' => {
            if let Some(end) = string_end(contents, start + 1) {
                return (Some(Kind::String), end);
            }
        },
        b'/' if next == Some(b'/') => {
            let length = contents[start..]
                .iter()
                .take_while(|byte| !matches!(byte, b'\n' | b'\r'))
                .count();
            return (Some(Kind::Comment), start + length);
        },
        b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
            let end = word_end(start);
            return (Some(keywords.classify(&contents[start..end])), end);
        },
        b'\\' if matches!(next, Some(b'\n' | b'\r')) => {
            return (None, newline_end(contents, start + 1));
        },
        b'\n' | b'\r' => return (Some(Kind::Newline), newline_end(contents, start)),
        b' ' | b'\t' => {
            let length = contents[start..]
                .iter()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            return (Some(Kind::Whitespace), start + length);
        },
        _ => {},
    }
    if first == b'/' && next == Some(b'*') && *comments_end {
        match find(&contents[start + 2..], b"*/") {
            Some(offset) => return (Some(Kind::MultiComment), start + 2 + offset + 2),
            None => *comments_end = false,
        }
    }
    let kind = match first {
        b':' if next == Some(b':') => return (Some(Kind::DoubleColon), start + 2),
        b':' => Kind::Colon,
        b';' => Kind::SemiColon,
        b'#' => Kind::Hash,
        b'=' => Kind::Equal,
        b'(' => Kind::LPar,
        b')' => Kind::RPar,
        b'[' => Kind::LBracket,
        b']' => Kind::RBracket,
        b'{' => Kind::LBrace,
        b'}' => Kind::RBrace,
        b',' => Kind::Comma,
        _ => {
            // One character, with the continuation bytes of its UTF-8 encoding.
            let length = 1 + contents[start + 1..]
                .iter()
                .take_while(|byte| (0x80..0xc0).contains(*byte))
                .count();
            let length = if first >= 0xc0 { length } else { 1 };
            return (Some(Kind::Other), start + length);
        },
    };
    (Some(kind), start + 1)
}

/// Returns the end of the newline at `start`: `\r\n`, `\n`, or `\r`.
fn newline_end(contents: &[u8], start: usize) -> usize {
    if contents[start] == b'\r' && contents.get(start + 1) == Some(&b'\n') {
        start + 2
    } else {
        start + 1
    }
}

/// Returns the end of the string literal whose text starts at `start`: after the first `"`
/// that doesn't follow a `\`.
fn string_end(contents: &[u8], start: usize) -> Option<usize> {
    (start..contents.len())
        .find(|&index| contents[index] == b'"' && contents[index - 1] != b'\\')
        .map(|index| index + 1)
}

/// Returns the offset of the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A stream of tokens (upstream's `TokenStream`).
pub(crate) struct TokenStream {
    /// The tokens.
    tokens: Vec<Token>,
    /// The index of the current token.
    index: usize,
}

/// The end of a [`TokenStream`] was reached (upstream's `EOFException`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Eof;

impl TokenStream {
    /// Creates the stream of `tokens`.
    pub(crate) const fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, index: 0 }
    }

    /// Returns `true` if every token has been popped.
    pub(crate) const fn eof(&self) -> bool {
        self.index >= self.tokens.len()
    }

    /// Returns the index of the current token.
    pub(crate) const fn index(&self) -> usize {
        self.index
    }

    /// Skips tokens of the kinds `kinds`, and returns the index of the current token.
    pub(crate) fn skip_while(&mut self, kinds: &[Kind]) -> usize {
        while self
            .tokens
            .get(self.index)
            .is_some_and(|token| kinds.contains(&token.kind))
        {
            self.index += 1;
        }
        self.index
    }

    /// Skips tokens until one of the kinds `kinds`, and returns the index of the current token.
    pub(crate) fn skip_until(&mut self, kinds: &[Kind]) -> usize {
        while self
            .tokens
            .get(self.index)
            .is_some_and(|token| !kinds.contains(&token.kind))
        {
            self.index += 1;
        }
        self.index
    }

    /// Returns the current token and advances the stream.
    pub(crate) fn pop(&mut self) -> Result<Token, Eof> {
        let token = self.tokens.get(self.index).ok_or(Eof)?.clone();
        self.index += 1;
        Ok(token)
    }

    /// Steps back to the token popped last.
    pub(crate) const fn unpop(&mut self) {
        self.index -= 1;
    }

    /// Returns the tokens from `start` to `end`.
    pub(crate) fn slice(&self, start: usize, end: usize) -> Vec<Token> {
        self.tokens[start..end].to_vec()
    }
}

/// The reserved words of IEEE 1800-2023 (Annex B) in byte order, each with whether IEEE
/// 1364-2005 reserves it too.
const KEYWORDS: [(&str, bool); 248] = [
    ("accept_on", false),
    ("alias", false),
    ("always", true),
    ("always_comb", false),
    ("always_ff", false),
    ("always_latch", false),
    ("and", true),
    ("assert", false),
    ("assign", true),
    ("assume", false),
    ("automatic", true),
    ("before", false),
    ("begin", true),
    ("bind", false),
    ("bins", false),
    ("binsof", false),
    ("bit", false),
    ("break", false),
    ("buf", true),
    ("bufif0", true),
    ("bufif1", true),
    ("byte", false),
    ("case", true),
    ("casex", true),
    ("casez", true),
    ("cell", true),
    ("chandle", false),
    ("checker", false),
    ("class", false),
    ("clocking", false),
    ("cmos", true),
    ("config", true),
    ("const", false),
    ("constraint", false),
    ("context", false),
    ("continue", false),
    ("cover", false),
    ("covergroup", false),
    ("coverpoint", false),
    ("cross", false),
    ("deassign", true),
    ("default", true),
    ("defparam", true),
    ("design", true),
    ("disable", true),
    ("dist", false),
    ("do", false),
    ("edge", true),
    ("else", true),
    ("end", true),
    ("endcase", true),
    ("endchecker", false),
    ("endclass", false),
    ("endclocking", false),
    ("endconfig", true),
    ("endfunction", true),
    ("endgenerate", true),
    ("endgroup", false),
    ("endinterface", false),
    ("endmodule", true),
    ("endpackage", false),
    ("endprimitive", true),
    ("endprogram", false),
    ("endproperty", false),
    ("endsequence", false),
    ("endspecify", true),
    ("endtable", true),
    ("endtask", true),
    ("enum", false),
    ("event", true),
    ("eventually", false),
    ("expect", false),
    ("export", false),
    ("extends", false),
    ("extern", false),
    ("final", false),
    ("first_match", false),
    ("for", true),
    ("force", true),
    ("foreach", false),
    ("forever", true),
    ("fork", true),
    ("forkjoin", false),
    ("function", true),
    ("generate", true),
    ("genvar", true),
    ("global", false),
    ("highz0", true),
    ("highz1", true),
    ("if", true),
    ("iff", false),
    ("ifnone", true),
    ("ignore_bins", false),
    ("illegal_bins", false),
    ("implements", false),
    ("implies", false),
    ("import", false),
    ("incdir", true),
    ("include", true),
    ("initial", true),
    ("inout", true),
    ("input", true),
    ("inside", false),
    ("instance", true),
    ("int", false),
    ("integer", true),
    ("interconnect", false),
    ("interface", false),
    ("intersect", false),
    ("join", true),
    ("join_any", false),
    ("join_none", false),
    ("large", true),
    ("let", false),
    ("liblist", true),
    ("library", true),
    ("local", false),
    ("localparam", true),
    ("logic", false),
    ("longint", false),
    ("macromodule", true),
    ("matches", false),
    ("medium", true),
    ("modport", false),
    ("module", true),
    ("nand", true),
    ("negedge", true),
    ("nettype", false),
    ("new", false),
    ("nexttime", false),
    ("nmos", true),
    ("nor", true),
    ("noshowcancelled", true),
    ("not", true),
    ("notif0", true),
    ("notif1", true),
    ("null", false),
    ("or", true),
    ("output", true),
    ("package", false),
    ("packed", false),
    ("parameter", true),
    ("pmos", true),
    ("posedge", true),
    ("primitive", true),
    ("priority", false),
    ("program", false),
    ("property", false),
    ("protected", false),
    ("pull0", true),
    ("pull1", true),
    ("pulldown", true),
    ("pullup", true),
    ("pulsestyle_ondetect", true),
    ("pulsestyle_onevent", true),
    ("pure", false),
    ("rand", false),
    ("randc", false),
    ("randcase", false),
    ("randsequence", false),
    ("rcmos", true),
    ("real", true),
    ("realtime", true),
    ("ref", false),
    ("reg", true),
    ("reject_on", false),
    ("release", true),
    ("repeat", true),
    ("restrict", false),
    ("return", false),
    ("rnmos", true),
    ("rpmos", true),
    ("rtran", true),
    ("rtranif0", true),
    ("rtranif1", true),
    ("s_always", false),
    ("s_eventually", false),
    ("s_nexttime", false),
    ("s_until", false),
    ("s_until_with", false),
    ("scalared", true),
    ("sequence", false),
    ("shortint", false),
    ("shortreal", false),
    ("showcancelled", true),
    ("signed", true),
    ("small", true),
    ("soft", false),
    ("solve", false),
    ("specify", true),
    ("specparam", true),
    ("static", false),
    ("string", false),
    ("strong", false),
    ("strong0", true),
    ("strong1", true),
    ("struct", false),
    ("super", false),
    ("supply0", true),
    ("supply1", true),
    ("sync_accept_on", false),
    ("sync_reject_on", false),
    ("table", true),
    ("tagged", false),
    ("task", true),
    ("this", false),
    ("throughout", false),
    ("time", true),
    ("timeprecision", false),
    ("timeunit", false),
    ("tran", true),
    ("tranif0", true),
    ("tranif1", true),
    ("tri", true),
    ("tri0", true),
    ("tri1", true),
    ("triand", true),
    ("trior", true),
    ("trireg", true),
    ("type", false),
    ("typedef", false),
    ("union", false),
    ("unique", false),
    ("unique0", false),
    ("unsigned", true),
    ("until", false),
    ("until_with", false),
    ("untyped", false),
    ("use", true),
    ("uwire", true),
    ("var", false),
    ("vectored", true),
    ("virtual", false),
    ("void", false),
    ("wait", true),
    ("wait_order", false),
    ("wand", true),
    ("weak", false),
    ("weak0", true),
    ("weak1", true),
    ("while", true),
    ("wildcard", false),
    ("wire", true),
    ("with", false),
    ("within", false),
    ("wor", true),
    ("xnor", true),
    ("xor", true),
];
