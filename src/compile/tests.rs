// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests of compile planning and state updates. Running compile processes is tested in
//! `tests/operations/` with a fake simulator.
//!
//! AI NOTICE: Generated, minimally reviewed.

use super::*;
use crate::vhdl_parser::VhdlDesignFile;

struct Fixture {
    _temp: tempfile::TempDir,
    root: Utf8PathBuf,
    layout: OutputLayout,
    project: Project,
    simulator: Simulator,
    options: CompileOptions,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(temp.path()).unwrap().to_owned();
        let simulator = Simulator::from_identity(SimulatorIdentity {
            path: "/bin/risim-ghdl".into(),
            size: 1,
            modified: None,
            version_output: "GHDL 6.4.0-risim [simulation adapter]".to_owned(),
        })
        .unwrap();
        Self {
            _temp: temp,
            root: root.clone(),
            layout: OutputLayout::new(&root),
            project: Project::new(),
            simulator,
            options: CompileOptions::default(),
        }
    }

    fn library(&mut self, name: &str) -> &mut Self {
        self.library_with_standard(name, VhdlStandard::Vhdl2008)
    }

    fn library_with_standard(&mut self, name: &str, standard: VhdlStandard) -> &mut Self {
        self.project.add_library(name, standard, None).unwrap();
        self
    }

    fn add(&mut self, library: &str, name: &str, code: &str) -> FileId {
        let library = self.project.find_library(library).unwrap();
        let design_file = VhdlDesignFile::parse(code.as_bytes()).ok().map(Arc::new);
        self.project.add_source_file(
            library,
            &self.root.join(name),
            None,
            ContentHash::of(code.as_bytes()),
            design_file,
        )
    }

    fn plan(&self, targets: &[FileId], state: &CompileState) -> CompilePlan {
        CompilePlan::new(
            &PlanInput {
                layout: &self.layout,
                project: &self.project,
                targets,
                compile_options: &self.options,
                simulator: &self.simulator,
            },
            state,
        )
    }
}

fn names(plan: &CompilePlan) -> Vec<String> {
    plan.files()
        .iter()
        .map(|file| file.path.file_name().unwrap().to_owned())
        .collect()
}

/// Records every file of `plan` as compiled.
fn compiled_state(plan: &CompilePlan) -> CompileState {
    let mut state = CompileState::default();
    for file in plan.files() {
        fs::create_dir_all(&file.library_dir).unwrap();
        state.files.insert(
            file.key.clone(),
            FileState {
                compile_key: file.compile_key,
                diagnostics: Vec::new(),
            },
        );
    }
    state
}

const PKG: &str = "package pkg is end package;";
const PKG_BODY: &str = "package body pkg is end package body;";
const ENT: &str = "use work.pkg.all;\nentity ent is end entity;";
const ENT_ARCH: &str = "architecture rtl of ent is begin end architecture;";
const TB: &str = "entity tb_x is generic (runner_cfg : string); end entity;\n\
                  architecture tb of tb_x is begin\n  dut: entity work.ent;\nend architecture;";

#[test]
fn compile_set_contains_targets_and_implementation_dependencies() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    fixture.add("lib", "unused.vhd", "entity unused is end entity;");
    fixture.add("lib", "pkg.vhd", PKG);
    fixture.add("lib", "pkg_body.vhd", PKG_BODY);
    fixture.add("lib", "ent.vhd", ENT);
    fixture.add("lib", "ent_arch.vhd", ENT_ARCH);
    let tb = fixture.add("lib", "tb.vhd", TB);

    let plan = fixture.plan(&[tb], &CompileState::default());
    assert_eq!(plan.errors, []);
    assert_eq!(
        names(&plan),
        [
            "pkg.vhd",
            "pkg_body.vhd",
            "ent.vhd",
            "ent_arch.vhd",
            "tb.vhd"
        ]
    );
    assert!(plan.files().iter().all(|file| file.needs_compile));
    assert_eq!(plan.compile_count(), 5);
    assert_eq!(plan.units(), [vec![0, 1, 2, 3, 4]]);

    let tb_file = &plan.files()[4];
    assert_eq!(tb_file.command[1], "-a");
    assert_eq!(tb_file.command.last().unwrap(), tb_file.path.as_str());
    assert!(tb_file.output_file.starts_with(fixture.layout.root()));
}

