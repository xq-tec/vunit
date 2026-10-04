// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests ported from `tests/unit/test_project.py`.
//!
//! `VUnit` checks dependencies by marking a file as recompiled and checking which files then
//! need recompilation. Here, `assert_compiles` checks the dependency graph and the compile
//! order directly. Recompilation tests belong to the `compile` module.
//!
//! AI NOTICE: Generated, minimally reviewed.

use super::*;
use crate::diagnostics::Severity;
use crate::test_support::add_vhdl;

struct TestProject {
    project: Project,
}

impl TestProject {
    fn new() -> Self {
        Self {
            project: Project::new(),
        }
    }

    fn add_library(&mut self, name: &str) -> LibraryId {
        self.project
            .add_library(name, VhdlStandard::Vhdl2008, None)
            .unwrap()
    }

    fn add(&mut self, library: &str, path: &str, code: &str) -> FileId {
        add_vhdl(&mut self.project, library, Utf8Path::new(path), code)
    }

    fn compile_order(&self) -> Vec<FileId> {
        self.project.compile_order(None).files.unwrap()
    }

    fn compile_order_of(&self, targets: &[FileId]) -> Vec<FileId> {
        self.project.compile_order(Some(targets)).files.unwrap()
    }

    fn depends_on(&self, dependent: FileId, dependency: FileId, implementation: bool) -> bool {
        self.project
            .dependency_graph(implementation)
            .graph
            .dependents([dependency])
            .contains(&dependent)
    }

    /// `dependency` is compiled before `dependent`, which needs recompiling when `dependency`
    /// changes.
    fn assert_compiles(&self, dependency: FileId, before: FileId) {
        assert!(
            self.depends_on(before, dependency, false),
            "{} doesn't depend on {}",
            self.project.file(before).path,
            self.project.file(dependency).path
        );
        let order = self.compile_order();
        let position = |id| order.iter().position(|&other| other == id).unwrap();
        assert!(position(dependency) < position(before));
    }

    fn assert_not_compiles(&self, dependency: FileId, before: FileId) {
        assert!(!self.depends_on(before, dependency, false));
    }

    fn library(&self, name: &str) -> &Library {
        self.project
            .library(self.project.find_library(name).unwrap())
    }

    fn architecture_names(&self, library: &str, entity: &str) -> Vec<(String, String)> {
        self.library(library)
            .architectures(entity)
            .iter()
            .map(|(name, file)| (name.clone(), self.project.file(*file).path.to_string()))
            .collect()
    }

    fn diagnostic_messages(&self) -> Vec<String> {
        self.project
            .diagnostics()
            .iter()
            .map(ToString::to_string)
            .collect()
    }
}

fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|&(first, second)| (first.to_owned(), second.to_owned()))
        .collect()
}

fn three_file_project() -> (TestProject, [FileId; 3]) {
    let mut test = TestProject::new();
    test.add_library("lib");
    let file1 = test.add(
        "lib",
        "file1.vhd",
        "\
entity module1 is
end entity;

architecture arch of module1 is
begin
  report \"Updated\";
end architecture;
",
    );
    let file2 = test.add(
        "lib",
        "file2.vhd",
        "\
entity module2 is
end entity;

architecture arch of module2 is
begin
  module1_inst : entity lib.module1;
end architecture;
",
    );
    let file3 = test.add(
        "lib",
        "file3.vhd",
        "\
entity module3 is
end entity;

architecture arch of module3 is
begin
  module1_inst : entity work.module2;
end architecture;
",
    );
    (test, [file1, file2, file3])
}

