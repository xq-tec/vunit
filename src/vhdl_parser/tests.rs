// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests ported from `tests/unit/test_vhdl_parser.py`.
//!
//! AI NOTICE: Generated, minimally reviewed.

use super::*;
use crate::diagnostics::Position;

fn parse(code: &str) -> VhdlDesignFile {
    VhdlDesignFile::parse(code.as_bytes()).unwrap()
}

fn parse_single_entity(code: &str) -> VhdlEntity {
    let design_file = parse(code);
    assert_eq!(design_file.entities.len(), 1);
    design_file.entities.into_iter().next().unwrap()
}

fn parse_single_package(code: &str) -> VhdlPackage {
    let design_file = parse(code);
    assert_eq!(design_file.packages.len(), 1);
    design_file.packages.into_iter().next().unwrap()
}

fn parse_single_context(code: &str) -> VhdlContext {
    let design_file = parse(code);
    assert_eq!(design_file.contexts.len(), 1);
    design_file.contexts.into_iter().next().unwrap()
}

fn generic_names(entity: &VhdlEntity) -> Vec<&str> {
    entity
        .generics
        .iter()
        .map(|generic| generic.identifier.as_str())
        .collect()
}

#[test]
fn parsing_empty() {
    let design_file = parse("");
    assert!(design_file.entities.is_empty());
    assert!(design_file.packages.is_empty());
    assert!(design_file.architectures.is_empty());
}

#[test]
fn parsing_simple_entity() {
    let entity = parse_single_entity("entity simple is\nend entity;\n");
    assert_eq!(entity.identifier, "simple");
    assert!(entity.ports.is_empty());
    assert!(entity.generics.is_empty());
}

#[test]
fn entity_range_covers_name() {
    let entity = parse_single_entity("\n  entity Simple is\nend entity;\n");
    assert_eq!(
        entity.range,
        Range {
            start: Position {
                line: 2,
                column: 10
            },
            end: Position {
                line: 2,
                column: 15
            },
        }
    );
}

#[test]
fn parsing_entity_with_package_generic() {
    let entity = parse_single_entity(
        "\
entity ent is
  generic (
    package p is new work.pkg generic map (c => 1, b => 2);
    package_g : integer
    );
end entity;
",
    );
    assert_eq!(entity.identifier, "ent");
    assert!(entity.ports.is_empty());
    assert_eq!(generic_names(&entity), ["package_g"]);
    assert_eq!(entity.generics[0].subtype_indication.type_mark, "integer");
}

#[test]
fn parsing_entity_with_type_generic() {
    let entity = parse_single_entity(
        "\
entity ent is
  generic (
    type t;
    type_g : integer
    );
end entity;
",
    );
    assert_eq!(generic_names(&entity), ["type_g"]);
    assert_eq!(entity.generics[0].subtype_indication.type_mark, "integer");
}