#[test]
fn compile_keys_change_with_dependencies() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    fixture.add("lib", "pkg.vhd", PKG);
    fixture.add("lib", "other.vhd", "entity other is end entity;");
    fixture.add("lib", "ent.vhd", ENT);
    let tb = fixture.add("lib", "tb.vhd", TB);
    let other_tb = fixture.add(
        "lib",
        "tb_other.vhd",
        "entity tb_other is generic (runner_cfg : string); end entity;\n\
         architecture a of tb_other is begin u: entity work.other; end architecture;",
    );
    let targets = [tb, other_tb];
    let plan = fixture.plan(&targets, &CompileState::default());
    let state = compiled_state(&plan);
    assert_eq!(fixture.plan(&targets, &state).compile_count(), 0);

    // Change the package: it and its users need compiling, unrelated files don't.
    let mut changed = Fixture {
        project: Project::new(),
        ..fixture
    };
    changed.library("lib");
    changed.add(
        "lib",
        "pkg.vhd",
        "package pkg is constant c : bit := '1'; end package;",
    );
    changed.add("lib", "other.vhd", "entity other is end entity;");
    changed.add("lib", "ent.vhd", ENT);
    changed.add("lib", "tb.vhd", TB);
    changed.add(
        "lib",
        "tb_other.vhd",
        "entity tb_other is generic (runner_cfg : string); end entity;\n\
         architecture a of tb_other is begin u: entity work.other; end architecture;",
    );
    let changed_plan = changed.plan(&targets, &state);
    let recompiled: Vec<&str> = changed_plan
        .files()
        .iter()
        .filter(|file| file.needs_compile)
        .map(|file| file.path.file_name().unwrap())
        .collect();
    assert_eq!(recompiled, ["pkg.vhd", "ent.vhd", "tb.vhd"]);
}

#[test]
fn compile_keys_depend_on_options_and_simulator() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    let pkg = fixture.add("lib", "pkg.vhd", PKG);
    let plan = fixture.plan(&[pkg], &CompileState::default());
    let state = compiled_state(&plan);
    assert_eq!(fixture.plan(&[pkg], &state).compile_count(), 0);

    fixture.options.a_flags = vec!["-frelaxed".to_owned()];
    assert_eq!(fixture.plan(&[pkg], &state).compile_count(), 1);
    fixture.options.a_flags.clear();

    let mut identity = fixture.simulator.identity().clone();
    identity.size = 2;
    fixture.simulator = Simulator::from_identity(identity).unwrap();
    assert_eq!(fixture.plan(&[pkg], &state).compile_count(), 1);
}

#[test]
fn deleted_library_directory_forces_compile_of_dependents() {
    let mut fixture = Fixture::new();
    fixture.library("lib").library("user_lib");
    fixture.add("lib", "pkg.vhd", PKG);
    let user = fixture.add(
        "user_lib",
        "user.vhd",
        "library lib;\nuse lib.pkg.all;\npackage user is end package;",
    );
    let plan = fixture.plan(&[user], &CompileState::default());
    let state = compiled_state(&plan);
    assert_eq!(fixture.plan(&[user], &state).compile_count(), 0);

    // The keys are unchanged, but recompiling the package makes its user obsolete.
    fs::remove_dir_all(fixture.layout.library_dir("lib")).unwrap();
    assert_eq!(fixture.plan(&[user], &state).compile_count(), 2);
}

#[test]
fn compile_invalidates_dependents_outside_the_compile_set() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    let pkg = fixture.add("lib", "pkg.vhd", PKG);
    let user = fixture.add(
        "lib",
        "user.vhd",
        "use work.pkg.all;\npackage user is end package;",
    );
    let all = fixture.plan(&[user], &CompileState::default());
    let mut state = compiled_state(&all);

    // Only the package is compiled, with an unchanged key.
    let plan = fixture.plan(&[pkg], &state);
    let results = vec![Some(FileResult {
        status: FileStatus::Compiled,
        diagnostics: Vec::new(),
    })];
    let report = finish(&plan, results, &mut state, Vec::new());
    assert_eq!(report.status, CompileStatus::Succeeded);
    let remaining: Vec<&str> = state.files.keys().map(FileKey::as_str).collect();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].ends_with("pkg.vhd"));
}

#[test]
fn independent_libraries_are_separate_units() {
    let mut fixture = Fixture::new();
    fixture
        .library("base")
        .library("a")
        .library("b")
        .library("top");
    fixture.add("base", "base.vhd", "package base_pkg is end package;");
    fixture.add(
        "a",
        "a.vhd",
        "library base; use base.base_pkg.all;\npackage a_pkg is end package;",
    );
    fixture.add(
        "b",
        "b.vhd",
        "library base; use base.base_pkg.all;\npackage b_pkg is end package;",
    );
    let top = fixture.add(
        "top",
        "top.vhd",
        "library a, b; use a.a_pkg.all; use b.b_pkg.all;\npackage top_pkg is end package;",
    );
    let plan = fixture.plan(&[top], &CompileState::default());
    assert_eq!(names(&plan), ["base.vhd", "a.vhd", "b.vhd", "top.vhd"]);
    assert_eq!(plan.units(), [vec![0], vec![1], vec![2], vec![3]]);
    assert_eq!(
        plan.unit_dependencies,
        [vec![], vec![0], vec![0], vec![1, 2]]
    );
}

