// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Regex-based extraction of design units and references from VHDL source files.
//!
//! A port of `vhdl_parser.py`. Like VUnit, the parser doesn't understand VHDL; it finds
//! design units, generics, ports, references and component instantiations with regular
//! expressions on the source text after comments are blanked out and the text is lowercased.
//!
//! Source files are Latin-1 (VUnit's `HDL_FILE_ENCODING`), so the regular expressions run on
//! bytes with Unicode disabled, and offsets are byte offsets. `\w` and `\s` are expanded to the
//! Latin-1 characters Python matches for them. Word boundaries (`\b`) only treat ASCII
//! characters as word characters, unlike Python.
//!
//! Not ported: parsing of enumeration, record and array types in packages. VUnit only uses
//! them for com codec generation, which isn't supported.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::bytes::Captures;
use regex::bytes::Regex;
use regex::bytes::RegexBuilder;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::diagnostics::LineIndex;
use crate::diagnostics::Range;
use crate::discovery;
use crate::discovery::TestScan;

#[cfg(test)]
mod tests;

/// The design units, references and component instantiations found in a VHDL file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VhdlDesignFile {
    /// Entity declarations.
    pub entities: Vec<VhdlEntity>,
    /// Architecture bodies.
    pub architectures: Vec<VhdlArchitecture>,
    /// Package declarations and package instantiations at the start of a line.
    pub packages: Vec<VhdlPackage>,
    /// Package bodies.
    pub package_bodies: Vec<VhdlPackageBody>,
    /// Context declarations.
    pub contexts: Vec<VhdlContext>,
    /// Names of instantiated components.
    pub component_instantiations: Vec<String>,
    /// Configuration declarations.
    pub configurations: Vec<VhdlConfiguration>,
    /// References to other design units.
    pub references: Vec<VhdlReference>,
    /// Test markers, for test discovery if the file contains a testbench architecture.
    pub tests: TestScan,
}

/// An entity declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VhdlEntity {
    /// The lowercase entity name.
    pub identifier: String,
    /// The entity name in the case it is declared with.
    pub declared_name: String,
    /// The location of the name.
    pub range: Range,
    /// Constant generics; type, package and subprogram generics are skipped.
    pub generics: Vec<VhdlInterfaceElement>,
    /// Ports.
    pub ports: Vec<VhdlInterfaceElement>,
}

/// A design unit that is known by its name alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VhdlNamedUnit {
    /// The lowercase name; for a package body, the name of its package.
    pub identifier: String,
    /// The location of the name.
    pub range: Range,
}

/// A design unit that belongs to an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VhdlEntityUnit {
    /// The lowercase name of the unit.
    pub identifier: String,
    /// The lowercase name of the entity.
    pub entity: String,
    /// The location of the unit's name.
    pub range: Range,
}

/// An architecture body.
pub type VhdlArchitecture = VhdlEntityUnit;

/// A package declaration or package instantiation.
pub type VhdlPackage = VhdlNamedUnit;

/// A package body.
pub type VhdlPackageBody = VhdlNamedUnit;

/// A context declaration.
pub type VhdlContext = VhdlNamedUnit;

/// A configuration declaration of an entity.
pub type VhdlConfiguration = VhdlEntityUnit;

/// A generic or port of an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VhdlInterfaceElement {
    /// The lowercase name (all names, if the element declares several).
    pub identifier: String,
    /// The mode (`in`, `out`, …), if given.
    pub mode: Option<String>,
    /// The subtype indication.
    pub subtype_indication: VhdlSubtypeIndication,
    /// The default value, if given.
    pub init_value: Option<String>,
}

/// The subtype indication of an interface element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VhdlSubtypeIndication {
    /// The full subtype indication.
    pub code: String,
    /// The type mark.
    pub type_mark: String,
    /// The parenthesized constraint following the type mark, if any.
    pub constraint: Option<String>,
    /// Whether the type mark is `std_logic_vector`.
    pub array_type: bool,
}

/// The kind of a [`VhdlReference`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceType {
    /// A `use` clause or a package instantiation.
    Package,
    /// A context reference.
    Context,
    /// A direct entity instantiation or an entity aspect.
    Entity,
    /// A configuration aspect.
    Configuration,
}