#[test]
fn parsing_entity_with_string_semicolon_colon() {
    let entity = parse_single_entity(
        r#"entity ent is
  generic (
        const : string := "a;a";
        const2 : string := ";a""a;a";
        const3 : string := ": a b c :"
    );
end entity;
"#,
    );
    assert_eq!(generic_names(&entity), ["const", "const2", "const3"]);
    for generic in &entity.generics {
        assert_eq!(generic.subtype_indication.type_mark, "string");
    }
    assert_eq!(entity.generics[0].init_value.as_deref(), Some(r#""a;a""#));
    assert_eq!(
        entity.generics[1].init_value.as_deref(),
        Some(r#"";a""a;a""#)
    );
    assert_eq!(
        entity.generics[2].init_value.as_deref(),
        Some(r#"": a b c :""#)
    );
}

#[test]
fn parsing_entity_with_function_generic() {
    let entity = parse_single_entity(
        "\
entity ent is
  generic (
    function f(a : integer; b : integer) return integer;
    function_g : boolean;
    impure function if(a : integer; b : integer) return integer;
    procedure_g : boolean;
    procedure p(a : integer; b : integer)
    );
end entity;
",
    );
    assert_eq!(generic_names(&entity), ["function_g", "procedure_g"]);
    assert_eq!(entity.generics[0].subtype_indication.type_mark, "boolean");
    assert_eq!(entity.generics[1].subtype_indication.type_mark, "boolean");
}

#[test]
fn getting_entities_from_design_file() {
    let design_file = parse(
        "
entity entity1 is
end entity;

package package1 is
end package;

entity entity2 is
end entity;
",
    );
    let names: Vec<_> = design_file
        .entities
        .iter()
        .map(|entity| entity.identifier.as_str())
        .collect();
    assert_eq!(names, ["entity1", "entity2"]);
}

#[test]
fn getting_architectures_from_design_file() {
    let design_file = parse(
        "
entity foo is
end entity;

architecture rtl of foo is
begin
end architecture;
",
    );
    assert_eq!(design_file.entities.len(), 1);
    assert_eq!(design_file.architectures.len(), 1);
    assert_eq!(design_file.architectures[0].entity, "foo");
    assert_eq!(design_file.architectures[0].identifier, "rtl");
}

#[test]
fn parsing_references() {
    let design_file = parse(
        "
library name1;
 use name1.foo.all;

library ieee ;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

use name1.bla.all;

library lib1,lib2, lib3;
use lib1.foo, lib2.bar,lib3.xyz;

context name1.is_identifier;

entity work1.foo1
entity work1.foo1(a1)
for all : bar use entity work2.foo2
for all : bar use entity work2.foo2 (a2)
for foo : bar use configuration work.cfg

entity foo is -- False
configuration bar of ent -- False

package new_pkg is new lib.pkg;
",
    );
    let mut references = design_file.references;
    references.sort();
    let mut expected = vec![
        VhdlReference::new(ReferenceType::Configuration, "work", "cfg", None),
        VhdlReference::new(ReferenceType::Context, "name1", "is_identifier", None),
        VhdlReference::new(ReferenceType::Entity, "work1", "foo1", Some("a1")),
        VhdlReference::new(ReferenceType::Entity, "work1", "foo1", None),
        VhdlReference::new(ReferenceType::Entity, "work2", "foo2", Some("a2")),
        VhdlReference::new(ReferenceType::Entity, "work2", "foo2", None),
        VhdlReference::new(ReferenceType::Package, "ieee", "numeric_std", Some("all")),
        VhdlReference::new(
            ReferenceType::Package,
            "ieee",
            "std_logic_1164",
            Some("all"),
        ),
        VhdlReference::new(ReferenceType::Package, "lib1", "foo", None),
        VhdlReference::new(ReferenceType::Package, "lib2", "bar", None),
        VhdlReference::new(ReferenceType::Package, "lib3", "xyz", None),
        VhdlReference::new(ReferenceType::Package, "name1", "bla", Some("all")),
        VhdlReference::new(ReferenceType::Package, "name1", "foo", Some("all")),
        VhdlReference::new(ReferenceType::Package, "lib", "pkg", None),
    ];
    expected.sort();
    assert_eq!(references, expected);
}

#[test]
fn references_are_lowercase() {
    let design_file = parse("use Lib.Pkg.All;");
    assert_eq!(
        design_file.references,
        [VhdlReference::new(
            ReferenceType::Package,
            "lib",
            "pkg",
            Some("all")
        )]
    );
}

fn check_generics_max_value_enable_foo(entity: &VhdlEntity) {
    assert_eq!(entity.identifier, "name");
    assert!(entity.ports.is_empty());
    assert_eq!(generic_names(entity), ["max_value", "enable_foo"]);

    let max_value = &entity.generics[0];
    assert_eq!(max_value.init_value.as_deref(), Some("(2-19)*4"));
    assert_eq!(max_value.mode, None);
    assert_eq!(
        max_value.subtype_indication.code,
        "integer range 2-2 to 2**10"
    );
    assert_eq!(max_value.subtype_indication.type_mark, "integer");

    let enable_foo = &entity.generics[1];
    assert_eq!(enable_foo.init_value, None);
    assert_eq!(enable_foo.mode, None);
    assert_eq!(enable_foo.subtype_indication.code, "boolean");
    assert_eq!(enable_foo.subtype_indication.type_mark, "boolean");
}

#[test]
fn parsing_entity_with_generics() {
    let entity = parse_single_entity(
        "\
entity name is
   generic (max_value : integer range 2-2 to 2**10 := (2-19)*4;
            enable_foo : boolean
   );
end entity;
",
    );
    check_generics_max_value_enable_foo(&entity);
}

#[test]
fn parsing_entity_with_generics_and_trailing_semicolon() {
    let entity = parse_single_entity(
        "\
entity name is
   generic (max_value : integer range 2-2 to 2**10 := (2-19)*4;
            enable_foo : boolean  ;
   );
end entity;
",
    );
    check_generics_max_value_enable_foo(&entity);
}

#[test]
fn parsing_entity_with_generics_corner_cases() {
    parse_single_entity("entity name is end entity;\n");

    for code in [
        "entity name is generic(g : t); end entity;\n",
        "entity name is generic\n(\ng : t\n);\nend entity;\n",
        "end architecture; entity name is generic\n(\ng : t\n);\nend entity;\n",
    ] {
        assert_eq!(generic_names(&parse_single_entity(code)), ["g"]);
    }

    let entity = parse_single_entity("entity name is foo_generic\n(\ng : t\n);\nend entity;\n");
    assert!(entity.generics.is_empty());
}

fn check_entity_with_ports(entity: &VhdlEntity) {
    assert_eq!(entity.identifier, "name");
    assert!(entity.generics.is_empty());

    let expected = [
        ("clk", "in", "std_logic", None),
        (
            "data",
            "out",
            "std_logic_vector(11-1 downto 0)",
            Some("(11-1 downto 0)"),
        ),
        ("signal_data2", "in", "std_logic", None),
        ("data3", "in", "signal_type", None),
        ("data4_signal", "in", "std_logic", None),
        ("data5", "in", "type_signal", None),
        ("clk2", "in", "std_logic", None),
        (
            "data7",
            "out",
            "std_logic_vector(11-1 downto 0)",
            Some("(11-1 downto 0)"),
        ),
        ("signal_data8", "in", "std_logic", None),
        ("data9", "in", "signal_type", None),
        ("data10_signal", "in", "std_logic", None),
        ("data11", "in", "type_signal", None),
    ];
    assert_eq!(entity.ports.len(), expected.len());
    for (port, (identifier, mode, code, constraint)) in entity.ports.iter().zip(expected) {
        assert_eq!(port.identifier, identifier);
        assert_eq!(port.init_value, None);
        assert_eq!(port.mode.as_deref(), Some(mode));
        assert_eq!(port.subtype_indication.code, code);
        let type_mark = code.split('(').next().unwrap();
        assert_eq!(port.subtype_indication.type_mark, type_mark);
        assert_eq!(port.subtype_indication.constraint.as_deref(), constraint);
    }
}

const PORTS: &str = "\
entity name is
port (
    clk : in std_logic;
 \t data : out std_logic_vector(11-1 downto 0);
    signal_data2 : in std_logic;
\t  data3 :\tin signal_type;
    data4_signal : in\tstd_logic;
    data5\t: in type_signal;
\t\tsignal clk2 : in std_logic;
    signal\tdata7 : out std_logic_vector(11-1 downto 0);
    signal signal_data8 : in std_logic;
    signal\t data9 : in signal_type;
    signal data10_signal :\tin std_logic;\t
    signal \t data11 : in type_signal
";

#[test]
fn parsing_entity_with_ports() {
    let entity = parse_single_entity(&format!("{PORTS});\nend entity;\n"));
    check_entity_with_ports(&entity);
}

#[test]
fn parsing_entity_with_ports_and_trailing_semicolon() {
    let entity = parse_single_entity(&format!("{PORTS};);\nend entity;\n"));
    check_entity_with_ports(&entity);
}

#[test]
fn parsing_simple_package_body() {
    let design_file = parse("package body simple is\nbegin\nend package body;\n");
    assert_eq!(design_file.package_bodies.len(), 1);
    assert_eq!(design_file.package_bodies[0].identifier, "simple");
}

#[test]
fn parsing_simple_package() {
    let package = parse_single_package("package simple is\nend package;\n");
    assert_eq!(package.identifier, "simple");
}

#[test]
fn parsing_generic_package() {
    let package = parse_single_package(
        "\
package pkg is
  generic (c : integer;
           b : bit_vector(4-1 downto 0));
end package;
",
    );
    assert_eq!(package.identifier, "pkg");
}

#[test]
fn parsing_generic_package_instance() {
    for code in [
        "package instance_pkg is new work.generic_pkg;\n",
        "\n\npackage instance_pkg is\nnew work.generic_pkg;\n",
        "package instance_pkg is new work.generic_pkg\n        generic map (foo : boolean);\n",
    ] {
        assert_eq!(parse_single_package(code).identifier, "instance_pkg");
    }

    // Nested packages are skipped, using the heuristic that they're indented.
    let design_file = parse(" package instance_pkg is new work.generic_pkg\n");
    assert!(design_file.packages.is_empty());
}

#[test]
fn parsing_context() {
    let context = parse_single_context(
        "\
context foo is
  library bar;
  use bar.bar_pkg.all;
end context;

context name1.is_identifier; -- Should be ignored
",
    );
    assert_eq!(context.identifier, "foo");

    let other_context = parse_single_context(
        "\
context identifier is
  library bar;
  use bar.bar_pkg.all;
end context identifier;
",
    );
    assert_eq!(other_context.identifier, "identifier");
}

#[test]
fn getting_component_instantiations_from_design_file() {
    let design_file = parse(
        r#"
entity top is
end entity;

architecture arch of top is
begin
    labelFoo : component foo
    generic map(WIDTH => 16)
    port map(clk => '1',
             rst => '0',
             in_vec => record_reg.input_signal,
             output => some_signal(UPPER_CONSTANT-1 downto LOWER_CONSTANT+1));

    label2Foo : foo2
    port map(clk => '1',
             rst => '0',
             output => "00");

    label3Foo : foo3 port map (clk, rst, X"A");

end architecture;

"#,
    );
    assert_eq!(
        design_file.component_instantiations,
        ["foo", "foo2", "foo3"]
    );
}

#[test]
fn remove_comments_keeps_offsets() {
    assert_eq!(remove_comments(b"a\n-- foo  \nb"), b"a\n        \nb");
}

#[test]
fn two_adjacent_hyphens_in_a_literal() {
    let stimulus = br#"signal a : std_logic_vector(3 downto 0) := "----";"#;
    assert_eq!(remove_comments(stimulus), stimulus);
}

#[test]
fn external_identifier() {
    let design_file = parse(
        r"
entity standard_identifier is
  generic (
    -- Extended identifiers with parenthesis will be accepted if they are balanced.
    -- Otherwise they will interfere with finding the closing parenthesis to the
    -- generic clause. Same thing with port clause. This is an acceptable limitation
    -- for now.
    \foo(bar)\ : integer
    );
end entity;

entity non-standard-identifier is -- This entity won't be found because of illegal identifier pattern.
end package;

entity \extended-identifier\ is
end entity \extended-identifier\;

package \a.package\ is
end package \a.package\;
",
    );
    let names: Vec<_> = design_file
        .entities
        .iter()
        .map(|entity| entity.identifier.as_str())
        .collect();
    assert_eq!(names, ["standard_identifier", r"\extended-identifier\"]);
    assert_eq!(design_file.packages.len(), 1);
    assert_eq!(design_file.packages[0].identifier, r"\a.package\");
}

#[test]
fn unbalanced_port_clause_is_a_parse_error() {
    // `test_recovers_from_parse_error` in `test_project.py`.
    VhdlDesignFile::parse(b"entity foo is\n port (foo : in bit;\nend entity;\n").unwrap_err();
}

#[test]
fn latin1_identifiers_and_offsets() {
    // 0xC4 is 'Ä', which lowercases to 0xE4 'ä'. Comments with Latin-1 bytes keep offsets.
    let design_file =
        VhdlDesignFile::parse(b"-- \xFC\xFC\nentity a\xC4bc is\nend entity;\n").unwrap();
    assert_eq!(design_file.entities.len(), 1);
    assert_eq!(design_file.entities[0].identifier, "aäbc");
    assert_eq!(
        design_file.entities[0].range.start,
        Position { line: 2, column: 8 }
    );
}

#[test]
fn split_not_in_parentheses_handles_quotes() {
    let parts = split_not_in_parentheses(br#"a := ";"; b := f(x; y); c"#, b';');
    assert_eq!(
        parts,
        [&br#"a := ";""#[..], &b" b := f(x; y)"[..], &b" c"[..]]
    );
}

#[test]
fn translates_python_classes() {
    assert_eq!(
        translate_python_classes(r"a\s+[\s\w=]\."),
        format!(r"a[{SPACE_CLASS}]+[{SPACE_CLASS}{WORD_CLASS}=]\.")
    );
}