#[test]
fn parses_entity_architecture() {
    let mut test = TestProject::new();
    test.add_library("lib");
    // The architecture is added before the entity, to test that they are paired later.
    test.add(
        "lib",
        "file2.vhd",
        "architecture arch3 of foo is\nbegin\nend architecture;\n",
    );
    let file1 = test.add(
        "lib",
        "file1.vhd",
        "\
entity foo is
end entity;

architecture arch of foo is
begin
end architecture;

architecture arch2 of foo is
begin
end architecture;
",
    );
    assert_eq!(
        test.library("lib").primary_unit("foo"),
        Some(PrimaryUnit {
            kind: UnitKind::Entity,
            file: file1
        })
    );
    assert_eq!(
        test.architecture_names("lib", "foo"),
        pairs(&[
            ("arch3", "file2.vhd"),
            ("arch", "file1.vhd"),
            ("arch2", "file1.vhd"),
        ])
    );

    test.add(
        "lib",
        "file3.vhd",
        "architecture arch4 of foo is\nbegin\nend architecture;\n",
    );
    assert_eq!(test.architecture_names("lib", "foo").len(), 4);
    let units = &test.project.file(file1).design_units;
    assert_eq!(units[1].kind, UnitKind::Architecture);
    assert_eq!(units[1].primary_unit.as_deref(), Some("foo"));
}

#[test]
fn parses_package() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let file = test.add(
        "lib",
        "file1.vhd",
        "\
package foo is
end package;

package body foo is
begin
end package body;
",
    );
    let library = test.library("lib");
    assert_eq!(library.primary_unit("foo").unwrap().kind, UnitKind::Package);
    assert_eq!(library.package_body("foo"), Some(file));
}

#[test]
fn unparsed_file_has_no_design_units() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let file = test.add(
        "lib",
        "file.vhd",
        "entity foo is\n port (foo : in bit;\nend entity;\n",
    );
    assert!(test.project.file(file).design_units.is_empty());
}

#[test]
fn finds_entity_instantiation_dependencies() {
    let (test, [file1, file2, file3]) = three_file_project();
    test.assert_compiles(file1, file2);
    test.assert_compiles(file2, file3);
}

#[test]
fn primary_with_same_name_in_multiple_libraries_secondary_dependency() {
    let mut test = TestProject::new();
    test.add_library("lib1");
    test.add_library("lib2");
    let foo_arch = test.add(
        "lib1",
        "foo_arch.vhd",
        "architecture arch of foo is\nbegin\nend architecture;\n",
    );
    let foo1_ent = test.add(
        "lib1",
        "foo1_ent.vhd",
        "entity foo is\nport (signal bar : boolean);\nend entity;\n",
    );
    test.add("lib2", "foo2_ent.vhd", "entity foo is\nend entity;\n");
    test.assert_compiles(foo1_ent, foo_arch);
}

#[test]
fn finds_entity_architecture_dependencies() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let entity = test.add("lib", "entity.vhd", "entity foo is\nend entity;\n");
    let arch1 = test.add(
        "lib",
        "arch1.vhd",
        "architecture arch1 of foo is\nbegin\nend architecture;\n",
    );
    let arch2 = test.add(
        "lib",
        "arch2.vhd",
        "architecture arch2 of foo is\nbegin\nend architecture;\n",
    );
    test.assert_compiles(entity, arch1);
    test.assert_compiles(entity, arch2);
}

#[test]
fn finds_package_dependencies() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let package = test.add("lib", "package.vhd", "package foo is\nend package;\n");
    let body = test.add(
        "lib",
        "body.vhd",
        "package body foo is\nbegin\nend package body;\n",
    );
    test.assert_compiles(package, body);
}

fn module_package_and_body(add_body: bool) -> (TestProject, FileId, Option<FileId>, FileId) {
    let mut test = TestProject::new();
    test.add_library("lib");
    let package = test.add("lib", "package.vhd", "package pkg is\nend package;\n");
    let body = add_body.then(|| {
        test.add(
            "lib",
            "body.vhd",
            "package body pkg is\nbegin\nend package body;\n",
        )
    });
    test.add_library("lib2");
    let module = test.add(
        "lib2",
        "module.vhd",
        "\
library lib;
use lib.pkg.all;

entity module is
end entity;

architecture arch of module is
begin
end architecture;
",
    );
    (test, package, body, module)
}

#[test]
fn finds_use_package_dependencies() {
    let (test, package, body, module) = module_package_and_body(true);
    let body = body.unwrap();
    test.assert_compiles(package, body);
    test.assert_compiles(package, module);
    test.assert_not_compiles(body, module);
}