/// A reference to a design unit in a library.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VhdlReference {
    /// The kind of reference.
    pub reference_type: ReferenceType,
    /// The lowercase library name, possibly `work`.
    pub library: String,
    /// The lowercase name of the referenced primary unit.
    pub design_unit: String,
    /// The name selected within the unit (`all`, an architecture, a package item), if any.
    pub name_within: Option<String>,
}

impl VhdlReference {
    /// Creates a reference.
    pub fn new(
        reference_type: ReferenceType,
        library: &str,
        design_unit: &str,
        name_within: Option<&str>,
    ) -> Self {
        Self {
            reference_type,
            library: library.to_owned(),
            design_unit: design_unit.to_owned(),
            name_within: name_within.map(str::to_owned),
        }
    }

    /// Whether this references an entity.
    pub fn is_entity_reference(&self) -> bool {
        self.reference_type == ReferenceType::Entity
    }

    /// Whether this references a package.
    pub fn is_package_reference(&self) -> bool {
        self.reference_type == ReferenceType::Package
    }

    /// Whether this references all names within the unit (`.all`).
    pub fn references_all_names_within(&self) -> bool {
        self.name_within.as_deref() == Some("all")
    }
}

/// The file can't be parsed.
///
/// VUnit then treats it as having no design units.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("{0}")]
pub struct ParseError(String);

/// The version of the parser output.
///
/// Persistent parse caches with another version are discarded, so increment it whenever the
/// parser's results change.
pub const PARSER_VERSION: u32 = 1;

impl VhdlDesignFile {
    /// Parses the raw (Latin-1) contents of a VHDL file.
    ///
    /// # Errors
    ///
    /// Fails like VUnit does, for example on an unbalanced generic or port clause.
    pub fn parse(source: &[u8]) -> Result<Self, ParseError> {
        let mut code = remove_comments(source);
        let lines = LineIndex::new(source);
        let tests = discovery::scan_tests(source, &code, &lines);
        lowercase_latin1(&mut code);
        let range = |captures: &Captures<'_>, group: &str| -> Range {
            lines.range(captures.name(group).map_or(0..0, |found| found.range()))
        };

        let mut entities = Vec::new();
        for captures in ENTITY_START_RE.captures_iter(&code) {
            let start = captures.get_match().start();
            let identifier = group_bytes(&captures, "id");
            let sub_code = &code[start..];
            if let Some(end) = entity_end_re(identifier).find(sub_code) {
                let entity_code = &sub_code[..end.end()];
                let id_span = captures.name("id").map_or(0..0, |found| found.range());
                entities.push(VhdlEntity {
                    identifier: latin1(identifier),
                    declared_name: latin1(&source[id_span]),
                    range: range(&captures, "id"),
                    generics: find_generic_clause(entity_code)?,
                    ports: find_port_clause(entity_code)?,
                });
            }
        }

        let architectures = ARCHITECTURE_RE
            .captures_iter(&code)
            .map(|captures| VhdlArchitecture {
                identifier: group(&captures, "id"),
                entity: group(&captures, "entity_id"),
                range: range(&captures, "id"),
            })
            .collect();

        let mut packages = Vec::new();
        for captures in PACKAGE_START_RE.captures_iter(&code) {
            let start = captures.get_match().start();
            let identifier = group_bytes(&captures, "id");
            if package_end_re(identifier).is_match(&code[start..]) {
                packages.push(VhdlPackage {
                    identifier: latin1(identifier),
                    range: range(&captures, "id"),
                });
            }
        }
        packages.extend(
            PACKAGE_INSTANCE_DECLARATION_RE
                .captures_iter(&code)
                .map(|captures| VhdlPackage {
                    identifier: group(&captures, "new_name"),
                    range: range(&captures, "new_name"),
                }),
        );

        let package_bodies = PACKAGE_BODY_RE
            .captures_iter(&code)
            .map(|captures| VhdlPackageBody {
                identifier: group(&captures, "package"),
                range: range(&captures, "package"),
            })
            .collect();

        let contexts = CONTEXT_START_RE
            .captures_iter(&code)
            .map(|captures| VhdlContext {
                identifier: group(&captures, "id"),
                range: range(&captures, "id"),
            })
            .collect();

        let component_instantiations = COMPONENT_RE
            .captures_iter(&code)
            .map(|captures| group(&captures, "component"))
            .collect();

        let configurations = CONFIGURATION_RE
            .captures_iter(&code)
            .map(|captures| VhdlConfiguration {
                identifier: group(&captures, "id"),
                entity: group(&captures, "entity_id"),
                range: range(&captures, "id"),
            })
            .collect();

