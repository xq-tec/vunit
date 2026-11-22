// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The token-level preprocessor of the Verilog scanner.
//!
//! A port of `vunit/parsing/verilog/preprocess.py`.
//! It handles `` `define `` (with formal arguments and their defaults), `` `undef ``,
//! `` `undefineall ``, `` `ifdef ``, `` `ifndef ``, `` `elsif ``, `` `else ``, `` `endif ``,
//! `` `include `` (also with a macro for the file name), and protected regions, which it skips.
//! It drops `` `timescale ``, `` `default_nettype ``, and `` `unconnected_drive `` with the rest
//! of their line, and every other directive by itself.
//!
//! Deviations from upstream:
//!
//! - A quoted `` `include `` searches the directory of the including file first, then the
//!   include directories, as hydrolyze does; upstream searches the directory of the top file
//!   and then the include directories.
//!   Paths are normalized lexically instead of resolved.
//! - The default of a formal argument may have several tokens and white space around `=`;
//!   upstream takes the token right after `=`.
//!   An empty actual argument takes the default (IEEE 1800 22.5.1); upstream leaves it empty.
//! - `` `resetall `` doesn't undefine the macros (IEEE 1800 22.3); upstream undefines them.
//! - Where upstream raises an exception that nobody catches, such as at the end of the file in
//!   `` `pragma ``, after `` `define NAME `` followed by neither white space nor `(`, or in a
//!   too deep nesting of macros and includes, the directive is dropped with a problem.
//!
//! Problems are collected instead of logged; the scanner logs them at debug level, since
//! hydrolyze reports the real errors.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fmt;
use std::hash::BuildHasher as _;
use std::io;
use std::sync::Arc;

use camino::Utf8Component;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use rustc_hash::FxBuildHasher;
use rustc_hash::FxHashMap;

use super::SvInclude;
use super::tokenizer::Eof;
use super::tokenizer::KeywordSet;
use super::tokenizer::Kind;
use super::tokenizer::Location;
use super::tokenizer::Source;
use super::tokenizer::Token;
use super::tokenizer::TokenStream;
use super::tokenizer::tokenize;

/// How deeply macro expansions, includes, and conditional blocks may nest.
///
/// Upstream relies on Python's recursion limit.
const MAX_DEPTH: usize = 200;

/// The macros defined at a point of the preprocessing.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Defines {
    /// The macros by name.
    macros: FxHashMap<String, Arc<Macro>>,
    /// The sum of the hashes of the names, which identifies the set of names (upstream's
    /// `hash(frozenset(defines.keys()))`).
    names_hash: u64,
}

impl Defines {
    /// Creates an empty set of macros.
    pub fn new() -> Self {
        Self::default()
    }

    /// Defines the macro `name`, replacing a macro of that name.
    ///
    /// `formals` are the formal arguments in their parentheses, such as `(a, b=1)`, and `text`
    /// is the text of the macro, as the `defines` options give them.
    /// Formal arguments that don't parse are ignored.
    pub fn define(&mut self, name: &str, formals: Option<&str>, text: &str) {
        let tokens_of = |option_text: &str| {
            let source = Arc::new(Source {
                path: None,
                contents: Arc::from(option_text.as_bytes()),
            });
            tokenize(&source, KeywordSet::Sv, None)
        };
        let (args, defaults) = formals
            .and_then(|formals| {
                let mut stream = TokenStream::new(tokens_of(formals));
                stream.skip_while(&[Kind::Whitespace]);
                let lpar = stream.pop().ok().filter(|token| token.kind == Kind::LPar)?;
                parse_formals(lpar, &mut stream).ok()
            })
            .unwrap_or_default();
        self.insert(Macro {
            name: name.to_owned(),
            tokens: tokens_of(text),
            args,
            defaults,
        });
    }

    /// Removes the macro `name`, and returns `true` if it was defined.
    pub fn undefine(&mut self, name: &str) -> bool {
        if self.macros.remove(name).is_some() {
            self.names_hash = self.names_hash.wrapping_sub(name_hash(name));
            true
        } else {
            false
        }
    }

    /// Returns `true` if the macro `name` is defined.
    pub fn contains(&self, name: &str) -> bool {
        self.macros.contains_key(name)
    }