#[test]
fn finds_extra_package_body_dependencies() {
    // `VUnit` adds these with `depend_on_package_body`; here they are implementation
    // dependencies.
    let (test, package, body, module) = module_package_and_body(true);
    let body = body.unwrap();
    assert!(test.depends_on(body, package, true));
    assert!(test.depends_on(module, body, true));
    assert!(test.depends_on(module, package, true));
}

#[test]
fn package_can_have_no_body() {
    let (test, package, _, module) = module_package_and_body(false);
    test.assert_compiles(package, module);
    assert!(test.depends_on(module, package, true));
}

#[test]
fn finds_use_package_dependencies_case_insensitive() {
    for (library_clause, use_clause) in [("lib", "Lib"), ("Lib", "lib"), ("LIB", "Lib")] {
        let mut test = TestProject::new();
        test.add_library("Lib");
        let package = test.add(
            "Lib",
            "package.vhd",
            "package pkg is\nend package;\n\npackage body PKG is\nbegin\nend package body;\n",
        );
        test.add_library("lib2");
        let module = test.add(
            "lib2",
            "module.vhd",
            &format!("library {library_clause};\nuse {use_clause}.PKG.all;\n"),
        );
        test.assert_compiles(package, module);
    }
}

#[test]
fn error_on_case_insensitive_library_name_conflict() {
    let mut test = TestProject::new();
    test.add_library("Lib");
    let error = test
        .project
        .add_library("lib", VhdlStandard::Vhdl2008, None)
        .unwrap_err();
    assert_eq!(
        error,
        LibraryError::Duplicate {
            name: "lib".to_owned(),
            existing: "Lib".to_owned(),
        }
    );
}

#[test]
fn error_on_adding_duplicate_library() {
    let mut test = TestProject::new();
    test.add_library("lib");
    test.project
        .add_library("lib", VhdlStandard::Vhdl2008, None)
        .unwrap_err();
}

#[test]
fn error_on_work_library() {
    let mut test = TestProject::new();
    assert_eq!(
        test.project
            .add_library("Work", VhdlStandard::Vhdl2008, None),
        Err(LibraryError::Work)
    );
}

#[test]
fn package_instantiation_dependencies_on_generic_package() {
    let mut test = TestProject::new();
    test.add_library("pkg_lib");
    let pkg = test.add("pkg_lib", "pkg.vhd", "package pkg is\nend package;\n");
    test.add_library("lib");
    let ent = test.add(
        "lib",
        "ent.vhd",
        "\
library pkg_lib;

entity ent is
end entity;

architecture a of ent is
   package pkg_inst is new pkg_lib.pkg;
begin
end architecture;
",
    );
    test.assert_compiles(pkg, ent);
}

#[test]
fn package_instantiation_dependencies_on_instantiated_package() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let generic_pkg = test.add(
        "lib",
        "generic_pkg.vhd",
        "package generic_pkg is\n  generic (foo : boolean);\nend package;\n",
    );
    let instance_pkg = test.add(
        "lib",
        "instance_pkg.vhd",
        "package instance_pkg is new work.generic_pkg\n  generic map (foo => false);\n",
    );
    let user = test.add("lib", "user.vhd", "use work.instance_pkg;\n");
    test.assert_compiles(generic_pkg, instance_pkg);
    test.assert_compiles(instance_pkg, user);
}

#[test]
fn finds_context_dependencies() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let context = test.add("lib", "context.vhd", "context foo is\nend context;\n");
    test.add_library("lib2");
    let module = test.add(
        "lib2",
        "module.vhd",
        "\
library lib;
context lib.foo;

entity module is
end entity;

architecture arch of module is
begin
end architecture;
",
    );
    test.assert_compiles(context, module);
}

#[test]
fn finds_configuration_dependencies() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let cfg = test.add(
        "lib",
        "cfg.vhd",
        "configuration cfg of ent is\nend configuration;\n",
    );
    let ent = test.add("lib", "ent.vhd", "entity ent is\nend entity;\n");
    let ent_a1 = test.add(
        "lib",
        "ent_a1.vhd",
        "architecture a1 of ent is\nbegin\nend architecture;\n",
    );
    let ent_a2 = test.add(
        "lib",
        "ent_a2.vhd",
        "architecture a2 of ent is\nbegin\nend architecture;\n",
    );
    test.assert_compiles(ent, cfg);
    test.assert_compiles(ent_a1, cfg);
    test.assert_compiles(ent_a2, cfg);
}