        Ok(Self {
            entities,
            architectures,
            packages,
            package_bodies,
            contexts,
            component_instantiations,
            configurations,
            references: find_references(&code),
            tests,
        })
    }
}

/// Python's `\w` for Latin-1 text, as the body of a character class.
const WORD_CLASS: &str = r"0-9A-Za-z_\xAA\xB2\xB3\xB5\xB9\xBA\xBC-\xBE\xC0-\xD6\xD8-\xF6\xF8-\xFF";

/// Python's `\s` for Latin-1 text, as the body of a character class.
const SPACE_CLASS: &str = r"\t\n\x0B\x0C\r \x1C-\x1F\x85\xA0";

/// A VHDL identifier: a basic identifier or an extended identifier.
const ID_PATTERN: &str = r"[A-Za-z]\w*|\\[^\n\r\\]+\\";

const PACKAGE_INSTANCE_PATTERN: &str =
    r"\bpackage\s+(?P<new_name><ID>)\s+is\s+new\s+(?P<lib><ID>)\.(?P<name><ID>)";

#[derive(Clone, Copy)]
pub(crate) struct Flags {
    pub(crate) multi_line: bool,
    pub(crate) dot_all: bool,
}

pub(crate) const NO_FLAGS: Flags = Flags {
    multi_line: false,
    dot_all: false,
};
const MULTILINE: Flags = Flags {
    multi_line: true,
    dot_all: false,
};
const MULTILINE_DOTALL: Flags = Flags {
    multi_line: true,
    dot_all: true,
};

/// Compiles a Python regular expression.
///
/// `<ID>` stands for [`ID_PATTERN`]; `\w` and `\s` get Python's Latin-1 meaning. All
/// expressions are case-insensitive, as in VUnit (the parsed code is lowercase anyway).
#[expect(
    clippy::unwrap_used,
    reason = "the patterns are constants; a broken one fails every test"
)]
pub(crate) fn python_regex(pattern: &str, flags: Flags) -> Regex {
    let pattern = translate_python_classes(&pattern.replace("<ID>", ID_PATTERN));
    RegexBuilder::new(&pattern)
        .unicode(false)
        .case_insensitive(true)
        .multi_line(flags.multi_line)
        .dot_matches_new_line(flags.dot_all)
        .build()
        .unwrap()
}

/// Replaces `\w` and `\s` with explicit Latin-1 classes, inside and outside character classes.
fn translate_python_classes(pattern: &str) -> String {
    let mut translated = String::with_capacity(pattern.len() * 2);
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    translated.push('\\');
                    break;
                };
                let class = match escaped {
                    'w' => WORD_CLASS,
                    's' => SPACE_CLASS,
                    _ => {
                        translated.push('\\');
                        translated.push(escaped);
                        continue;
                    },
                };
                if in_class {
                    translated.push_str(class);
                } else {
                    translated.push('[');
                    translated.push_str(class);
                    translated.push(']');
                }
            },
            '[' if !in_class => {
                in_class = true;
                translated.push('[');
            },
            ']' if in_class => {
                in_class = false;
                translated.push(']');
            },
            _ => translated.push(ch),
        }
    }
    translated
}

/// Escapes Latin-1 bytes for use in a pattern compiled by [`python_regex`].
fn escape_bytes(bytes: &[u8]) -> String {
    let mut escaped = String::new();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            escaped.push(char::from(byte));
        } else {
            write!(escaped, r"\x{byte:02X}").expect("writing to a String succeeds");
        }
    }
    escaped
}

static COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r#"(?:(?:"[^"]*")|(--[^\n]*))"#, MULTILINE));

static COMPONENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r#"(?:<ID>)\s*:\s*(?:component)?\s*(?:(?:<ID>)\.)?(?P<component><ID>)\s*(?:generic|port) map\s*\([\s\w=>,.)(+\-*/'"]*\);"#,
        NO_FLAGS,
    )
});

static PACKAGE_BODY_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r"\bpackage\s+body\s+(?P<package><ID>)\s+is",
        MULTILINE_DOTALL,
    )
});

static CONFIGURATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r"\bconfiguration\s+(?P<id><ID>)\s+of\s+(?P<entity_id><ID>)\s+is",
        MULTILINE,
    )
});

static ARCHITECTURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r"\barchitecture\s+(?P<id><ID>)\s+of\s+(?P<entity_id><ID>)\s+is",
        MULTILINE,
    )
});

static PACKAGE_START_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bpackage\s+(?P<id><ID>)\s+is", MULTILINE));

/// Package instantiations at the start of a line. The indentation heuristic skips nested
/// instantiations.
static PACKAGE_INSTANCE_DECLARATION_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(&format!("^{PACKAGE_INSTANCE_PATTERN}"), MULTILINE));

fn package_end_re(identifier: &[u8]) -> Regex {
    python_regex(
        &format!(
            r"\bend(\s+package)?(\s+{})?[\s]*;",
            escape_bytes(identifier)
        ),
        MULTILINE,
    )
}

static ENTITY_START_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bentity\s+(?P<id><ID>)\s+is", MULTILINE));

fn entity_end_re(identifier: &[u8]) -> Regex {
    python_regex(
        &format!(
            r"\bend[\s]*(entity)?[\s]*({})?[\s]*;",
            escape_bytes(identifier)
        ),
        MULTILINE,
    )
}

static GENERIC_CLAUSE_START_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bgeneric[\s]*\(", MULTILINE));

static PORT_CLAUSE_START_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"^[\s]*port[\s]*\(", MULTILINE));

static LEADING_SEMICOLON_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\A[\s]*;", MULTILINE));

static PACKAGE_GENERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\A\s*package\s+", MULTILINE));

static TYPE_GENERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\A\s*type\s+", MULTILINE));

static FUNCTION_GENERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\A\s*(impure\s+)?(function|procedure)\s+", MULTILINE));

static SIGNAL_KEYWORD_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bsignal\b", MULTILINE));

static CONSTANT_KEYWORD_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bconstant\b", MULTILINE));

static SUBTYPE_INDICATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r"\A[\s]*(?P<type_mark><ID>)[\s]*(?P<constraint>\(.*\))?",
        MULTILINE,
    )
});

static CONTEXT_START_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bcontext\s+(?P<id><ID>)\s+is", MULTILINE));

static USES_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r"\b(?P<use_type>use|context)\s+(?P<id>(?:<ID>)(\.(?:<ID>)){1,2})(?P<extra>(\s*,\s*(?:<ID>)(\.(?:<ID>)){1,2})*)\s*;",
        MULTILINE,
    )
});

static ENTITY_REFERENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    python_regex(
        r"\bentity\s+(?P<lib><ID>)\.(?P<ent><ID>)\s*(\((?P<arch><ID>)\))?",
        MULTILINE,
    )
});

static CONFIGURATION_REFERENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"\bconfiguration\s+(?P<lib><ID>)\.(?P<cfg><ID>)", MULTILINE));

static PACKAGE_INSTANCE_REFERENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(PACKAGE_INSTANCE_PATTERN, MULTILINE));

/// Replaces comments with spaces, so offsets stay the same.
///
/// String literals are skipped, so `"--"` inside a string isn't a comment.
pub fn remove_comments(code: &[u8]) -> Vec<u8> {
    let mut result = code.to_vec();
    for captures in COMMENT_RE.captures_iter(code) {
        if let Some(comment) = captures.get(1) {
            result[comment.range()].fill(b' ');
        }
    }
    result
}

/// Lowercases Latin-1 text like Python's `str.lower()`.
fn lowercase_latin1(code: &mut [u8]) {
    for byte in code {
        if byte.is_ascii_uppercase() || ((0xC0..=0xDE).contains(byte) && *byte != 0xD7) {
            *byte += 0x20;
        }
    }
}

/// Decodes Latin-1 bytes.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().copied().map(char::from).collect()
}

fn group_bytes<'code>(captures: &Captures<'code>, name: &str) -> &'code [u8] {
    captures.name(name).map_or(&[], |found| found.as_bytes())
}

fn group(captures: &Captures<'_>, name: &str) -> String {
    latin1(group_bytes(captures, name))
}

fn optional_group(captures: &Captures<'_>, name: &str) -> Option<String> {
    captures.name(name).map(|found| latin1(found.as_bytes()))
}

/// Whitespace as Python's `str.strip()` and `str.split()` see it in Latin-1 text.
const fn is_python_space(byte: u8) -> bool {
    matches!(
        byte,
        b'\t' | b'\n' | 0x0B | 0x0C | b'\r' | 0x1C..=0x1F | b' ' | 0x85 | 0xA0
    )
}