    /// Returns the names of the macros, in no particular order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.macros.keys().map(String::as_str)
    }

    /// Returns the number of macros.
    pub fn len(&self) -> usize {
        self.macros.len()
    }

    /// Returns `true` if no macro is defined.
    pub fn is_empty(&self) -> bool {
        self.macros.is_empty()
    }

    /// Returns the macro `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&Arc<Macro>> {
        self.macros.get(name)
    }

    /// Defines `macro_`, replacing a macro of its name.
    pub(crate) fn insert(&mut self, macro_: Macro) {
        let hash = name_hash(&macro_.name);
        if self
            .macros
            .insert(macro_.name.clone(), Arc::new(macro_))
            .is_none()
        {
            self.names_hash = self.names_hash.wrapping_add(hash);
        }
    }

    /// Removes every macro.
    fn clear(&mut self) {
        self.macros.clear();
        self.names_hash = 0;
    }
}

impl fmt::Debug for Defines {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.names().collect();
        names.sort_unstable();
        formatter.debug_tuple("Defines").field(&names).finish()
    }
}

/// Returns the hash of a macro name, a summand of [`Defines::names_hash`].
fn name_hash(name: &str) -> u64 {
    FxBuildHasher.hash_one(name)
}

/// A `` `define `` macro with zero or more formal arguments.
#[derive(Debug, Clone)]
pub(crate) struct Macro {
    /// The name.
    pub(crate) name: String,
    /// The text, without the newline that ends it.
    pub(crate) tokens: Vec<Token>,
    /// The names of the formal arguments.
    pub(crate) args: Vec<String>,
    /// The default of each formal argument, if it has one.
    pub(crate) defaults: Vec<Option<Vec<Token>>>,
}

impl PartialEq for Macro {
    /// Compares the macros without the locations of their tokens.
    fn eq(&self, other: &Self) -> bool {
        let same_tokens = |left: &[Token], right: &[Token]| {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| left.kind == right.kind && left.text() == right.text())
        };
        self.name == other.name
            && self.args == other.args
            && same_tokens(&self.tokens, &other.tokens)
            && self.defaults.len() == other.defaults.len()
            && self
                .defaults
                .iter()
                .zip(&other.defaults)
                .all(|(left, right)| match (left, right) {
                    (Some(left), Some(right)) => same_tokens(left, right),
                    (None, None) => true,
                    _ => false,
                })
    }
}

impl Eq for Macro {}

impl Macro {
    /// Creates the macro `name` without text and arguments.
    const fn empty(name: String) -> Self {
        Self {
            name,
            tokens: Vec::new(),
            args: Vec::new(),
            defaults: Vec::new(),
        }
    }

    /// Expands the macro called by `token`, with its actual arguments from `stream`.
    fn expand_from_stream(
        &self,
        token: &Token,
        stream: &mut TokenStream,
        previous: &Arc<Location>,
    ) -> Result<Vec<Token>, Problem> {
        let mut values = Vec::new();
        if !self.args.is_empty() {
            values = parse_macro_actuals(token, stream)?.map_err(|Eof| {
                Problem::warning("EOF reached when parsing `define actuals", &token.location)
            })?;
            // An empty actual argument takes the default (IEEE 1800 22.5.1).
            for (value, default) in values.iter_mut().zip(&self.defaults) {
                if let Some(default) = default
                    && value.iter().all(|item| item.kind == Kind::Whitespace)
                {
                    value.clone_from(default);
                }
            }
            if values.len() < self.args.len() {
                for index in values.len()..self.args.len() {
                    let Some(Some(default)) = self.defaults.get(index) else {
                        return Err(Problem::warning(
                            format!("Missing value for argument {}", self.args[index]),
                            &token.location,
                        ));
                    };
                    values.push(default.clone());
                }
            } else if values.len() > self.args.len() {
                return Err(Problem::warning(
                    format!(
                        "Too many arguments got {} expected {}",
                        values.len(),
                        self.args.len()
                    ),
                    &token.location,
                ));
            }
        }
        Ok(self.expand(&values, previous))
    }