#[test]
fn finds_configuration_reference_dependencies() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let cfg = test.add(
        "lib",
        "cfg.vhd",
        "configuration cfg of ent is\nend configuration;\n",
    );
    test.add("lib", "ent.vhd", "entity ent is\nend entity;\n");
    test.add(
        "lib",
        "ent_a.vhd",
        "architecture a of ent is\nbegin\nend architecture;\n",
    );
    let top = test.add(
        "lib",
        "top.vhd",
        "\
entity top is
end entity;

architecture a of top is
   for inst : comp use configuration work.cfg;
begin
   inst : comp;
end architecture;
",
    );
    test.assert_compiles(cfg, top);
}

fn entity_with_two_architectures() -> TestProject {
    let mut test = TestProject::new();
    test.add_library("lib");
    test.add("lib", "ent.vhd", "entity ent is\nend entity;\n");
    test.add(
        "lib",
        "ent_a1.vhd",
        "architecture a1 of ent is\nbegin\nend architecture;\n",
    );
    test.add(
        "lib",
        "ent_a2.vhd",
        "architecture a2 of ent is\nbegin\nend architecture;\n",
    );
    test
}

#[test]
fn specific_architecture_reference_dependencies() {
    let mut test = entity_with_two_architectures();
    let ent_a1 = test
        .library("lib")
        .file(Utf8Path::new("ent_a1.vhd"))
        .unwrap();
    let ent_a2 = test
        .library("lib")
        .file(Utf8Path::new("ent_a2.vhd"))
        .unwrap();
    let top1 = test.add(
        "lib",
        "top1.vhd",
        "\
entity top1 is
end entity;

architecture a of top1 is
begin
  inst : entity work.ent(a1);
end architecture;
",
    );
    let top2 = test.add(
        "lib",
        "top2.vhd",
        "\
entity top2 is
end entity;

architecture a of top2 is
  for inst : comp use entity work.ent(a2);
begin
  inst : comp;
end architecture;
",
    );
    test.assert_compiles(ent_a1, top1);
    test.assert_compiles(ent_a2, top2);
    test.assert_not_compiles(ent_a2, top1);
}

#[test]
fn error_on_ambiguous_architecture() {
    let mut test = entity_with_two_architectures();
    let top = test.add(
        "lib",
        "top.vhd",
        "\
entity top is
end entity;

architecture a of top is
begin
  inst1 : entity work.ent;
  inst2 : entity work.ent;
end architecture;
",
    );
    // Reported once, although `ent` is instantiated twice.
    let analysis = test.project.dependency_graph(false);
    assert_eq!(analysis.diagnostics.len(), 1);
    let diagnostic = &analysis.diagnostics[0];
    assert_eq!(diagnostic.severity, Severity::Error);
    assert_eq!(diagnostic.file.as_deref(), Some(Utf8Path::new("top.vhd")));
    assert!(diagnostic.message.contains("lib.ent"));
    assert!(diagnostic.message.contains("a1 (ent_a1.vhd)"));

    // The ordering still works, and the compile order reports the error.
    let order = test.project.compile_order(Some(&[top]));
    assert_eq!(order.diagnostics.len(), 1);
    assert_eq!(order.files.unwrap().len(), 4);

    // Implementation dependencies include all architectures, which isn't ambiguous.
    assert!(test.project.dependency_graph(true).diagnostics.is_empty());
}

#[test]
fn work_library_reference_non_lower_case() {
    let mut test = TestProject::new();
    test.add_library("UPPER");
    test.add("UPPER", "ent.vhd", "entity ent is\nend entity;\n");
    let ent_a1 = test.add(
        "UPPER",
        "ent_a1.vhd",
        "architecture a1 of ent is\nbegin\nend architecture;\n",
    );
    let top1 = test.add(
        "UPPER",
        "top1.vhd",
        "\
entity top1 is
end entity;

architecture a of top1 is
begin
  inst : entity work.ent(a1);
end architecture;
",
    );
    test.assert_compiles(ent_a1, top1);
    assert_eq!(test.project.file(top1).dependencies[0].library, "UPPER");
}