fn python_strip(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&byte| !is_python_space(byte))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&byte| !is_python_space(byte))
        .map_or(start, |last| last + 1);
    &bytes[start..end]
}

/// Python's `bytes.find(needle)`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Python's `bytes.split(separator)` for a non-empty separator.
fn split_subslice<'text>(text: &'text [u8], separator: &[u8]) -> Vec<&'text [u8]> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(index) = find_subslice(rest, separator) {
        parts.push(&rest[..index]);
        rest = &rest[index + separator.len()..];
    }
    parts.push(rest);
    parts
}

/// Returns the offset just past the parenthesis that closes an already opened one.
fn find_closing_parenthesis(code: &[u8]) -> Result<usize, ParseError> {
    let mut count = 1_usize;
    for (index, &byte) in code.iter().enumerate() {
        match byte {
            b'(' => count += 1,
            b')' => count -= 1,
            _ => continue,
        }
        if count == 0 {
            return Ok(index + 1);
        }
    }
    Err(ParseError(
        "failed to find the closing parenthesis of a generic or port clause".to_owned(),
    ))
}

/// Finds an interface list that starts with `start` (ending in `(`) and is followed by `;`,
/// and returns the code from `start` up to and including the semicolon.
fn find_interface_clause<'code>(
    code: &'code [u8],
    start: &Regex,
) -> Result<Option<&'code [u8]>, ParseError> {
    let Some(clause_start) = start.find(code) else {
        return Ok(None);
    };
    let closing = clause_start.end() + find_closing_parenthesis(&code[clause_start.end()..])?;
    Ok(LEADING_SEMICOLON_RE
        .find(&code[closing..])
        .map(|semicolon| &code[clause_start.start()..closing + semicolon.end()]))
}

/// The text between the first `(` and the last `)` of an interface clause.
fn interface_list(clause: &[u8]) -> &[u8] {
    let start = clause
        .iter()
        .position(|&byte| byte == b'(')
        .map_or(0, |index| index + 1);
    // Python's `rfind` returns -1 when nothing is found, which drops the last character.
    let end = clause
        .iter()
        .rposition(|&byte| byte == b')')
        .unwrap_or_else(|| clause.len().saturating_sub(1));
    clause.get(start..end).unwrap_or_default()
}

fn find_generic_clause(code: &[u8]) -> Result<Vec<VhdlInterfaceElement>, ParseError> {
    let Some(clause) = find_interface_clause(code, &GENERIC_CLAUSE_START_RE)? else {
        return Ok(Vec::new());
    };
    let mut generics = Vec::new();
    for element in split_not_in_parentheses(interface_list(clause), b';') {
        if python_strip(element).is_empty()
            || PACKAGE_GENERIC_RE.is_match(element)
            || TYPE_GENERIC_RE.is_match(element)
            || FUNCTION_GENERIC_RE.is_match(element)
        {
            continue;
        }
        generics.push(parse_interface_element(element, false, true)?);
    }
    Ok(generics)
}

fn find_port_clause(code: &[u8]) -> Result<Vec<VhdlInterfaceElement>, ParseError> {
    let Some(clause) = find_interface_clause(code, &PORT_CLAUSE_START_RE)? else {
        return Ok(Vec::new());
    };
    interface_list(clause)
        .split(|&byte| byte == b';')
        .filter(|element| !python_strip(element).is_empty())
        .map(|element| parse_interface_element(element, true, false))
        .collect()
}

/// Splits at `separator`, but not inside parentheses or string literals.
fn split_not_in_parentheses(text: &[u8], separator: u8) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut depth = 0_i64;
    let mut quoted = false;
    let mut escaped = false;
    let mut part_start = 0;
    for (index, &byte) in text.iter().enumerate() {
        if byte == b'"' && !escaped {
            if text.get(index + 1) == Some(&b'"') {
                escaped = true;
            } else {
                quoted = !quoted;
            }
        } else {
            escaped = false;
        }

        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {},
        }

        if byte == separator && depth == 0 && !quoted {
            parts.push(&text[part_start..index]);
            part_start = index + 1;
        }
    }
    if part_start < text.len() {
        parts.push(&text[part_start..]);
    }
    parts
}