    /// Expands the macro with the actual arguments `values`.
    fn expand(&self, values: &[Vec<Token>], previous: &Arc<Location>) -> Vec<Token> {
        let mut tokens = Vec::with_capacity(self.tokens.len());
        for token in &self.tokens {
            let arg = (token.kind == Kind::Identifier)
                .then(|| {
                    self.args
                        .iter()
                        .position(|arg| arg.as_bytes() == token.text())
                })
                .flatten();
            match arg {
                Some(index) => tokens.extend(values[index].iter().cloned()),
                None => tokens.push(token.clone()),
            }
        }
        for token in &mut tokens {
            token.location = token.location.with_previous(previous);
        }
        tokens
    }
}

/// Parses the actual arguments of a macro call, such as `1` and `2` in `` `macro(1, 2) ``.
///
/// The outer error is a problem; the inner one the end of the stream.
fn parse_macro_actuals(
    macro_token: &Token,
    stream: &mut TokenStream,
) -> Result<Result<Vec<Vec<Token>>, Eof>, Problem> {
    stream.skip_while(&[Kind::Whitespace]);
    let Ok(token) = stream.pop() else {
        return Ok(Err(Eof));
    };
    if token.kind != Kind::LPar {
        return Err(Problem::warning(
            "Bad `define argument list",
            &macro_token.location,
        ));
    }
    Ok(parse_macro_values(stream))
}

/// Parses the actual arguments after the `(` of a macro call, up to its `)`.
fn parse_macro_values(stream: &mut TokenStream) -> Result<Vec<Vec<Token>>, Eof> {
    let mut value = Vec::new();
    let mut values = Vec::new();
    let (mut bracket_count, mut brace_count, mut par_count) = (0_i32, 0_i32, 0_i32);
    let mut token = stream.pop()?;
    while !(token.kind == Kind::RPar && par_count == 0) {
        match token.kind {
            Kind::LBracket => bracket_count += 1,
            Kind::RBracket => bracket_count -= 1,
            Kind::LBrace => brace_count += 1,
            Kind::RBrace => brace_count -= 1,
            Kind::LPar => par_count += 1,
            Kind::RPar => par_count -= 1,
            _ => {},
        }
        if token.kind == Kind::Comma && bracket_count == 0 && brace_count == 0 && par_count == 0 {
            values.push(std::mem::take(&mut value));
        } else {
            value.push(token);
        }
        token = stream.pop()?;
    }
    values.push(value);
    Ok(values)
}

/// The formal arguments of a macro and their defaults.
type Formals = (Vec<String>, Vec<Option<Vec<Token>>>);

/// Parses the formal arguments of a `` `define `` from its `(` to its `)`.
///
/// Unlike upstream, which takes the token right after `=` for the default, white space may
/// surround `=`, and the default runs to the next `,` or `)` outside parentheses, brackets, and
/// braces, without trailing white space.
/// Like upstream, the first token after `=` and its white space belongs to the default even if
/// it is a `,` or `)`.
fn parse_formals(lpar: Token, stream: &mut TokenStream) -> Result<Formals, Eof> {
    let mut args = Vec::new();
    let mut defaults = Vec::new();
    let mut token = lpar;
    while token.kind != Kind::RPar {
        if token.kind == Kind::Identifier {
            args.push(token.value().into_owned());
            stream.skip_while(&[Kind::Whitespace]);
            token = stream.pop()?;
            let mut default = None;
            if token.kind == Kind::Equal {
                stream.skip_while(&[Kind::Whitespace]);
                let first = stream.pop()?;
                let mut depth = nesting(first.kind);
                let mut value = vec![first];
                token = stream.pop()?;
                while depth > 0 || !matches!(token.kind, Kind::Comma | Kind::RPar) {
                    depth += nesting(token.kind);
                    value.push(token);
                    token = stream.pop()?;
                }
                while value.len() > 1
                    && value
                        .last()
                        .is_some_and(|last| last.kind == Kind::Whitespace)
                {
                    value.pop();
                }
                default = Some(value);
            }
            defaults.push(default);
        } else {
            token = stream.pop()?;
        }
    }
    Ok((args, defaults))
}

/// Returns how a token of the kind `kind` changes the nesting of parentheses, brackets, and
/// braces.
const fn nesting(kind: Kind) -> i32 {
    match kind {
        Kind::LPar | Kind::LBracket | Kind::LBrace => 1,
        Kind::RPar | Kind::RBracket | Kind::RBrace => -1,
        _ => 0,
    }
}