#[test]
fn multiple_identical_file_names_with_different_path_in_same_library() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let a_foo = test.add("lib", "a/foo.vhd", "entity a_foo is\nend entity;\n");
    let b_foo = test.add("lib", "b/foo.vhd", "entity b_foo is\nend entity;\n");
    assert_ne!(a_foo, b_foo);
    assert_eq!(test.compile_order(), [a_foo, b_foo]);
    assert!(test.project.diagnostics().is_empty());
}

#[test]
fn duplicate_file_is_added_once() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let file1 = test.add("lib", "file.vhd", "entity foo is end entity;");
    let file2 = test.add("lib", "file.vhd", "entity foo is end entity;");
    assert_eq!(file1, file2);
    assert_eq!(test.project.files().len(), 1);
    assert!(test.project.diagnostics().is_empty());
}

fn check_warning_on_duplicate(setup: &[(&str, &str)], code: &str, expected: &str) {
    let mut test = TestProject::new();
    test.add_library("lib");
    for &(path, setup_code) in setup {
        test.add("lib", path, setup_code);
    }
    test.add("lib", "file.vhd", code);
    assert!(test.project.diagnostics().is_empty());
    test.add("lib", "file_copy.vhd", code);
    let messages = test.diagnostic_messages();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].starts_with("file_copy.vhd:"), "{}", messages[0]);
    assert!(messages[0].ends_with(expected), "{}", messages[0]);
}

#[test]
fn warning_on_duplicate_entity() {
    check_warning_on_duplicate(
        &[],
        "entity ent is\nend entity;\n",
        "warning: entity 'ent' previously defined in file.vhd",
    );
}

#[test]
fn warning_on_duplicate_package() {
    check_warning_on_duplicate(
        &[],
        "package pkg is\nend package;\n",
        "warning: package 'pkg' previously defined in file.vhd",
    );
}

#[test]
fn warning_on_duplicate_configuration() {
    check_warning_on_duplicate(
        &[],
        "configuration cfg of ent is\nend configuration;\n",
        "warning: configuration 'cfg' previously defined in file.vhd",
    );
}

#[test]
fn warning_on_duplicate_package_body() {
    check_warning_on_duplicate(
        &[("pkg.vhd", "package pkg is\nend package;\n")],
        "package body pkg is\nend package bodY;\n",
        "warning: package body 'pkg' previously defined in file.vhd",
    );
}

#[test]
fn warning_on_duplicate_architecture() {
    check_warning_on_duplicate(
        &[
            ("ent.vhd", "entity ent is\nend entity;\n"),
            (
                "arch.vhd",
                "architecture a_no_duplicate of ent is\nbegin\nend architecture;\n",
            ),
        ],
        "architecture a of ent is\nbegin\nend architecture;\n",
        "warning: architecture 'a' previously defined in file.vhd",
    );
}

#[test]
fn warning_on_duplicate_context() {
    check_warning_on_duplicate(
        &[],
        "context ctx is\nend context;\n",
        "warning: context 'ctx' previously defined in file.vhd",
    );
}

#[test]
fn finds_component_instantiation_dependencies() {
    let mut test = TestProject::new();
    test.add_library("toplib");
    let top = test.add(
        "toplib",
        "top.vhd",
        "\
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
             output => \"00\");
end architecture;
",
    );
    let comp1 = test.add("toplib", "comp1.vhd", "entity foo is\nend entity;\n");
    let comp2 = test.add(
        "toplib",
        "comp2.vhd",
        "entity foo2 is\nend entity;\n\narchitecture arch of foo2 is\nbegin\nend architecture;\n",
    );
    let comp1_arch = test.add(
        "toplib",
        "comp1_arch.vhd",
        "architecture arch of foo is\nbegin\nend architecture;\n",
    );
    assert_eq!(
        test.project.file(top).component_instantiations(),
        ["foo", "foo2"]
    );
    let order = test.compile_order_of(&[top]);
    assert!(order.contains(&comp1));
    assert!(order.contains(&comp1_arch));
    assert!(order.contains(&comp2));
    // Components aren't dependencies for ordering.
    test.assert_not_compiles(comp1, top);
}

