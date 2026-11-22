// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The finders of design elements and references in preprocessed tokens.
//!
//! A port of `VerilogDesignFile`, `VerilogModule`, and `VerilogPackage` of
//! `vunit/parsing/verilog/parser.py`.
//! Like upstream, the finders don't parse SystemVerilog; they look for short token patterns after
//! white space, comments, and newlines are removed.
//!
//! Deviations from upstream:
//!
//! - Interfaces, programs, primitives, and `macromodule` are definitions too, and their end
//!   keywords close any of them.
//!   `interface class`, `virtual interface`, `extern module`, and the generic interface ports
//!   `interface name` and `interface.modport name` don't start a definition.
//!   The lifetime in `module automatic name` isn't the name.
//! - The finders reclassify identifiers and keywords with the keyword set of the file, so that a
//!   macro defined by an option reads the same in a Verilog and in a SystemVerilog file.
//! - `import a::x, b::y;` imports `b` too.
//! - A package reference `p::x` right after an identifier is found; upstream skips it.
//! - `name.modport port`, an interface port with a modport, is an instance of `name`; upstream
//!   takes the modport for the instance.
//! - The name of a parameter is the last identifier of its declaration before its default, not
//!   the first or second token after `parameter`, which may be the type.
//! - At the end of the tokens, the finders stop instead of failing.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::sync::Arc;

use super::SvDefinition;
use super::SvDefinitionKind;
use super::SvName;
use super::preprocessor::Problem;
use super::tokenizer::Keyword;
use super::tokenizer::KeywordSet;
use super::tokenizer::Kind;
use super::tokenizer::Source;
use super::tokenizer::Token;

/// Returns the tokens that the finders look at: without white space, comments, and newlines,
/// and with identifiers and keywords classified by `keywords`.
pub(crate) fn significant_tokens(tokens: Vec<Token>, keywords: KeywordSet) -> Vec<Token> {
    tokens
        .into_iter()
        .filter(|token| {
            !matches!(
                token.kind,
                Kind::Whitespace | Kind::Comment | Kind::Newline | Kind::MultiComment
            )
        })
        .map(|mut token| {
            if token.kind.is_name() {
                token.kind = keywords.classify(token.text());
            }
            token
        })
        .collect()
}

/// Returns the name of `token` and its range if it is spelled in `top` itself.
fn name_of(token: &Token, top: &Arc<Source>) -> SvName {
    SvName {
        name: token.value().into_owned(),
        name_range: token
            .location
            .is_spelled_in(top)
            .then(|| token.location.range.clone()),
        name_location: None,
    }
}

/// Returns the kind of definition that the keyword at `index` starts, if it starts one.
fn definition_start(tokens: &[Token], index: usize) -> Option<SvDefinitionKind> {
    let Kind::Keyword(keyword) = tokens[index].kind else {
        return None;
    };
    let kind = match keyword {
        Keyword::Module => SvDefinitionKind::Module,
        Keyword::Macromodule => SvDefinitionKind::Macromodule,
        Keyword::Interface => SvDefinitionKind::Interface,
        Keyword::Program => SvDefinitionKind::Program,
        Keyword::Primitive => SvDefinitionKind::Primitive,
        _ => return None,
    };
    let previous = index.checked_sub(1).map(|previous| tokens[previous].kind);
    if matches!(
        previous,
        Some(Kind::Keyword(Keyword::Virtual | Keyword::Extern) | Kind::LPar | Kind::Comma)
    ) {
        return None;
    }
    match tokens.get(index + 1) {
        Some(next) if next.kind == Kind::Keyword(Keyword::Class) => None,
        Some(next) if next.kind == Kind::Other && next.text() == b"." => None,
        _ => Some(kind),
    }
}

/// Returns `true` if the token ends a definition.
const fn is_definition_end(token: &Token) -> bool {
    matches!(
        token.kind,
        Kind::Keyword(
            Keyword::Endmodule
                | Keyword::Endinterface
                | Keyword::Endprogram
                | Keyword::Endprimitive
        )
    )
}

/// Returns the index of the name of the design element whose keyword is at `index`, skipping a
/// lifetime.
fn name_index(tokens: &[Token], index: usize) -> Option<usize> {
    let next = tokens.get(index + 1)?;
    let is_lifetime = matches!(
        next.kind,
        Kind::Keyword(Keyword::Automatic | Keyword::Static)
    ) && tokens
        .get(index + 2)
        .is_some_and(|token| token.kind.is_name());
    let name = if is_lifetime { index + 2 } else { index + 1 };
    tokens[name].kind.is_name().then_some(name)
}

/// Finds the definitions; nested ones are ignored (upstream's `VerilogModule.find`).
pub(crate) fn find_definitions(tokens: &[Token], top: &Arc<Source>) -> Vec<SvDefinition> {
    let mut results = Vec::new();
    let mut current: Option<SvDefinition> = None;
    let mut balance = 0_usize;
    for index in 0..tokens.len() {
        if let Some(kind) = definition_start(tokens, index) {
            if balance == 0 {
                current = name_index(tokens, index).map(|name| {
                    let SvName {
                        name,
                        name_range,
                        name_location,
                    } = name_of(&tokens[name], top);
                    SvDefinition {
                        name,
                        kind,
                        parameters: Vec::new(),
                        name_range,
                        name_location,
                    }
                });
            }
            balance += 1;
        } else if is_definition_end(&tokens[index]) {
            if balance > 0 {
                balance -= 1;
                if balance == 0 {
                    results.extend(current.take());
                }
            }
        } else if balance == 1
            && let Some(parameter) = parse_parameter(tokens, index)
            && let Some(ref mut definition) = current
        {
            definition.parameters.push(parameter);
        }
    }
    results
}