/// How serious a [`Problem`] is, as upstream logs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    /// Expected in valid code, such as a macro of the simulator or a missing include file.
    Debug,
    /// Invalid code.
    Warning,
    /// A circular macro expansion or include.
    Error,
}

/// A problem of the preprocessor or the parser, which upstream logs.
#[derive(Debug, Clone)]
pub(crate) struct Problem {
    /// How serious the problem is.
    pub(crate) severity: Severity,
    /// The message, as upstream words it.
    pub(crate) message: String,
    /// Where the problem is.
    pub(crate) location: Location,
}

impl Problem {
    /// Creates a problem of severity `Debug`.
    pub(crate) fn debug(message: impl Into<String>, location: &Location) -> Self {
        Self::new(Severity::Debug, message, location)
    }

    /// Creates a problem of severity `Warning`.
    pub(crate) fn warning(message: impl Into<String>, location: &Location) -> Self {
        Self::new(Severity::Warning, message, location)
    }

    /// Creates a problem of severity `Error`.
    pub(crate) fn error(message: impl Into<String>, location: &Location) -> Self {
        Self::new(Severity::Error, message, location)
    }

    /// Creates a problem.
    fn new(severity: Severity, message: impl Into<String>, location: &Location) -> Self {
        Self {
            severity,
            message: message.into(),
            location: location.clone(),
        }
    }
}

/// Why a directive stopped: a problem, or the end of the stream.
enum Stop {
    /// A problem.
    Problem(Problem),
    /// The end of the stream.
    Eof,
}

impl From<Problem> for Stop {
    fn from(problem: Problem) -> Self {
        Self::Problem(problem)
    }
}

impl From<Eof> for Stop {
    fn from(Eof: Eof) -> Self {
        Self::Eof
    }
}

/// A point of a macro expansion or include: where the call or directive is, and which macro
/// names are defined there.
struct Point {
    /// The location of the macro call or `` `include `` directive.
    location: Location,
    /// [`Defines::names_hash`] at the point.
    names_hash: u64,
}

impl Point {
    /// Returns `true` if `self` and `other` are the same point.
    fn is_same(&self, other: &Self) -> bool {
        self.names_hash == other.names_hash && self.location.is_same_point(&other.location)
    }
}

/// The preprocessor of one file (upstream's `VerilogPreprocessor`).
pub(crate) struct Preprocessor<'a, R> {
    /// The keywords to tokenize included files with.
    keywords: KeywordSet,
    /// The include directories, searched after the directory of the including file.
    include_dirs: &'a [Utf8PathBuf],
    /// Reads an included file.
    file_reader: R,
    /// The macros defined so far.
    pub(crate) defines: Defines,
    /// The included files, in include order, also those not found.
    pub(crate) included: Vec<SvInclude>,
    /// The problems, in the order they were found.
    pub(crate) problems: Vec<Problem>,
    /// The macro calls being expanded.
    macro_trace: Vec<Point>,
    /// The `` `include `` directives being processed.
    include_trace: Vec<Point>,
    /// How deeply macro expansions, includes, and conditional blocks nest.
    depth: usize,
}