fn parse_interface_element(
    code: &[u8],
    is_signal: bool,
    is_constant: bool,
) -> Result<VhdlInterfaceElement, ParseError> {
    let mut code = code.to_vec();
    if is_signal {
        code = SIGNAL_KEYWORD_RE.replace_all(&code, &b""[..]).into_owned();
    }
    if is_constant {
        code = CONSTANT_KEYWORD_RE
            .replace_all(&code, &b""[..])
            .into_owned();
    }
    let error = || {
        ParseError(format!(
            "failed to parse interface element '{}'",
            latin1(python_strip(&code))
        ))
    };

    let colon_parts: Vec<&[u8]> = code.split(|&byte| byte == b':').collect();
    let identifier = latin1(python_strip(colon_parts[0]));
    let after_colon = python_strip(colon_parts.get(1).ok_or_else(error)?);

    // Python's `after_colon.split(None, 1)`.
    let first_end = after_colon
        .iter()
        .position(|&byte| is_python_space(byte))
        .unwrap_or(after_colon.len());
    let first = &after_colon[..first_end];
    if first.is_empty() {
        return Err(error());
    }
    let rest = python_strip(&after_colon[first_end..]);

    let (mode, subtype_code) = if is_mode(first) {
        if rest.is_empty() {
            return Err(error());
        }
        (Some(latin1(first)), rest)
    } else {
        (None, after_colon)
    };
    let subtype_indication = parse_subtype_indication(subtype_code).ok_or_else(error)?;

    let init_value = split_subslice(&code, b":=")
        .get(1)
        .map(|value| latin1(python_strip(value)));

    Ok(VhdlInterfaceElement {
        identifier,
        mode,
        subtype_indication,
        init_value,
    })
}

fn is_mode(code: &[u8]) -> bool {
    matches!(code, b"in" | b"out" | b"inout" | b"buffer" | b"linkage")
}

fn parse_subtype_indication(code: &[u8]) -> Option<VhdlSubtypeIndication> {
    let captures = SUBTYPE_INDICATION_RE.captures(code)?;
    let type_mark = group(&captures, "type_mark");
    let array_type = type_mark == "std_logic_vector";
    Some(VhdlSubtypeIndication {
        code: latin1(code),
        type_mark,
        constraint: optional_group(&captures, "constraint"),
        array_type,
    })
}

fn find_references(code: &[u8]) -> Vec<VhdlReference> {
    let mut references = Vec::new();

    for captures in USES_RE.captures_iter(code) {
        let reference_type = if group_bytes(&captures, "use_type") == b"use" {
            ReferenceType::Package
        } else {
            ReferenceType::Context
        };
        let mut ids = vec![python_strip(group_bytes(&captures, "id"))];
        let extra = group_bytes(&captures, "extra");
        if !extra.is_empty() {
            ids.extend(extra.split(|&byte| byte == b',').skip(1).map(python_strip));
        }
        for id in ids {
            let parts: Vec<String> = id.split(|&byte| byte == b'.').map(latin1).collect();
            let (library, design_unit) = (&parts[0], parts.get(1).map_or("", String::as_str));
            if parts.len() > 2 {
                for name_within in &parts[2..] {
                    references.push(VhdlReference::new(
                        reference_type,
                        library,
                        design_unit,
                        Some(name_within),
                    ));
                }
            } else {
                references.push(VhdlReference::new(
                    reference_type,
                    library,
                    design_unit,
                    None,
                ));
            }
        }
    }

    references.extend(
        ENTITY_REFERENCE_RE
            .captures_iter(code)
            .map(|captures| VhdlReference {
                reference_type: ReferenceType::Entity,
                library: group(&captures, "lib"),
                design_unit: group(&captures, "ent"),
                name_within: optional_group(&captures, "arch"),
            }),
    );

    references.extend(
        CONFIGURATION_REFERENCE_RE
            .captures_iter(code)
            .map(|captures| VhdlReference {
                reference_type: ReferenceType::Configuration,
                library: group(&captures, "lib"),
                design_unit: group(&captures, "cfg"),
                name_within: None,
            }),
    );

    references.extend(
        PACKAGE_INSTANCE_REFERENCE_RE
            .captures_iter(code)
            .map(|captures| VhdlReference {
                reference_type: ReferenceType::Package,
                library: group(&captures, "lib"),
                design_unit: group(&captures, "name"),
                name_within: None,
            }),
    );

    references
}
