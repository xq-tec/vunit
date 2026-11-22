// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests ported from `tests/unit/test_verilog_tokenizer.py`, `test_verilog_preprocessor.py`,
//! and `test_verilog_parser.py`, and tests of the deviations.
//!
//! The parse cache tests of `test_verilog_parser.py` aren't ported, since the scanner has no
//! cache.
//! Upstream asserts on log calls; these tests assert on the problems, with the spelled text of
//! each location of their chain, innermost first.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::io;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;

use super::preprocessor::Macro;
use super::preprocessor::Preprocessor;
use super::preprocessor::Problem;
use super::preprocessor::Severity;
use super::tokenizer::Keyword;
use super::tokenizer::Kind;
use super::tokenizer::Location;
use super::tokenizer::Source;
use super::tokenizer::Token;
use super::tokenizer::tokenize;
use super::*;

/// The directory of the files of a test.
const OUT: &str = "/out";

/// Returns a source of `code` at `path`.
fn source(path: &str, code: &str) -> Arc<Source> {
    Arc::new(Source {
        path: Some(Utf8PathBuf::from(path)),
        contents: Arc::from(code.as_bytes()),
    })
}

/// Tokenizes `code` as the file `/out/fn.v` with the SystemVerilog keywords.
fn tokenize_code(code: &str) -> Vec<Token> {
    tokenize(&source("/out/fn.v", code), KeywordSet::Sv, None)
}

/// Returns the kinds and values of `tokens`.
fn kinds_and_values(tokens: &[Token]) -> Vec<(Kind, String)> {
    tokens
        .iter()
        .map(|token| (token.kind, token.value().into_owned()))
        .collect()
}

/// Describes the chain of `location`, innermost first, as `file:line:text` items.
fn chain(location: &Location) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = Some(location);
    while let Some(item) = current {
        let contents = &item.source.contents;
        let line = contents[..item.range.start]
            .split(|&byte| byte == b'\n')
            .count();
        let name = item
            .source
            .path
            .as_deref()
            .and_then(Utf8Path::file_name)
            .unwrap_or("?");
        let text = String::from_utf8_lossy(&contents[item.range.clone()]);
        result.push(format!("{name}:{line}:{text}"));
        current = item.previous.as_deref();
    }
    result
}