#[test]
fn minimal_file_set_without_target() {
    let (test, files) = three_file_project();
    assert_eq!(test.compile_order(), files);
    let all: Vec<FileId> = test.project.files().map(|(id, _)| id).collect();
    assert_eq!(test.compile_order_of(&all), files);
}

#[test]
fn minimal_file_set_with_target() {
    let (test, [file1, file2, file3]) = three_file_project();
    assert_eq!(test.compile_order_of(&[file2]), [file1, file2]);
    // Indirect dependencies are included.
    assert_eq!(test.compile_order_of(&[file3]), [file1, file2, file3]);
}

#[test]
fn compile_order_keeps_insertion_order_for_independent_files() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let user = test.add("lib", "user.vhd", "use work.pkg.all;\nentity user is end;");
    let other = test.add("lib", "other.vhd", "entity other is end;");
    let pkg = test.add("lib", "pkg.vhd", "package pkg is end;");
    assert_eq!(test.compile_order(), [other, pkg, user]);
}

#[test]
fn compiles_same_file_into_different_libraries() {
    let mut test = TestProject::new();
    test.add_library("lib");
    let other_pkg = test.add(
        "lib",
        "other_pkg.vhd",
        "package other_pkg is\nend package other_pkg;\n",
    );
    let mut pkgs = Vec::new();
    let mut second_pkgs = Vec::new();
    for lib in ["lib1", "lib2"] {
        test.add_library(lib);
        pkgs.push(test.add(
            lib,
            "pkg.vhd",
            "library lib;\nuse lib.other_pkg.all;\n\npackage pkg is\nend package pkg;\n",
        ));
        second_pkgs.push(test.add(
            lib,
            &format!("{lib}_pkg.vhd"),
            "use work.pkg.all;\n\npackage second_pkg is\nend package second_pkg;\n",
        ));
    }
    assert_ne!(pkgs[0], pkgs[1]);
    assert_eq!(test.compile_order().len(), 5);
    test.assert_compiles(other_pkg, pkgs[0]);
    test.assert_compiles(other_pkg, pkgs[1]);
    test.assert_compiles(pkgs[0], second_pkgs[0]);
    test.assert_compiles(pkgs[1], second_pkgs[1]);
    test.assert_not_compiles(pkgs[0], second_pkgs[1]);
}

#[test]
fn circular_dependencies_cause_error() {
    let mut test = TestProject::new();
    test.add_library("lib");
    test.add(
        "lib",
        "ent1.vhd",
        "\
entity ent1 is
end ent1;

architecture arch of ent1 is
begin
   ent2_inst : entity work.ent2;
end architecture;
",
    );
    test.add(
        "lib",
        "ent2.vhd",
        "\
entity ent2 is
end ent2;

architecture arch of ent2 is
begin
   ent1_inst : entity work.ent1;
end architecture;
",
    );
    let order = test.project.compile_order(None);
    order.files.unwrap_err();
    let messages: Vec<_> = order.diagnostics.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        ["ent1.vhd: error: found circular dependency: ent1.vhd -> ent2.vhd -> ent1.vhd"]
    );
}

#[test]
fn order_of_adding_libraries_is_kept() {
    for order in [[0, 1, 2, 3], [3, 1, 0, 2], [2, 3, 1, 0]] {
        let mut test = TestProject::new();
        for index in order {
            test.add_library(&format!("lib{index}"));
        }
        let names: Vec<String> = test
            .project
            .libraries()
            .map(|(_, library)| library.name.clone())
            .collect();
        let expected: Vec<String> = order.iter().map(|index| format!("lib{index}")).collect();
        assert_eq!(names, expected);
    }
}

const BUFFERS: &str = "\
library ieee;
use ieee.std_logic_1164.all;

entity buffer1 is
  port (Q : out std_logic);
end entity;

architecture arch of buffer1 is begin
  Q <= '1';