/// How many tokens after `parameter` [`parse_parameter`] looks at.
const MAX_PARAMETER_TOKENS: usize = 64;

/// Returns the name of the parameter declared at `index`
/// (upstream's `VerilogModule.parse_parameter`).
///
/// The name is the last identifier before the first `=`, `,`, `;`, or `)` outside parentheses
/// and brackets, within [`MAX_PARAMETER_TOKENS`] tokens.
/// Upstream takes the second token after `parameter` if that is an identifier, else the first,
/// which gives the type in `parameter int unsigned N`, `parameter logic [7:0] N`, and
/// `parameter p::t N`.
fn parse_parameter(tokens: &[Token], index: usize) -> Option<String> {
    if tokens[index].kind != Kind::Keyword(Keyword::Parameter) {
        return None;
    }
    let mut depth = 0_i32;
    let mut name = None;
    for token in tokens.iter().skip(index + 1).take(MAX_PARAMETER_TOKENS) {
        match token.kind {
            Kind::LPar | Kind::LBracket => depth += 1,
            Kind::RPar | Kind::RBracket if depth > 0 => depth -= 1,
            Kind::Equal | Kind::Comma | Kind::SemiColon | Kind::RPar if depth == 0 => break,
            Kind::Identifier if depth == 0 => name = Some(token),
            _ => {},
        }
    }
    name.map(|name| name.value().into_owned())
}

/// Finds the packages (upstream's `VerilogPackage.find`).
pub(crate) fn find_packages(tokens: &[Token], top: &Arc<Source>) -> Vec<SvName> {
    (0..tokens.len())
        .filter(|&index| tokens[index].kind == Kind::Keyword(Keyword::Package))
        .filter_map(|index| name_index(tokens, index))
        .map(|name| name_of(&tokens[name], top))
        .collect()
}

/// Finds the imported packages, in order (upstream's `VerilogDesignFile.find_imports`).
pub(crate) fn find_imports(tokens: &[Token], problems: &mut Vec<Problem>) -> Vec<String> {
    let mut results = Vec::new();
    let mut index = 0;
    while let Some(import_token) = tokens.get(index) {
        index += 1;
        if import_token.kind != Kind::Keyword(Keyword::Import) {
            continue;
        }
        let Some(package) = tokens.get(index) else {
            problems.push(Problem::warning(
                "EOF reached when parsing import",
                &import_token.location,
            ));
            break;
        };
        index += 1;
        if package.kind != Kind::Identifier {
            problems.push(Problem::warning("import bad argument", &package.location));
            continue;
        }
        results.push(package.value().into_owned());
        // The further packages of `import a::x, b::y;`.
        while let Some(item) = tokens.get(index)
            && !matches!(item.kind, Kind::SemiColon | Kind::Keyword(Keyword::Import))
        {
            index += 1;
            if item.kind == Kind::Comma
                && let [next_package, colons, ..] = &tokens[index..]
                && next_package.kind == Kind::Identifier
                && colons.kind == Kind::DoubleColon
            {
                results.push(next_package.value().into_owned());
            }
        }
    }
    results
}

/// Finds the package references `p::x`, in order
/// (upstream's `VerilogDesignFile.find_package_references`).
pub(crate) fn find_package_references(tokens: &[Token]) -> Vec<String> {
    let mut results = Vec::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        index += 1;
        if token.kind == Kind::Keyword(Keyword::Import) {
            // Past the `;`.
            index += tokens[index..]
                .iter()
                .position(|item| item.kind == Kind::SemiColon)
                .map_or(tokens.len() - index, |offset| offset + 1);
        } else if token.kind == Kind::Identifier
            && tokens
                .get(index)
                .is_some_and(|next| next.kind == Kind::DoubleColon)
        {
            results.push(token.value().into_owned());
            index += tokens[index..]
                .iter()
                .take_while(|item| matches!(item.kind, Kind::Identifier | Kind::DoubleColon))
                .count();
        }
    }
    results
}

/// Finds the instances and interface ports, `name name`, `name #`, and `name.modport name`, in
/// order (upstream's `VerilogDesignFile.find_instances`).
pub(crate) fn find_instances(tokens: &[Token]) -> Vec<String> {
    let mut results = Vec::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        index += 1;
        if matches!(token.kind, Kind::Keyword(Keyword::Begin | Keyword::End)) {
            // A block label: `begin : name`.
            if tokens
                .get(index)
                .is_some_and(|colon| colon.kind == Kind::Colon)
            {
                index = (index + 2).min(tokens.len());
            }
            continue;
        }
        if token.kind != Kind::Identifier {
            continue;
        }
        let Some(next) = tokens.get(index) else {
            continue;
        };
        index += 1;
        if matches!(next.kind, Kind::Hash | Kind::Identifier) {
            results.push(token.value().into_owned());
        } else if next.kind == Kind::Other
            && next.text() == b"."
            && let [modport, port, ..] = &tokens[index..]
            && modport.kind == Kind::Identifier
            && port.kind == Kind::Identifier
        {
            results.push(token.value().into_owned());
            index += 2;
        }
    }
    results
}