impl<'a, R> Preprocessor<'a, R>
where
    R: FnMut(&Utf8Path) -> io::Result<Arc<[u8]>>,
{
    /// Creates a preprocessor with the macros `defines` and the include directories
    /// `include_dirs`, which reads included files with `file_reader`.
    pub(crate) const fn new(
        keywords: KeywordSet,
        include_dirs: &'a [Utf8PathBuf],
        defines: Defines,
        file_reader: R,
    ) -> Self {
        Self {
            keywords,
            include_dirs,
            file_reader,
            defines,
            included: Vec::new(),
            problems: Vec::new(),
            macro_trace: Vec::new(),
            include_trace: Vec::new(),
            depth: 0,
        }
    }

    /// Preprocesses `tokens` of the file `current`, and returns the tokens with the directives
    /// handled and the macros expanded.
    pub(crate) fn preprocess(&mut self, tokens: Vec<Token>, current: &Source) -> Vec<Token> {
        let mut stream = TokenStream::new(tokens);
        let mut result = Vec::new();
        while let Ok(token) = stream.pop() {
            if token.kind != Kind::Preprocessor {
                result.push(token);
                continue;
            }
            match self.directive(&token, &mut stream, current) {
                Ok(output) => result.extend(output),
                Err(Stop::Problem(problem)) => self.problems.push(problem),
                Err(Stop::Eof) => self.problems.push(Problem::warning(
                    format!("EOF reached when parsing `{}", token.value()),
                    &token.location,
                )),
            }
        }
        result
    }

    /// Handles the directive or macro call `token`.
    fn directive(
        &mut self,
        token: &Token,
        stream: &mut TokenStream,
        current: &Source,
    ) -> Result<Vec<Token>, Stop> {
        match &*token.value() {
            "define" => {
                let macro_ = define(token, stream)?;
                self.defines.insert(macro_);
            },
            "undef" => undef(token, stream, &mut self.defines)?,
            "undefineall" => self.defines.clear(),
            "include" => return self.include(token, stream, current),
            "ifdef" | "ifndef" => {
                let branch = if_statement(token, stream, &self.defines)?;
                self.enter(token)?;
                let result = self.preprocess(branch, current);
                self.depth -= 1;
                return Ok(result);
            },
            "timescale" | "default_nettype" | "unconnected_drive" => {
                stream.skip_until(&[Kind::Newline]);
            },
            "pragma" => {
                stream.skip_while(&[Kind::Whitespace]);
                if stream.pop()?.text() == b"protect" {
                    stream.skip_while(&[Kind::Whitespace]);
                    if stream.pop()?.text() == b"begin_protected" {
                        skip_protected_region(stream)?;
                    }
                }
            },
            // `resetall doesn't affect macros (IEEE 1800 22.3).
            "resetall" | "celldefine" | "endcelldefine" | "nounconnected_drive" => {},
            name if self.defines.contains(name) => {
                return Ok(self.expand_macro(token, stream, current)?);
            },
            _ => {
                return Err(Problem::debug("Verilog undefined name", &token.location).into());
            },
        }
        Ok(Vec::new())
    }

    /// Enters a nested expansion, include, or conditional block at `token`.
    ///
    /// The caller decrements [`Self::depth`] when it leaves.
    fn enter(&mut self, token: &Token) -> Result<(), Problem> {
        if self.depth >= MAX_DEPTH {
            return Err(Problem::error(
                "Macro expansions, includes, and conditional blocks nest too deeply",
                &token.location,
            ));
        }
        self.depth += 1;
        Ok(())
    }

    /// Expands the macro call `macro_token`, and preprocesses the expansion.
    fn expand_macro(
        &mut self,
        macro_token: &Token,
        stream: &mut TokenStream,
        current: &Source,
    ) -> Result<Vec<Token>, Problem> {
        let name = macro_token.value();
        let Some(macro_) = self.defines.get(&name).map(Arc::clone) else {
            return Err(Problem::debug(
                "Verilog undefined name",
                &macro_token.location,
            ));
        };
        let point = Point {
            location: macro_token.location.clone(),
            names_hash: self.defines.names_hash,
        };
        if self.macro_trace.iter().any(|traced| traced.is_same(&point)) {
            return Err(Problem::error(
                format!("Circular macro expansion of {name} detected"),
                &macro_token.location,
            ));
        }
        let previous = Arc::new(macro_token.location.clone());
        let expanded = macro_.expand_from_stream(macro_token, stream, &previous)?;
        self.enter(macro_token)?;
        self.macro_trace.push(point);
        let tokens = self.preprocess(expanded, current);
        self.macro_trace.pop();
        self.depth -= 1;
        Ok(tokens)
    }

    /// Handles the `` `include `` directive `token`, and returns the preprocessed tokens of the
    /// included file.
    fn include(
        &mut self,
        token: &Token,
        stream: &mut TokenStream,
        current: &Source,
    ) -> Result<Vec<Token>, Stop> {
        stream.skip_while(&[Kind::Whitespace]);
        let Ok(argument) = stream.pop() else {
            return Err(Problem::warning(
                "EOF reached when parsing `include argument",
                &token.location,
            )
            .into());
        };
        let file_name_token = match argument.kind {
            Kind::Preprocessor => {
                let name = argument.value();
                if !self.defines.contains(&name) {
                    return Err(Problem::warning(
                        "Verilog `include argument not defined",
                        &argument.location,
                    )
                    .into());
                }
                let expanded = self.expand_macro(&argument, stream, current)?;
                let Some(first) = expanded.into_iter().next() else {
                    return Err(Problem::warning(
                        format!("Verilog `include has bad argument, empty define `{name}"),
                        &argument.location,
                    )
                    .into());
                };
                if first.kind != Kind::String {
                    return Err(Problem::warning(
                        "Verilog `include has bad argument",
                        &first.location,
                    )
                    .into());
                }
                first
            },
            Kind::String => argument,
            _ => {
                return Err(
                    Problem::warning("Verilog `include bad argument", &argument.location).into(),
                );
            },
        };

        let spelling = file_name_token.value().into_owned();
        let found = self.find_included_file(&spelling, current);
        self.included.push(SvInclude {
            spelling: spelling.clone(),
            path: found.as_ref().map(|(path, _)| path.clone()),
        });
        let Some((path, contents)) = found else {
            // Debug, since tools have many builtin include files.
            return Err(Problem::debug(
                format!("Could not find `include file {spelling}"),
                &file_name_token.location,
            )
            .into());
        };

        let point = Point {
            location: token.location.clone(),
            names_hash: self.defines.names_hash,
        };
        if self
            .include_trace
            .iter()
            .any(|traced| traced.is_same(&point))
        {
            return Err(Problem::error(
                format!("Circular `include of {spelling} detected"),
                &file_name_token.location,
            )
            .into());
        }
        self.enter(token)?;
        self.include_trace.push(point);
        let source = Arc::new(Source {
            path: Some(path),
            contents,
        });
        let previous = Arc::new(token.location.clone());
        let tokens = tokenize(&source, self.keywords, Some(&previous));
        let tokens = self.preprocess(tokens, &source);
        self.include_trace.pop();
        self.depth -= 1;
        Ok(tokens)
    }

    /// Finds and reads the file that `` `include "spelling" `` in `current` includes.
    ///
    /// It searches the directory of `current`, then the include directories, as hydrolyze does.
    fn find_included_file(
        &mut self,
        spelling: &str,
        current: &Source,
    ) -> Option<(Utf8PathBuf, Arc<[u8]>)> {
        let own = current
            .path
            .as_deref()
            .and_then(Utf8Path::parent)
            .map(Utf8Path::to_owned);
        for directory in own.iter().chain(self.include_dirs) {
            let candidate = normalize(&directory.join(spelling));
            match (self.file_reader)(&candidate) {
                Ok(contents) => return Some((candidate, contents)),
                Err(error) => {
                    if error.kind() != io::ErrorKind::NotFound {
                        tracing::debug!(path = %candidate, %error, "failed to read an include file");
                    }
                },
            }
        }
        None
    }
}