end architecture;

library ieee;
use ieee.std_logic_1164.all;

entity buffer2 is
  port (Q : out std_logic);
end entity;

architecture arch of buffer2 is
  component buffer1
    port (Q : out std_logic);
  end component buffer1;

begin
  my_buffer_i : buffer1
    port map (Q => Q);
end architecture;
";

#[test]
fn circular_dependencies_through_libraries() {
    let mut test = TestProject::new();
    test.add_library("lib_1");
    test.add_library("lib_2");
    test.add_library("lib");
    test.add("lib_1", "file1.vhd", BUFFERS);
    test.add("lib_2", "file2.vhd", BUFFERS);
    let file3 = test.add(
        "lib",
        "file3.vhd",
        "\
library lib_1;

entity your_buffer is
end entity;

architecture arch of your_buffer is
begin
  my_buffer_i : entity lib_1.buffer1;
end architecture;
",
    );
    assert_eq!(test.compile_order_of(&[file3]).len(), 2);
}

#[test]
fn dependencies_on_multiple_libraries() {
    let mut test = TestProject::new();
    test.add_library("lib_1");
    test.add_library("lib_2");
    test.add_library("lib");
    test.add("lib_1", "file1.vhd", BUFFERS);
    let lib2_file1 = test.add("lib_2", "lib2/file1.vhd", BUFFERS);
    let file3 = test.add(
        "lib",
        "file3.vhd",
        "\
library ieee;use ieee.std_logic_1164.all;
library lib_1;entity your_buffer is port (D : in std_logic; Q : out std_logic);end entity;
architecture arch of your_buffer is
component buffer1 port (D : in  std_logic;Q : out std_logic);end component buffer1;
begin  my_buffer_i : buffer1 port map (D => D,Q => Q);end architecture;
",
    );
    assert!(!test.compile_order_of(&[file3]).contains(&lib2_file1));
}

#[test]
fn dependencies_on_separated_architecture() {
    let mut test = TestProject::new();
    test.add_library("lib");
    test.add(
        "lib",
        "file1.vhd",
        "\
library ieee;
use ieee.std_logic_1164.all;

entity buffer1 is
  port (D : in std_logic;
        Q : out std_logic);
end entity;
",
    );
    let file1_arch = test.add(
        "lib",
        "file1_arch.vhd",
        "\
library ieee;
use ieee.std_logic_1164.all;

architecture arch of buffer1 is
begin
  Q <= D;
end architecture;
",
    );
    let file3 = test.add(
        "lib",
        "file3.vhd",
        "\
library ieee;
use ieee.std_logic_1164.all;

entity your_buffer is
port (D : in std_logic; Q : out std_logic);
end entity;

architecture arch of your_buffer is
begin
my_buffer_i : entity work.buffer1
  port map (D => D,Q => Q);
end architecture;
",
    );
    assert!(test.compile_order_of(&[file3]).contains(&file1_arch));
}

#[test]
fn external_library_units_are_not_required() {
    let mut test = TestProject::new();
    test.project
        .add_library("ext", VhdlStandard::Vhdl2008, Some("/ext".into()))
        .unwrap();
    test.add_library("lib");
    let user = test.add(
        "lib",
        "user.vhd",
        "library ext;\nuse ext.pkg.all;\nentity user is end;",
    );
    assert!(test.library("ext").is_external());
    assert_eq!(test.compile_order_of(&[user]), [user]);
}

#[test]
fn file_standard_defaults_to_library_standard() {
    let mut test = TestProject::new();
    let library = test
        .project
        .add_library("lib", VhdlStandard::Vhdl1993, None)
        .unwrap();
    let default = test.add("lib", "a.vhd", "");
    let explicit = test.project.add_source_file(
        library,
        Utf8Path::new("b.vhd"),
        Some(VhdlStandard::Vhdl2019),
        ContentHash::of(b""),
        None,
    );
    assert_eq!(
        test.project.file(default).vhdl_standard,
        VhdlStandard::Vhdl1993
    );
    assert_eq!(
        test.project.file(explicit).vhdl_standard,
        VhdlStandard::Vhdl2019
    );
}