#[test]
fn mutually_dependent_libraries_form_one_unit() {
    let mut fixture = Fixture::new();
    fixture.library("x").library("y");
    fixture.add("x", "x1.vhd", "package x1 is end package;");
    fixture.add(
        "y",
        "y1.vhd",
        "library x; use x.x1.all;\npackage y1 is end package;",
    );
    let top = fixture.add(
        "x",
        "x2.vhd",
        "library y; use y.y1.all;\npackage x2 is end package;",
    );
    let plan = fixture.plan(&[top], &CompileState::default());
    assert_eq!(names(&plan), ["x1.vhd", "y1.vhd", "x2.vhd"]);
    assert_eq!(plan.units(), [vec![0, 1, 2]]);
}

#[test]
fn cycle_is_an_error() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    fixture.add("lib", "a.vhd", "use work.b.all;\npackage a is end package;");
    let b_file = fixture.add("lib", "b.vhd", "use work.a.all;\npackage b is end package;");
    fixture.add("lib", "c.vhd", "package c is end package;");
    let plan = fixture.plan(&[b_file], &CompileState::default());
    assert_eq!(plan.errors.len(), 1);
    assert!(plan.errors[0].message.contains("circular dependency"));
    assert!(plan.files().is_empty());
}

#[test]
fn cycle_outside_the_compile_set_is_ignored() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    fixture.add("lib", "a.vhd", "use work.b.all;\npackage a is end package;");
    fixture.add("lib", "b.vhd", "use work.a.all;\npackage b is end package;");
    let c_file = fixture.add("lib", "c.vhd", "package c is end package;");
    let plan = fixture.plan(&[c_file], &CompileState::default());
    assert_eq!(plan.errors, []);
    assert_eq!(names(&plan), ["c.vhd"]);
}

#[test]
fn mixed_standards_are_an_error() {
    let mut fixture = Fixture::new();
    fixture
        .library("lib")
        .library_with_standard("old", VhdlStandard::Vhdl1993);
    fixture.add("old", "old.vhd", "package old_pkg is end package;");
    let user = fixture.add(
        "lib",
        "user.vhd",
        "library old; use old.old_pkg.all;\npackage user is end package;",
    );
    let plan = fixture.plan(&[user], &CompileState::default());
    assert_eq!(plan.errors.len(), 1);
    assert!(plan.errors[0].message.contains("mixed VHDL standards"));
}

#[test]
fn vhdl_2019_needs_a_recent_simulator() {
    let mut fixture = Fixture::new();
    let mut identity = fixture.simulator.identity().clone();
    identity.version_output = "GHDL 5.0.1 [simulation adapter]".to_owned();
    fixture.simulator = Simulator::from_identity(identity).unwrap();
    fixture.library_with_standard("lib", VhdlStandard::Vhdl2019);
    let pkg = fixture.add("lib", "pkg.vhd", PKG);
    let plan = fixture.plan(&[pkg], &CompileState::default());
    assert_eq!(plan.errors.len(), 1);
    assert!(plan.errors[0].message.contains("VHDL-2019"));
}

#[test]
fn failure_invalidates_dependents_outside_the_compile_set() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    fixture.add("lib", "pkg.vhd", PKG);
    let ent = fixture.add("lib", "ent.vhd", ENT);
    let user = fixture.add(
        "lib",
        "user.vhd",
        "use work.pkg.all;\npackage user is end package;",
    );
    let unrelated = fixture.add("lib", "unrelated.vhd", "package unrelated is end package;");
    let all = fixture.plan(&[ent, user, unrelated], &CompileState::default());
    let mut state = compiled_state(&all);
    assert_eq!(state.files.len(), 4);

    // Only `ent.vhd` and the package are in this compile set; the package fails.
    let plan = fixture.plan(&[ent], &state);
    let results = vec![
        Some(FileResult {
            status: FileStatus::Failed,
            diagnostics: vec![Diagnostic::error("broken")],
        }),
        Some(FileResult {
            status: FileStatus::Skipped,
            diagnostics: Vec::new(),
        }),
    ];
    let report = finish(&plan, results, &mut state, Vec::new());
    assert_eq!(report.status, CompileStatus::Failed);
    let remaining: Vec<&str> = state.files.keys().map(FileKey::as_str).collect();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].ends_with("unrelated.vhd"));
    assert_eq!(report.diagnostics.len(), 1);
}

#[test]
fn targets_are_testbench_files() {
    let mut fixture = Fixture::new();
    fixture.library("lib");
    let entity = fixture.add(
        "lib",
        "tb_ent.vhd",
        "entity tb_split is generic (runner_cfg : string); end entity;",
    );
    let architecture = fixture.add(
        "lib",
        "tb_arch.vhd",
        "architecture a of tb_split is begin end architecture;",
    );
    fixture.add("lib", "other.vhd", PKG);
    let discovery = crate::discovery::discover(&fixture.project, &[]);
    assert_eq!(targets(&discovery), [entity, architecture]);
}