/// Handles a `` `define `` directive, and returns the macro it defines.
fn define(define_token: &Token, stream: &mut TokenStream) -> Result<Macro, Problem> {
    stream.skip_while(&[Kind::Whitespace, Kind::Newline]);
    let Ok(name_token) = stream.pop() else {
        return Err(Problem::warning(
            "Verilog `define without argument",
            &define_token.location,
        ));
    };
    if name_token.kind != Kind::Identifier {
        return Err(Problem::warning(
            "Verilog `define invalid name",
            &name_token.location,
        ));
    }
    let name = name_token.value().into_owned();

    let Ok(token) = stream.pop() else {
        return Ok(Macro::empty(name));
    };
    let (args, defaults) = match token.kind {
        Kind::Newline => return Ok(Macro::empty(name)),
        Kind::Whitespace => (Vec::new(), Vec::new()),
        Kind::LPar => {
            let location = token.location.clone();
            parse_formals(token, stream).map_err(|Eof| {
                Problem::warning("EOF reached when parsing `define argument list", &location)
            })?
        },
        // Upstream fails here; the text starts right after the name instead.
        _ => {
            stream.unpop();
            (Vec::new(), Vec::new())
        },
    };

    stream.skip_while(&[Kind::Whitespace]);
    let start = stream.index();
    let end = stream.skip_until(&[Kind::Newline]);
    let _newline = stream.pop();
    Ok(Macro {
        name,
        tokens: stream.slice(start, end),
        args,
        defaults,
    })
}

