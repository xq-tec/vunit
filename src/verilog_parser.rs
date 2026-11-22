// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Token-level scanning of SystemVerilog and Verilog files for design elements and references.
//!
//! A port of `vunit/parsing/verilog/` (`tokenizer.py`, `tokens.py`, `preprocess.py`, and
//! `parser.py`), without its parse cache.
//! Like VUnit, the scanner doesn't parse SystemVerilog: a lexer splits the bytes of a file into
//! tokens, a preprocessor handles macros, conditional compilation, and includes on the tokens,
//! and finders look for short token patterns, such as `module name` or `name name` for an
//! instance.
//! Its results order the compilation; a mistake costs a recompile, since hydrolyze orders the
//! design units within a call and reports the real errors.
//!
//! The deviations from upstream are documented in the modules: the lexer and the keyword sets
//! in `tokenizer`, the include search and directives in `preprocessor`, and the definitions and
//! references in `parser`.
//! Beyond upstream, [`scan`] returns the macros defined at the end of the file, so that a
//! library that is one compilation unit carries them into its next file.
//!
//! AI NOTICE: Generated, minimally reviewed.

mod parser;
mod preprocessor;
#[cfg(test)]
mod tests;
mod tokenizer;

use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;

pub use self::preprocessor::Defines;
use self::preprocessor::Preprocessor;
use self::preprocessor::Problem;
pub use self::tokenizer::KeywordSet;
use self::tokenizer::Source;
use crate::diagnostics;
use crate::diagnostics::LineIndex;

/// The input of [`scan`]: a file, its contents, and its options.
#[derive(Debug, Clone, Copy)]
pub struct ScanInput<'a> {
    /// The path of the file, which also locates quoted includes.
    pub path: &'a Utf8Path,
    /// The contents of the file.
    pub contents: &'a [u8],
    /// The include directories, searched in order after the directory of the including file.
    pub include_dirs: &'a [Utf8PathBuf],
    /// The macros defined at the start of the file: those of the options, and in a library that
    /// is one compilation unit, those carried from the previous file.
    pub defines: &'a Defines,
    /// The keywords of the file.
    pub keywords: KeywordSet,
}

/// The design elements and references found in a SystemVerilog or Verilog file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SvDesignFile {
    /// The definitions: modules, macromodules, interfaces, programs, and primitives.
    ///
    /// Nested definitions are ignored.
    pub definitions: Vec<SvDefinition>,
    /// The packages.
    pub packages: Vec<SvName>,
    /// The imported packages, `p` of `import p::…`, in order.
    pub imports: Vec<String>,
    /// The package references, `p` of `p::x`, in order.
    pub package_references: Vec<String>,
    /// The instantiated or port types, `m` of `m name` and `m #`, and `i` of `i.modport name`,
    /// in order: instances of definitions and interface ports, among others.
    pub instances: Vec<String>,
    /// The `` `include `` directives, flattened, in include order.
    pub includes: Vec<SvInclude>,
}

/// A definition: a module, macromodule, interface, program, or primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvDefinition {
    /// The name.
    pub name: String,
    /// The kind of definition.
    pub kind: SvDefinitionKind,
    /// The names of the parameters declared with `parameter` in the definition, in order.
    pub parameters: Vec<String>,
    /// The bytes of the name in the file, if the file spells it itself, not an include or a
    /// macro.
    pub name_range: Option<Range<usize>>,
    /// The lines and columns of [`name_range`](Self::name_range), for diagnostics.
    pub name_location: Option<diagnostics::Range>,
}

/// The kind of a [`SvDefinition`], by its keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SvDefinitionKind {
    /// `module`
    Module,
    /// `macromodule`
    Macromodule,
    /// `interface`
    Interface,
    /// `program`
    Program,
    /// `primitive`
    Primitive,
}

impl fmt::Display for SvDefinitionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Module => "module",
            Self::Macromodule => "macromodule",
            Self::Interface => "interface",
            Self::Program => "program",
            Self::Primitive => "primitive",
        })
    }
}

/// A named design element other than a definition: a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvName {
    /// The name.
    pub name: String,
    /// The bytes of the name in the file, if the file spells it itself, not an include or a
    /// macro.
    pub name_range: Option<Range<usize>>,
    /// The lines and columns of [`name_range`](Self::name_range), for diagnostics.
    pub name_location: Option<diagnostics::Range>,
}

/// An `` `include `` directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvInclude {
    /// The file name as the directive spells it, without quotes.
    pub spelling: String,
    /// The file found, or `None` if none was.
    pub path: Option<Utf8PathBuf>,
}

/// Scans a SystemVerilog or Verilog file, and returns what it finds and the macros defined at
/// its end.
///
/// `file_reader` reads included files; a quoted `` `include `` searches the directory of the
/// including file, then the include directories of `input`, and takes the first file that
/// `file_reader` reads.
/// Problems, such as a missing include file or a malformed directive, are logged at debug level.
pub fn scan(
    input: &ScanInput<'_>,
    file_reader: impl FnMut(&Utf8Path) -> io::Result<Arc<[u8]>>,
) -> (SvDesignFile, Defines) {
    let (design_file, defines, problems) = scan_with_problems(input, file_reader);
    for problem in problems {
        tracing::debug!(
            path = %input.path,
            location = problem.location.describe(),
            severity = ?problem.severity,
            "{}",
            problem.message
        );
    }
    (design_file, defines)
}

/// Scans a file like [`scan`], and returns its problems instead of logging them.
fn scan_with_problems(
    input: &ScanInput<'_>,
    file_reader: impl FnMut(&Utf8Path) -> io::Result<Arc<[u8]>>,
) -> (SvDesignFile, Defines, Vec<Problem>) {
    let source = Arc::new(Source {
        path: Some(input.path.to_owned()),
        contents: Arc::from(input.contents),
    });
    let tokens = tokenizer::tokenize(&source, input.keywords, None);
    let mut preprocessor = Preprocessor::new(
        input.keywords,
        input.include_dirs,
        input.defines.clone(),
        file_reader,
    );
    let tokens = preprocessor.preprocess(tokens, &source);
    let Preprocessor {
        defines,
        included,
        mut problems,
        ..
    } = preprocessor;

    let tokens = parser::significant_tokens(tokens, input.keywords);
    let mut design_file = SvDesignFile {
        definitions: parser::find_definitions(&tokens, &source),
        packages: parser::find_packages(&tokens, &source),
        imports: parser::find_imports(&tokens, &mut problems),
        package_references: parser::find_package_references(&tokens),
        instances: parser::find_instances(&tokens),
        includes: included,
    };
    let lines = LineIndex::new(input.contents);
    let names = design_file
        .definitions
        .iter_mut()
        .map(|definition| (&definition.name_range, &mut definition.name_location))
        .chain(
            design_file
                .packages
                .iter_mut()
                .map(|package| (&package.name_range, &mut package.name_location)),
        );
    for (range, location) in names {
        *location = range.clone().map(|range| lines.range(range));
    }
    (design_file, defines, problems)
}