/// Returns a file reader of the in-memory files `files`.
fn reader(files: &[(&str, &str)]) -> impl FnMut(&Utf8Path) -> io::Result<Arc<[u8]>> + use<> {
    let files: FxHashMap<Utf8PathBuf, Arc<[u8]>> = files
        .iter()
        .map(|&(path, contents)| (Utf8PathBuf::from(path), Arc::from(contents.as_bytes())))
        .collect();
    move |path: &Utf8Path| {
        files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

mod tokenizer_tests {
    use super::*;

    /// Tokenizes `code` and checks the kinds and values of its tokens.
    fn check(code: &str, expected: &[(Kind, &str)]) {
        let expected: Vec<(Kind, String)> = expected
            .iter()
            .map(|&(kind, value)| (kind, value.to_owned()))
            .collect();
        assert_eq!(kinds_and_values(&tokenize_code(code)), expected, "{code:?}");
    }

    #[test]
    fn tokenizes_define() {
        check(
            "`define name",
            &[
                (Kind::Preprocessor, "define"),
                (Kind::Whitespace, " "),
                (Kind::Identifier, "name"),
            ],
        );
    }

    #[test]
    fn newline_is_not_whitespace() {
        check(
            " \n  \n\n",
            &[
                (Kind::Whitespace, " "),
                (Kind::Newline, ""),
                (Kind::Whitespace, "  "),
                (Kind::Newline, ""),
                (Kind::Newline, ""),
            ],
        );
    }

    #[test]
    fn tokenizes_string_literal() {
        check(r#""hello""#, &[(Kind::String, "hello")]);
        check(
            r#""hel""lo""#,
            &[(Kind::String, "hel"), (Kind::String, "lo")],
        );
        check(r#""h\"ello""#, &[(Kind::String, "h\"ello")]);
        check(r#""\"ello""#, &[(Kind::String, "\"ello")]);
        check(r#""\"\"""#, &[(Kind::String, "\"\"")]);
        check("\"hi\nthere\"", &[(Kind::String, "hi\nthere")]);
        check("\"hi\\\nthere\"", &[(Kind::String, "hithere")]);
    }

    #[test]
    fn tokenizes_single_line_comment() {
        check("// asd", &[(Kind::Comment, " asd")]);
        check(
            "asd// asd",
            &[(Kind::Identifier, "asd"), (Kind::Comment, " asd")],
        );
        check(
            "asd// asd //",
            &[(Kind::Identifier, "asd"), (Kind::Comment, " asd //")],
        );
    }

    #[test]
    fn tokenizes_multi_line_comment() {
        check("/* asd */", &[(Kind::MultiComment, " asd ")]);
        check("/* /* asd */", &[(Kind::MultiComment, " /* asd ")]);
        check("/* 1 \n 2 */", &[(Kind::MultiComment, " 1 \n 2 ")]);
        check("/* 1 \r\n 2 */", &[(Kind::MultiComment, " 1 \r\n 2 ")]);
    }

    #[test]
    fn tokenizes_semi_colon() {
        check("asd;", &[(Kind::Identifier, "asd"), (Kind::SemiColon, "")]);
    }

    #[test]
    fn tokenizes_newline() {
        check("asd\n", &[(Kind::Identifier, "asd"), (Kind::Newline, "")]);
    }

    #[test]
    fn tokenizes_comma() {
        check(",", &[(Kind::Comma, "")]);
    }

    #[test]
    fn tokenizes_parenthesis() {
        check("()", &[(Kind::LPar, ""), (Kind::RPar, "")]);
    }

    #[test]
    fn tokenizes_hash() {
        check("#", &[(Kind::Hash, "")]);
    }

    #[test]
    fn tokenizes_equal() {
        check("=", &[(Kind::Equal, "")]);
    }

    #[test]
    fn escaped_newline_ignored() {
        check(
            "a\\\nb",
            &[(Kind::Identifier, "a"), (Kind::Identifier, "b")],
        );
    }

    #[test]
    fn tokenizes_keywords() {
        check("module", &[(Kind::Keyword(Keyword::Module), "module")]);
        check(
            "endmodule",
            &[(Kind::Keyword(Keyword::Endmodule), "endmodule")],
        );
        check("package", &[(Kind::Keyword(Keyword::Package), "package")]);
        check(
            "endpackage",
            &[(Kind::Keyword(Keyword::Other), "endpackage")],
        );
        check(
            "parameter",
            &[(Kind::Keyword(Keyword::Parameter), "parameter")],
        );
        check("import", &[(Kind::Keyword(Keyword::Import), "import")]);
    }

    #[test]
    fn has_location_information() {
        let tokens = tokenize_code("`define foo");
        let ranges: Vec<_> = tokens
            .iter()
            .map(|token| token.location.range.clone())
            .collect();
        assert_eq!(ranges, [0..7, 7..8, 8..11]);
        assert!(tokens.iter().all(|token| {
            token.location.previous.is_none()
                && token.location.source.path.as_deref() == Some(Utf8Path::new("/out/fn.v"))
        }));
    }

    #[test]
    fn crlf_is_one_newline() {
        check(
            "a\r\nb\rc // x\r\n",
            &[
                (Kind::Identifier, "a"),
                (Kind::Newline, ""),
                (Kind::Identifier, "b"),
                (Kind::Newline, ""),
                (Kind::Identifier, "c"),
                (Kind::Whitespace, " "),
                (Kind::Comment, " x"),
                (Kind::Newline, ""),
            ],
        );
        check(
            "a\\\r\nb",
            &[(Kind::Identifier, "a"), (Kind::Identifier, "b")],
        );
    }

    #[test]
    fn verilog_keywords_leave_sv_keywords_identifiers() {
        let tokens = tokenize(
            &source("/out/fn.v", "package module logic"),
            KeywordSet::Verilog1364,
            None,
        );
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Identifier,
                Kind::Whitespace,
                Kind::Keyword(Keyword::Module),
                Kind::Whitespace,
                Kind::Identifier,
            ]
        );
    }
}

mod preprocessor_tests {
    use super::*;

    /// The result of preprocessing `/out/fn.v`.
    struct PreprocessResult {
        /// The preprocessed tokens.
        tokens: Vec<Token>,
        /// The macros at the end.
        defines: Defines,
        /// The paths of the included files that were found.
        included_files: Vec<Utf8PathBuf>,
        /// The problems.
        problems: Vec<Problem>,
    }

    impl PreprocessResult {
        /// Checks that the tokens are those of `code`.
        fn assert_has_tokens(&self, code: &str) -> &Self {
            assert_eq!(
                kinds_and_values(&self.tokens),
                kinds_and_values(&tokenize_code(code))
            );
            self
        }

        /// Checks that no macro is defined.
        fn assert_no_defines(&self) -> &Self {
            assert!(self.defines.is_empty(), "{:?}", self.defines);
            self
        }

        /// Checks that exactly `defines` are defined.
        fn assert_has_defines(&self, defines: &[Macro]) -> &Self {
            let mut names: Vec<_> = self.defines.names().collect();
            names.sort_unstable();
            let mut expected: Vec<_> = defines.iter().map(|macro_| macro_.name.as_str()).collect();
            expected.sort_unstable();
            assert_eq!(names, expected);
            for macro_ in defines {
                assert_eq!(**self.defines.get(&macro_.name).unwrap(), *macro_);
            }
            self
        }

        /// Checks the included files.
        fn assert_included_files(&self, files: &[&str]) -> &Self {
            assert_eq!(self.included_files, files);
            self
        }

        /// Checks that there are no problems.
        fn assert_no_log(&self) -> &Self {
            assert!(self.problems.is_empty(), "{:?}", self.problems);
            self
        }

        /// Checks that there is exactly one problem, with `severity`, `message`, and the chain
        /// `expected`, innermost first.
        fn assert_problem(&self, severity: Severity, message: &str, expected: &[&str]) -> &Self {
            assert_eq!(self.problems.len(), 1, "{:?}", self.problems);
            let problem = &self.problems[0];
            assert_eq!(problem.severity, severity);
            assert_eq!(problem.message, message);
            assert_eq!(chain(&problem.location), expected);
            self
        }

        /// Checks that there is no warning.
        fn assert_no_warning(&self) -> &Self {
            assert!(
                self.problems
                    .iter()
                    .all(|problem| problem.severity != Severity::Warning),
                "{:?}",
                self.problems
            );
            self
        }
    }

    /// Preprocesses `code` as `/out/fn.v`, with the in-memory files `files` and the include
    /// directories `include_dirs`.
    fn preprocess_with(
        code: &str,
        files: &[(&str, &str)],
        include_dirs: &[&str],
    ) -> PreprocessResult {
        let top = source("/out/fn.v", code);
        let include_dirs: Vec<Utf8PathBuf> = include_dirs.iter().map(Utf8PathBuf::from).collect();
        let mut preprocessor =
            Preprocessor::new(KeywordSet::Sv, &include_dirs, Defines::new(), reader(files));
        let tokens = preprocessor.preprocess(tokenize(&top, KeywordSet::Sv, None), &top);
        PreprocessResult {
            tokens,
            defines: preprocessor.defines,
            included_files: preprocessor
                .included
                .into_iter()
                .filter_map(|include| include.path)
                .collect(),
            problems: preprocessor.problems,
        }
    }

    /// Preprocesses `code` as `/out/fn.v`.
    fn preprocess(code: &str) -> PreprocessResult {
        preprocess_with(code, &[], &[OUT])
    }

    /// Creates the macro `name` with the text `text`, the formal arguments `args`, and the
    /// defaults `defaults`.
    fn macro_with(name: &str, text: &str, args: &[&str], defaults: &[(&str, &str)]) -> Macro {
        Macro {
            name: name.to_owned(),
            tokens: tokenize_code(text),
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            defaults: args
                .iter()
                .map(|arg| {
                    defaults
                        .iter()
                        .find(|(default_arg, _)| default_arg == arg)
                        .map(|(_, default)| tokenize_code(default))
                })
                .collect(),
        }
    }

    /// Creates the macro `name` with the text `text` and no formal arguments.
    fn macro_(name: &str, text: &str) -> Macro {
        macro_with(name, text, &[], &[])
    }

    #[test]
    fn non_preprocess_tokens_are_kept() {
        preprocess(r#""hello"ident/*comment*///comment"#)
            .assert_has_tokens(r#""hello"ident/*comment*///comment"#)
            .assert_no_defines();
    }

    #[test]
    fn preprocess_define_without_value() {
        preprocess("`define foo")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_("foo", "")]);
        preprocess("`define foo\nkeep")
            .assert_has_tokens("keep")
            .assert_has_defines(&[macro_("foo", "")]);
    }

    #[test]
    fn preprocess_define_with_value() {
        preprocess(r#"`define foo bar "abc""#)
            .assert_has_tokens("")
            .assert_has_defines(&[macro_("foo", r#"bar "abc""#)]);
    }

    #[test]
    fn preprocess_define_with_lpar_value() {
        preprocess("`define foo (bar)")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_("foo", "(bar)")]);
    }

    #[test]
    fn preprocess_define_with_one_arg() {
        preprocess("`define foo(arg)arg 123")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_with("foo", "arg 123", &["arg"], &[])]);
    }

    #[test]
    fn preprocess_define_with_one_arg_ignores_initial_space() {
        preprocess("`define foo(arg) arg 123")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_with("foo", "arg 123", &["arg"], &[])]);
    }

    #[test]
    fn preprocess_define_with_multiple_args() {
        preprocess("`define foo( arg1, arg2)arg1 arg2")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_with("foo", "arg1 arg2", &["arg1", "arg2"], &[])]);
    }

    #[test]
    fn preprocess_define_with_default_values() {
        preprocess("`define foo(arg1, arg2=default)arg1 arg2")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_with(
                "foo",
                "arg1 arg2",
                &["arg1", "arg2"],
                &[("arg2", "default")],
            )]);
    }

    #[test]
    fn preprocess_substitute_define_without_args() {
        preprocess("`define foo bar \"abc\"\n`foo").assert_has_tokens(r#"bar "abc""#);
    }

    #[test]
    fn preprocess_substitute_define_with_one_arg() {
        preprocess("`define foo(arg)arg 123\n`foo(hello hey)").assert_has_tokens("hello hey 123");
    }

    #[test]
    fn preprocess_define_with_spaced_default_values() {
        preprocess("`define foo(arg1 = `bar, arg2 = f(1, 2) )arg1 arg2")
            .assert_has_tokens("")
            .assert_has_defines(&[macro_with(
                "foo",
                "arg1 arg2",
                &["arg1", "arg2"],
                &[("arg1", "`bar"), ("arg2", "f(1, 2)")],
            )]);
        preprocess("`define bar b\n`define foo(arg1 = `bar, arg2 = f(1, 2) )arg1 arg2\n`foo()")
            .assert_has_tokens("b f(1, 2)")
            .assert_no_log();
    }

    #[test]
    fn preprocess_substitute_define_with_space_before_arg() {
        preprocess("`define foo(arg) arg\n`foo (hello)").assert_has_tokens("hello");
    }

    #[test]
    fn preprocess_substitute_define_no_args() {
        preprocess("`define foo bar\n`foo (hello)").assert_has_tokens("bar (hello)");
    }

    #[test]
    fn preprocess_substitute_define_with_multile_args() {
        preprocess("`define foo(arg1, arg2)arg1,arg2\n`foo(1 2, hello)")
            .assert_has_tokens("1 2, hello");
    }

    #[test]
    fn preprocess_substitute_define_with_default_values() {
        preprocess("`define foo(arg1, arg2=default)arg1 arg2\n`foo(1)")
            .assert_has_tokens("1 default");
    }

    #[test]
    fn preprocess_include_directive() {
        preprocess_with(
            r#"`include "include.svh""#,
            &[("/out/include.svh", "hello hey")],
            &[OUT],
        )
        .assert_has_tokens("hello hey")
        .assert_included_files(&["/out/include.svh"]);
    }

    #[test]
    fn detects_circular_includes() {
        preprocess_with(
            r#"`include "include1.svh""#,
            &[
                ("/out/include1.svh", r#"`include "include2.svh""#),
                ("/out/include2.svh", r#"`include "include1.svh""#),
            ],
            &[OUT],
        )
        .assert_problem(
            Severity::Error,
            "Circular `include of include2.svh detected",
            &[
                r#"include1.svh:1:"include2.svh""#,
                "include2.svh:1:`include",
                "include1.svh:1:`include",
                "fn.v:1:`include",
            ],
        );
    }

    #[test]
    fn detects_circular_include_of_self() {
        preprocess_with(
            r#"`include "include.svh""#,
            &[("/out/include.svh", r#"`include "include.svh""#)],
            &[OUT],
        )
        .assert_problem(
            Severity::Error,
            "Circular `include of include.svh detected",
            &[
                r#"include.svh:1:"include.svh""#,
                "include.svh:1:`include",
                "fn.v:1:`include",
            ],
        );
    }

    #[test]
    fn does_not_detect_non_circular_includes() {
        preprocess_with(
            "`include \"include1.svh\"\n`include \"include2.svh\"",
            &[
                ("/out/include3.svh", "keep"),
                (
                    "/out/include1.svh",
                    "`include \"include3.svh\"\n`include \"include2.svh\"",
                ),
                ("/out/include2.svh", r#"`include "include3.svh""#),
            ],
            &[OUT],
        )
        .assert_no_log();
    }

    #[test]
    fn detects_circular_macro_expansion_of_self() {
        preprocess("\n`define foo `foo\n`foo\n").assert_problem(
            Severity::Error,
            "Circular macro expansion of foo detected",
            &["fn.v:2:`foo", "fn.v:2:`foo", "fn.v:3:`foo"],
        );
    }

    #[test]
    fn detects_circular_macro_expansion() {
        preprocess("\n`define foo `bar\n`define bar `foo\n`foo\n").assert_problem(
            Severity::Error,
            "Circular macro expansion of bar detected",
            &["fn.v:2:`bar", "fn.v:3:`foo", "fn.v:2:`bar", "fn.v:4:`foo"],
        );
    }

    #[test]
    fn does_not_detect_non_circular_macro_expansion() {
        preprocess("\n`define foo bar\n`foo\n`foo\n").assert_no_log();
    }

    #[test]
    fn preprocess_include_directive_from_define() {
        preprocess_with(
            "`define inc \"include.svh\"\n`include `inc",
            &[("/out/include.svh", "hello hey")],
            &[OUT],
        )
        .assert_has_tokens("hello hey")
        .assert_included_files(&["/out/include.svh"]);
    }

    #[test]
    fn preprocess_include_directive_from_define_with_args() {
        preprocess_with(
            "`define inc(a) a\n`include `inc(\"include.svh\")",
            &[("/out/include.svh", "hello hey")],
            &[OUT],
        )
        .assert_has_tokens("hello hey")
        .assert_included_files(&["/out/include.svh"]);
    }

    #[test]
    fn preprocess_macros_are_recursively_expanded() {
        preprocess("`define foo `bar\n`define bar xyz\n`foo\n`define bar abc\n`foo\n")
            .assert_has_tokens("xyz\nabc\n");
    }

    #[test]
    fn ifndef_taken() {
        preprocess("`ifndef foo\ntaken\n`endif\nkeep").assert_has_tokens("taken\nkeep");
    }

    #[test]
    fn ifdef_taken() {
        preprocess("`define foo\n`ifdef foo\ntaken\n`endif\nkeep").assert_has_tokens("taken\nkeep");
    }

    #[test]
    fn ifdef_else_taken() {
        preprocess("`define foo\n`ifdef foo\ntaken\n`else\nelse\n`endif\nkeep")
            .assert_has_tokens("taken\nkeep");
    }

    #[test]
    fn ifdef_not_taken() {
        preprocess("`ifdef foo\ntaken\n`endif\nkeep").assert_has_tokens("keep");
    }

    #[test]
    fn ifdef_else_not_taken() {
        preprocess("`ifdef foo\ntaken\n`else\nelse\n`endif\nkeep").assert_has_tokens("else\nkeep");
    }

    #[test]
    fn ifdef_elsif_taken() {
        preprocess(
            "`define foo\n`ifdef foo\ntaken\n`elsif bar\nelsif_taken\n`else\nelse_taken\n`endif\n\
             keep",
        )
        .assert_has_tokens("taken\nkeep");
    }

    #[test]
    fn ifdef_elsif_elseif_taken() {
        preprocess(
            "`define bar\n`ifdef foo\ntaken\n`elsif bar\nelsif_taken\n`else\nelse_taken\n`endif\n\
             keep",
        )
        .assert_has_tokens("elsif_taken\nkeep");
    }

    #[test]
    fn ifdef_elsif_else_taken() {
        preprocess("`ifdef foo\ntaken\n`elsif bar\nelsif_taken\n`else\nelse_taken\n`endif\nkeep")
            .assert_has_tokens("else_taken\nkeep");
    }

    #[test]
    fn nested_ifdef() {
        preprocess(
            "`define foo\n`ifdef foo\nouter_before\n`ifdef bar\ninner_ifndef\n`else\ninner_else\n\
             `endif\n`ifdef bar\ninner_ifndef\n`elsif foo\ninner_elsif\n`endif\nouter_after\n\
             `endif\nkeep",
        )
        .assert_has_tokens("outer_before\ninner_else\ninner_elsif\nouter_after\nkeep");
    }

    #[test]
    fn preprocess_broken_define() {
        preprocess("`define")
            .assert_has_tokens("")
            .assert_no_defines()
            .assert_problem(
                Severity::Warning,
                "Verilog `define without argument",
                &["fn.v:1:`define"],
            );
    }

    #[test]
    fn preprocess_broken_define_first_argument() {
        preprocess(r#"`define "foo""#)
            .assert_has_tokens("")
            .assert_no_defines()
            .assert_problem(
                Severity::Warning,
                "Verilog `define invalid name",
                &[r#"fn.v:1:"foo""#],
            );
    }

    #[test]
    fn preprocess_broken_define_argument_list() {
        for code in [
            "`define foo(",
            "`define foo(a",
            "`define foo(a=",
            "`define foo(a=b",
            "`define foo(a=)",
            r#"`define foo("a""#,
            r#"`define foo("a"="#,
        ] {
            preprocess(code)
                .assert_has_tokens("")
                .assert_no_defines()
                .assert_problem(
                    Severity::Warning,
                    "EOF reached when parsing `define argument list",
                    &["fn.v:1:("],
                );
        }
    }

    #[test]
    fn preprocess_substitute_define_broken_args() {
        for call in ["`foo(1 2)", "`foo", "`foo(", "`foo(1"] {
            preprocess(&format!("`define foo(arg1, arg2)arg1,arg2\n{call}")).assert_has_tokens("");
        }
    }

    #[test]
    fn preprocess_substitute_define_missing_argument() {
        preprocess("`define foo(arg1, arg2)arg1,arg2\n`foo(1)")
            .assert_has_tokens("")
            .assert_problem(
                Severity::Warning,
                "Missing value for argument arg2",
                &["fn.v:2:`foo"],
            );
    }

    #[test]
    fn preprocess_substitute_define_too_many_argument() {
        preprocess("`define foo(arg1)arg1\n`foo(1, 2)")
            .assert_has_tokens("")
            .assert_problem(
                Severity::Warning,
                "Too many arguments got 2 expected 1",
                &["fn.v:2:`foo"],
            );
    }

    #[test]
    fn preprocess_substitute_define_with_nested_argument() {
        preprocess("`define foo(arg1, arg2)arg1\n`foo([1, 2], 3)")
            .assert_no_warning()
            .assert_has_tokens("[1, 2]");
        preprocess("`define foo(arg1, arg2)arg1\n`foo({1, 2}, 3)")
            .assert_no_warning()
            .assert_has_tokens("{1, 2}");
        preprocess("`define foo(arg1, arg2)arg1\n`foo((1, 2), 3)")
            .assert_no_warning()
            .assert_has_tokens("(1, 2)");
        preprocess("`define foo(arg1)arg1\n`foo((1, 2))")
            .assert_no_warning()
            .assert_has_tokens("(1, 2)");
        // Not OK in a simulator, but the simulator tells the user.
        preprocess("`define foo(arg1)arg1\n`foo([1, 2)")
            .assert_no_warning()
            .assert_has_tokens("[1, 2");
    }

    #[test]
    fn preprocess_substitute_define_eof() {
        for call in ["`foo(1 2", "`foo((1 2)"] {
            preprocess(&format!("`define foo(arg1, arg2)arg1,arg2\n{call}"))
                .assert_has_tokens("")
                .assert_problem(
                    Severity::Warning,
                    "EOF reached when parsing `define actuals",
                    &["fn.v:2:`foo"],
                );
        }
    }

    #[test]
    fn substitute_undefined() {
        // Debug, since tools have many macros of their own.
        preprocess("`foo").assert_has_tokens("").assert_problem(
            Severity::Debug,
            "Verilog undefined name",
            &["fn.v:1:`foo"],
        );
    }

    #[test]
    fn preprocess_include_directive_missing_file() {
        // Debug, since tools have many builtin include files.
        preprocess(r#"`include "missing.svh""#)
            .assert_has_tokens("")
            .assert_included_files(&[])
            .assert_problem(
                Severity::Debug,
                "Could not find `include file missing.svh",
                &[r#"fn.v:1:"missing.svh""#],
            );
    }

    #[test]
    fn preprocess_include_directive_missing_argument() {
        preprocess("`include")
            .assert_has_tokens("")
            .assert_included_files(&[])
            .assert_problem(
                Severity::Warning,
                "EOF reached when parsing `include argument",
                &["fn.v:1:`include"],
            );
    }

    #[test]
    fn preprocess_include_directive_bad_argument() {
        preprocess_with(
            r#"`include foo "include.svh""#,
            &[("/out/include.svh", "hello hey")],
            &[OUT],
        )
        .assert_has_tokens(r#" "include.svh""#)
        .assert_included_files(&[])
        .assert_problem(
            Severity::Warning,
            "Verilog `include bad argument",
            &["fn.v:1:foo"],
        );
    }

    #[test]
    fn preprocess_include_directive_from_define_bad_argument() {
        preprocess("`define inc foo\n`include `inc\nkeep")
            .assert_has_tokens("\nkeep")
            .assert_included_files(&[])
            .assert_problem(
                Severity::Warning,
                "Verilog `include has bad argument",
                &["fn.v:1:foo", "fn.v:2:`inc"],
            );
    }

    #[test]
    fn preprocess_include_directive_from_empty_define() {
        preprocess("`define inc\n`include `inc\nkeep")
            .assert_has_tokens("\nkeep")
            .assert_included_files(&[])
            .assert_problem(
                Severity::Warning,
                "Verilog `include has bad argument, empty define `inc",
                &["fn.v:2:`inc"],
            );
    }

    #[test]
    fn preprocess_include_directive_from_define_not_defined() {
        preprocess("`include `inc")
            .assert_has_tokens("")
            .assert_included_files(&[])
            .assert_problem(
                Severity::Warning,
                "Verilog `include argument not defined",
                &["fn.v:1:`inc"],
            );
    }

    #[test]
    fn preprocess_error_in_include_file() {
        preprocess_with(
            "\n\n`include \"include.svh\"",
            &[("/out/include.svh", "`include foo")],
            &[OUT],
        )
        .assert_has_tokens("\n\n")
        .assert_included_files(&["/out/include.svh"])
        .assert_problem(
            Severity::Warning,
            "Verilog `include bad argument",
            &["include.svh:1:foo", "fn.v:3:`include"],
        );
    }

    #[test]
    fn preprocess_error_in_expanded_define() {
        preprocess("`define foo `include wrong\n`foo\n")
            .assert_has_tokens("\n")
            .assert_included_files(&[])
            .assert_problem(
                Severity::Warning,
                "Verilog `include bad argument",
                &["fn.v:1:wrong", "fn.v:2:`foo"],
            );
    }

    #[test]
    fn ifdef_eof() {
        preprocess("`ifdef foo\ntaken")
            .assert_has_tokens("")
            .assert_problem(
                Severity::Warning,
                "EOF reached when parsing `ifdef",
                &["fn.v:1:`ifdef"],
            );
    }

    #[test]
    fn ifdef_bad_argument() {
        preprocess("`ifdef \"hello\"\nkeep")
            .assert_has_tokens("\nkeep")
            .assert_problem(
                Severity::Warning,
                "Bad argument to `ifdef",
                &[r#"fn.v:1:"hello""#],
            );
    }

    #[test]
    fn elsif_bad_argument() {
        preprocess("`ifdef bar\n`elsif \"hello\"\nkeep")
            .assert_has_tokens("\nkeep")
            .assert_problem(
                Severity::Warning,
                "Bad argument to `elsif",
                &[r#"fn.v:2:"hello""#],
            );
    }

    #[test]
    fn undefineall() {
        preprocess("`define foo keep\n`define bar keep2\n`foo\n`undefineall")
            .assert_has_tokens("keep\n")
            .assert_no_defines();
    }

    /// Upstream undefines the macros at `` `resetall ``; IEEE 1800 22.3 doesn't.
    #[test]
    fn resetall() {
        preprocess("`define foo keep\n`define bar keep2\n`foo\n`resetall")
            .assert_has_tokens("keep\n")
            .assert_has_defines(&[macro_("foo", "keep"), macro_("bar", "keep2")])
            .assert_no_log();
    }

    #[test]
    fn undef() {
        preprocess("`define foo keep\n`define bar keep2\n`foo\n`undef foo")
            .assert_has_tokens("keep\n")
            .assert_has_defines(&[macro_("bar", "keep2")]);
    }

    #[test]
    fn undef_eof() {
        preprocess("`undef")
            .assert_has_tokens("")
            .assert_no_defines()
            .assert_problem(
                Severity::Warning,
                "EOF reached when parsing `undef",
                &["fn.v:1:`undef"],
            );
    }

    #[test]
    fn undef_bad_argument() {
        preprocess(r#"`undef "foo""#)
            .assert_has_tokens("")
            .assert_no_defines()
            .assert_problem(
                Severity::Warning,
                "Bad argument to `undef",
                &[r#"fn.v:1:"foo""#],
            );
    }

    #[test]
    fn undef_not_defined() {
        preprocess("`undef foo")
            .assert_has_tokens("")
            .assert_no_defines()
            .assert_problem(
                Severity::Warning,
                "`undef argument was not previously defined",
                &["fn.v:1:foo"],
            );
    }

    #[test]
    fn ignores_celldefine() {
        preprocess("`celldefine`endcelldefine keep")
            .assert_has_tokens(" keep")
            .assert_no_log();
    }

    #[test]
    fn ignores_timescale() {
        preprocess("`timescale 1 ns / 1 ps\nkeep")
            .assert_has_tokens("\nkeep")
            .assert_no_log();
    }

    #[test]
    fn ignores_default_nettype() {
        preprocess("`default_nettype none\nkeep")
            .assert_has_tokens("\nkeep")
            .assert_no_log();
    }

    #[test]
    fn ignores_nounconnected_drive() {
        preprocess("`nounconnected_drive keep")
            .assert_has_tokens(" keep")
            .assert_no_log();
    }

    #[test]
    fn ignores_protected_region() {
        preprocess(
            "keep_before\n`pragma protect begin_protected\nASDADSJAKSJDKSAJDISA\n`pragma protect \
             end_protected\nkeep_end",
        )
        .assert_has_tokens("keep_before\n\nkeep_end")
        .assert_no_log();
    }

    #[test]
    fn include_searches_the_including_files_directory_first() {
        // `/out/sub/a.svh`, found in the include directory `/out/sub`, includes `b.svh`, which
        // is in `/out/sub`, in the earlier include directory `/out/inc`, and in `/out`.
        let files = [
            ("/out/sub/a.svh", r#"`include "b.svh""#),
            ("/out/sub/b.svh", "sub_b"),
            ("/out/inc/b.svh", "inc_b"),
            ("/out/b.svh", "top_b"),
        ];
        preprocess_with(r#"`include "a.svh""#, &files, &["/out/inc", "/out/sub"])
            .assert_has_tokens("sub_b")
            .assert_included_files(&["/out/sub/a.svh", "/out/sub/b.svh"])
            .assert_no_log();
        // The top file's directory comes before the include directories.
        preprocess_with(r#"`include "b.svh""#, &files, &["/out/inc"]).assert_has_tokens("top_b");
        // The directory of the top file isn't searched for an include of an included file.
        let nested_files = [
            ("/out/sub/a.svh", r#"`include "c.svh""#),
            ("/out/c.svh", "top_c"),
        ];
        preprocess_with(r#"`include "sub/a.svh""#, &nested_files, &[])
            .assert_has_tokens("")
            .assert_included_files(&["/out/sub/a.svh"]);
    }

    #[test]
    fn include_paths_are_normalized() {
        preprocess_with(r#"`include "../lib/./x.svh""#, &[("/lib/x.svh", "x")], &[])
            .assert_has_tokens("x")
            .assert_included_files(&["/lib/x.svh"]);
    }

    #[test]
    fn too_deep_nesting_stops() {
        let code = format!(
            "{}keep\n{}",
            "`ifndef X\n".repeat(300),
            "`endif\n".repeat(300)
        );
        let result = preprocess(&code);
        assert_eq!(result.problems.len(), 1, "{:?}", result.problems);
        assert_eq!(
            result.problems[0].message,
            "Macro expansions, includes, and conditional blocks nest too deeply"
        );
    }
}

mod parser_tests {
    use super::*;

    /// Scans `code` as `/out/file_name.sv`, with the macros `defines` and the in-memory files
    /// `files`.
    fn scan_with(
        code: &str,
        keywords: KeywordSet,
        defines: &Defines,
        files: &[(&str, &str)],
    ) -> (SvDesignFile, Defines, Vec<Problem>) {
        let input = ScanInput {
            path: Utf8Path::new("/out/file_name.sv"),
            contents: code.as_bytes(),
            include_dirs: &[],
            defines,
            keywords,
        };
        scan_with_problems(&input, reader(files))
    }

    /// Scans `code` as `/out/file_name.sv`.
    fn parse(code: &str) -> SvDesignFile {
        scan_with(code, KeywordSet::Sv, &Defines::new(), &[]).0
    }

    /// Returns the names and kinds of the definitions of `design_file`.
    fn definitions(design_file: &SvDesignFile) -> Vec<(&str, SvDefinitionKind)> {
        design_file
            .definitions
            .iter()
            .map(|definition| (definition.name.as_str(), definition.kind))
            .collect()
    }

    /// Returns the names of the definitions of `design_file`.
    fn definition_names(design_file: &SvDesignFile) -> Vec<&str> {
        design_file
            .definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect()
    }

    /// Returns the names of the packages of `design_file`.
    fn package_names(design_file: &SvDesignFile) -> Vec<&str> {
        design_file
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect()
    }

    #[test]
    fn parsing_empty() {
        assert_eq!(parse(""), SvDesignFile::default());
    }

    #[test]
    fn parse_module() {
        let design_file = parse(
            "module true1;\n  my_module hello\n  \"module false\";\nendmodule\n/*\nmodule \
             false\n*/\n  module true2; // module false\nendmodule module true3\nendmodule\n",
        );
        assert_eq!(
            definitions(&design_file),
            [
                ("true1", SvDefinitionKind::Module),
                ("true2", SvDefinitionKind::Module),
                ("true3", SvDefinitionKind::Module),
            ]
        );
    }

    /// Keywords are allowed as names, since standards may be mixed.
    #[test]
    fn parse_module_with_keyword_name() {
        let design_file = parse("module global;\nendmodule\n\nmodule soft;\nendmodule\n");
        assert_eq!(definition_names(&design_file), ["global", "soft"]);
    }

    #[test]
    fn parse_parameter_without_type() {
        let design_file =
            parse("module foo;\n  parameter param1;\n  parameter param2 = 1;\nendmodule\n");
        assert_eq!(definition_names(&design_file), ["foo"]);
        assert_eq!(design_file.definitions[0].parameters, ["param1", "param2"]);
    }

    #[test]
    fn parse_parameter_with_type() {
        let design_file = parse(
            "module foo;\n  parameter string param1;\n  parameter integer param2 = 1;\nendmodule\n",
        );
        assert_eq!(definition_names(&design_file), ["foo"]);
        assert_eq!(design_file.definitions[0].parameters, ["param1", "param2"]);
    }

    #[test]
    fn nested_modules_are_ignored() {
        let design_file = parse(
            "module foo;\n  parameter string param1;\n  module nested;\n    parameter integer \
             param_nested;\n  endmodule\n  parameter string param2;\nendmodule\n",
        );
        assert_eq!(definition_names(&design_file), ["foo"]);
        assert_eq!(design_file.definitions[0].parameters, ["param1", "param2"]);
    }

    #[test]
    fn parse_package() {
        let design_file = parse("package true1;\nendpackage package true2; endpackage\n");
        assert_eq!(package_names(&design_file), ["true1", "true2"]);
    }

    #[test]
    fn parse_imports() {
        let design_file = parse("import true1;\npackage pkg;\n  import true2::*;\nendpackage\n");
        assert_eq!(design_file.imports, ["true1", "true2"]);
    }

    #[test]
    fn parse_package_references() {
        let design_file = parse(
            "import false1;\nimport false1::false2::*;\npackage pkg;\n  \
             true1::func(true2::bar());\n  true3::foo();\nendpackage\n",
        );
        assert_eq!(design_file.package_references, ["true1", "true2", "true3"]);
    }

    #[test]
    fn parse_import_with_bad_argument() {
        let (design_file, _, problems) =
            scan_with("import;\n", KeywordSet::Sv, &Defines::new(), &[]);
        assert!(design_file.imports.is_empty());
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].severity, Severity::Warning);
        assert_eq!(problems[0].message, "import bad argument");
        assert_eq!(chain(&problems[0].location), ["file_name.sv:1:;"]);
    }

    #[test]
    fn parse_import_eof() {
        let (design_file, _, problems) =
            scan_with("import\n", KeywordSet::Sv, &Defines::new(), &[]);
        assert!(design_file.imports.is_empty());
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].severity, Severity::Warning);
        assert_eq!(problems[0].message, "EOF reached when parsing import");
        assert_eq!(chain(&problems[0].location), ["file_name.sv:1:import"]);
    }

    #[test]
    fn parse_instances() {
        let design_file = parse(
            "module name;\n  true1 instance_name1();\n  true2 instance_name2(.foo(bar));\n  true3 \
             #(.param(1)) instance_name3(.foo(bar));\nendmodule\n",
        );
        assert_eq!(design_file.instances, ["true1", "true2", "true3"]);
    }

    #[test]
    fn parse_instances_after_block_label() {
        let design_file = parse(
            "module name;\ngenvar i;\n  generate\n    for( i=0; i < 10; i = i + 1 )\n      \
             begin: INST_GEN\n        true1 instance_name1();\n    end : INST_GEN\n    true2 \
             instance_name2();\n  endgenerate\nendmodule\n",
        );
        assert_eq!(design_file.instances, ["true1", "true2"]);
    }

    #[test]
    fn parse_instances_without_crashing() {
        let design_file = parse("module name;\nendmodule identifier\n");
        assert!(design_file.instances.is_empty());
    }

    #[test]
    fn can_set_pre_defined_defines() {
        let mut defines = Defines::new();
        defines.define("foo", None, "module mod1;");
        let (design_file, ..) = scan_with(
            "`ifdef foo\n`foo\nendmodule;\n`endif\n",
            KeywordSet::Sv,
            &defines,
            &[],
        );
        assert_eq!(definition_names(&design_file), ["mod1"]);
    }

    #[test]
    fn finds_interfaces_programs_primitives_and_macromodules() {
        let design_file = parse(
            "interface bus_if #(parameter W = 8) (input clk);\n  modport master(output \
             data);\nendinterface : bus_if\nprogram automatic test;\nendprogram\nprimitive \
             udp(out, in);\nendprimitive\nmacromodule mac;\nendmodule\n",
        );
        assert_eq!(
            definitions(&design_file),
            [
                ("bus_if", SvDefinitionKind::Interface),
                ("test", SvDefinitionKind::Program),
                ("udp", SvDefinitionKind::Primitive),
                ("mac", SvDefinitionKind::Macromodule),
            ]
        );
        assert_eq!(design_file.definitions[0].parameters, ["W"]);
    }

    #[test]
    fn interface_uses_that_are_no_definitions() {
        let design_file = parse(
            "module m(interface generic, interface.mp generic_mp);\n  virtual interface bus_if \
             vif;\nendmodule\npackage p;\n  interface class ic;\n  endclass\nendpackage\nextern \
             module e(input a);\nmodule after;\nendmodule\n",
        );
        assert_eq!(definition_names(&design_file), ["m", "after"]);
        assert_eq!(package_names(&design_file), ["p"]);
    }

    #[test]
    fn interface_ports_are_instances() {
        let design_file = parse(
            "module m(bus_if.master port_a, bus_if port_b, input logic clk);\n  sub \
             u_sub(.a(port_a));\nendmodule\n",
        );
        assert_eq!(design_file.instances, ["bus_if", "bus_if", "sub"]);
    }

    #[test]
    fn imports_of_a_list() {
        let design_file =
            parse("import a::*, b::x;\nimport \"DPI-C\" function void f(int y, t z);\n");
        assert_eq!(design_file.imports, ["a", "b"]);
    }

    #[test]
    fn package_reference_after_an_identifier() {
        let design_file = parse("module m;\n  x p::t;\nendmodule\n");
        assert_eq!(design_file.package_references, ["p"]);
    }

    #[test]
    fn verilog_keywords_make_package_an_identifier() {
        let code = "module m;\n  wire package;\n  import x;\nendmodule\n";
        let (verilog, ..) = scan_with(code, KeywordSet::Verilog1364, &Defines::new(), &[]);
        assert_eq!(definition_names(&verilog), ["m"]);
        assert!(verilog.packages.is_empty());
        assert!(verilog.imports.is_empty());
        assert_eq!(verilog.instances, ["import"]);
        let (sv, ..) = scan_with(code, KeywordSet::Sv, &Defines::new(), &[]);
        assert_eq!(sv.imports, ["x"]);
        assert!(sv.instances.is_empty());
    }

    #[test]
    fn macros_are_carried_out_of_scan() {
        let mut defines = Defines::new();
        defines.define("FROM_OPTIONS", Some("(a, b=1)"), "a + b");
        let (_, after_first, _) = scan_with(
            "`define FIRST module first;\n`undef FROM_OPTIONS\n",
            KeywordSet::Sv,
            &defines,
            &[],
        );
        assert_eq!(after_first.names().collect::<Vec<_>>(), ["FIRST"]);
        let (second, ..) = scan_with("`FIRST\nendmodule\n", KeywordSet::Sv, &after_first, &[]);
        assert_eq!(definition_names(&second), ["first"]);
        assert_eq!(second.definitions[0].name_range, None);
    }

    #[test]
    fn macro_formals_from_options() {
        let mut defines = Defines::new();
        defines.define("M", Some("(name, kind=module)"), "kind name; endmodule");
        let (design_file, ..) = scan_with("`M(a)\n`M(b, program)\n", KeywordSet::Sv, &defines, &[]);
        assert_eq!(
            definitions(&design_file),
            [
                ("a", SvDefinitionKind::Module),
                ("b", SvDefinitionKind::Program)
            ]
        );
    }

    #[test]
    fn name_range_only_for_names_spelled_in_the_file() {
        let code = "`include \"inc.svh\"\n`define NAME from_macro\nmodule `NAME;\nendmodule\n\
                    `ifndef X\nmodule spelled;\nendmodule\n`endif\npackage pkg;\nendpackage\n";
        let (design_file, _, problems) = scan_with(
            code,
            KeywordSet::Sv,
            &Defines::new(),
            &[(
                "/out/inc.svh",
                "module included;\nendmodule\npackage inc_pkg;\nendpackage\n",
            )],
        );
        assert!(problems.is_empty(), "{problems:?}");
        let names: Vec<_> = design_file
            .definitions
            .iter()
            .map(|definition| (definition.name.as_str(), definition.name_range.clone()))
            .collect();
        let spelled = code.find("spelled").unwrap();
        assert_eq!(
            names,
            [
                ("included", None),
                ("from_macro", None),
                ("spelled", Some(spelled..spelled + "spelled".len())),
            ]
        );
        let pkg = code.find("pkg;").unwrap();
        let packages: Vec<_> = design_file
            .packages
            .iter()
            .map(|package| (package.name.as_str(), package.name_range.clone()))
            .collect();
        assert_eq!(packages, [("inc_pkg", None), ("pkg", Some(pkg..pkg + 3))]);
        assert_eq!(
            design_file.includes,
            [SvInclude {
                spelling: "inc.svh".to_owned(),
                path: Some(Utf8PathBuf::from("/out/inc.svh")),
            }]
        );
    }

    #[test]
    fn includes_are_flattened_with_missing_ones() {
        let (design_file, ..) = scan_with(
            "`include \"a.svh\"\n`include \"missing.svh\"\n",
            KeywordSet::Sv,
            &Defines::new(),
            &[("/out/a.svh", "`include \"b.svh\"\n"), ("/out/b.svh", "")],
        );
        let includes: Vec<_> = design_file
            .includes
            .iter()
            .map(|include| {
                (
                    include.spelling.as_str(),
                    include.path.as_deref().map(Utf8Path::as_str),
                )
            })
            .collect();
        assert_eq!(
            includes,
            [
                ("a.svh", Some("/out/a.svh")),
                ("b.svh", Some("/out/b.svh")),
                ("missing.svh", None),
            ]
        );
    }

    #[test]
    fn definition_with_lifetime() {
        let design_file = parse("module automatic m;\nendmodule\npackage static p;\nendpackage\n");
        assert_eq!(definition_names(&design_file), ["m"]);
        assert_eq!(package_names(&design_file), ["p"]);
    }
}