/// Handles an `` `undef `` directive.
fn undef(
    undef_token: &Token,
    stream: &mut TokenStream,
    defines: &mut Defines,
) -> Result<(), Problem> {
    stream.skip_while(&[Kind::Whitespace, Kind::Newline]);
    let Ok(name_token) = stream.pop() else {
        return Err(Problem::warning(
            "EOF reached when parsing `undef",
            &undef_token.location,
        ));
    };
    if name_token.kind != Kind::Identifier {
        return Err(Problem::warning(
            "Bad argument to `undef",
            &name_token.location,
        ));
    }
    if !defines.undefine(&name_token.value()) {
        return Err(Problem::warning(
            "`undef argument was not previously defined",
            &name_token.location,
        ));
    }
    Ok(())
}

/// Handles an `` `ifdef `` or `` `ifndef `` directive up to its `` `endif ``, and returns the
/// tokens of the branch taken, not yet preprocessed.
fn if_statement(
    if_token: &Token,
    stream: &mut TokenStream,
    defines: &Defines,
) -> Result<Vec<Token>, Stop> {
    let mut result = Vec::new();
    stream.skip_while(&[Kind::Whitespace]);
    let arg = stream.pop()?;
    check_if_argument(if_token, &arg, stream)?;

    let mut taken = is_taken(if_token, &arg, defines);
    let mut any_taken = taken;
    let mut count = 1_usize;
    loop {
        let next = stream.pop()?;
        if next.kind == Kind::Preprocessor {
            if next.is_directive("ifdef") || next.is_directive("ifndef") {
                count += 1;
            } else if next.is_directive("endif") {
                count -= 1;
                if count == 0 {
                    break;
                }
            }
        }

        if count == 1 && next.is_directive("else") {
            stream.skip_while(&[Kind::Newline]);
            taken = !any_taken;
            any_taken = true;
        } else if count == 1 && next.is_directive("elsif") {
            stream.skip_while(&[Kind::Whitespace]);
            let elsif_arg = stream.pop()?;
            check_if_argument(&next, &elsif_arg, stream)?;
            stream.skip_while(&[Kind::Newline]);
            if any_taken {
                taken = false;
            } else {
                taken = is_taken(&next, &elsif_arg, defines);
                any_taken = taken;
            }
        } else if taken {
            result.push(next);
        }
    }
    stream.skip_while(&[Kind::Newline]);
    Ok(result)
}

/// Checks the argument `arg` of the conditional directive `directive`, and skips the newlines
/// after it.
fn check_if_argument(
    directive: &Token,
    arg: &Token,
    stream: &mut TokenStream,
) -> Result<(), Problem> {
    if arg.kind != Kind::Identifier {
        return Err(Problem::warning(
            format!("Bad argument to `{}", directive.value()),
            &arg.location,
        ));
    }
    stream.skip_while(&[Kind::Newline]);
    Ok(())
}

/// Returns `true` if the branch of the conditional directive `directive` with the argument `arg`
/// is taken.
fn is_taken(directive: &Token, arg: &Token, defines: &Defines) -> bool {
    let is_defined = defines.contains(&arg.value());
    if directive.is_directive("ifndef") {
        !is_defined
    } else {
        is_defined
    }
}

/// Skips a protected region up to `` `pragma protect end_protected ``.
fn skip_protected_region(stream: &mut TokenStream) -> Result<(), Eof> {
    while !stream.eof() {
        stream.skip_while(&[Kind::Whitespace]);
        let token = stream.pop()?;
        if token.is_directive("pragma") {
            stream.skip_while(&[Kind::Whitespace]);
            if stream.pop()?.text() == b"protect" {
                stream.skip_while(&[Kind::Whitespace]);
                if stream.pop()?.text() == b"end_protected" {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Removes `.` and resolves `..` in `path` without touching the file system, as hydrolyze does.
fn normalize(path: &Utf8Path) -> Utf8PathBuf {
    let mut result = Utf8PathBuf::new();
    for component in path.components() {
        match component {
            Utf8Component::CurDir => {},
            Utf8Component::ParentDir => match result.components().next_back() {
                None | Some(Utf8Component::ParentDir) => result.push(".."),
                Some(Utf8Component::RootDir | Utf8Component::Prefix(_)) => {},
                Some(_) => {
                    result.pop();
                },
            },
            other => result.push(other),
        }
    }
    result
}
