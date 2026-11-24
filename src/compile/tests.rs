// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Tests of compile planning and state updates. Running compile processes is tested in
//! `tests/operations/` with a fake simulator.
//!
//! AI NOTICE: Generated, minimally reviewed.

use super::*;
use crate::builtins;
use crate::sources::SourceCache;
use crate::sources::build_project;
use crate::spec::CompilationUnit;
use crate::spec::Define;
use crate::spec::Encoding;
use crate::spec::IncludeDir;
use crate::spec::Options;
use crate::spec::ProjectSpec;
use crate::test_support::TempRoot;
use crate::test_support::add_sv;
use crate::test_support::add_sv_including;
use crate::test_support::add_vhdl;
use crate::test_support::simulator;

struct Fixture {
    temp: TempRoot,
    layout: OutputLayout,
    project: Project,
    simulator: Simulator,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        Self {
            temp,
            layout,
            project: Project::new(),
            simulator: simulator(),
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
        let path = self.temp.root.join(name);
        add_vhdl(&mut self.project, library, &path, code)
    }

    fn plan(&self, targets: &[FileId], state: &CompileState) -> CompilePlan {
        CompilePlan::new(
            &PlanInput {
                layout: &self.layout,
                project: &self.project,
                targets,
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
        fs::write(
            file.library_dir
                .join(format!("{}.hlib", file.library.to_ascii_lowercase())),
            "",
        )
        .unwrap();
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
    let command = plan.compile_command(&[3, 4]);
    assert_eq!(command[1], "analyze");
    assert_eq!(
        command[command.len() - 2..],
        [plan.files()[3].path.as_str(), tb_file.path.as_str()]
    );
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
    fixture.library("lib").library("user_lib").library("other");
    fixture.add("lib", "pkg.vhd", PKG);
    let user = fixture.add(
        "user_lib",
        "user.vhd",
        "library lib;\nuse lib.pkg.all;\npackage user is end package;",
    );
    let other = fixture.add("other", "other.vhd", "package other is end package;");
    let targets = [user, other];
    let plan = fixture.plan(&targets, &CompileState::default());
    let state = compiled_state(&plan);
    assert_eq!(fixture.plan(&targets, &state).compile_count(), 0);

    // The options of a library change its files' keys and those of their dependents, and only
    // the commands of its batches have their arguments.
    let lib = fixture.project.find_library("lib").unwrap();
    fixture.project.set_library_options(
        lib,
        Options {
            relaxed: true,
            ..Options::default()
        },
    );
    let relaxed_plan = fixture.plan(&targets, &state);
    let compiled: Vec<_> = relaxed_plan
        .files()
        .iter()
        .filter(|file| file.needs_compile)
        .map(|file| file.path.file_name().unwrap())
        .collect();
    assert_eq!(compiled, ["pkg.vhd", "user.vhd"]);
    let relaxed: Vec<_> = relaxed_plan
        .files()
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            let command = relaxed_plan.compile_command(&[*index]);
            assert!(!command.is_empty());
            command.iter().any(|arg| arg == "--relaxed")
        })
        .map(|(_, file)| file.path.file_name().unwrap())
        .collect();
    assert_eq!(relaxed, ["pkg.vhd"]);
    fixture.project.set_library_options(lib, Options::default());
    assert_eq!(fixture.plan(&targets, &state).compile_count(), 0);

    // The keys of SystemVerilog files don't reach the arguments of VHDL files; `encoding` does.
    let sv_options = Options {
        include_dirs: Some(vec![IncludeDir::new("/inc")]),
        defines: BTreeMap::from([("WIDTH".to_owned(), Define::text("8"))]),
        compilation_unit: Some(CompilationUnit::Library),
        keywords: Some("1800-2017".parse().unwrap()),
        default_timescale: Some("1ns/1ps".parse().unwrap()),
        sv_libraries: Some(vec!["other".to_owned()]),
        ..Options::default()
    };
    fixture.project.set_library_options(lib, sv_options.clone());
    assert_eq!(fixture.plan(&targets, &state).compile_count(), 0);
    fixture.project.set_library_options(
        lib,
        Options {
            encoding: Some(Encoding::Utf8),
            ..sv_options
        },
    );
    let encoded_plan = fixture.plan(&targets, &state);
    assert_eq!(encoded_plan.compile_count(), 2);
    let command = encoded_plan.compile_command(&[0]);
    assert!(command.iter().any(|arg| arg == "--encoding=utf-8"));
    assert!(!command.iter().any(|arg| arg.starts_with("--define")));
    fixture.project.set_library_options(lib, Options::default());

    let mut identity = fixture.simulator.identity().clone();
    identity.version_output = "risim-runner 0.1.0\nhydrolyze archive format 2\n".to_owned();
    fixture.simulator = Simulator::from_identity(identity).unwrap();
    assert_eq!(fixture.plan(&targets, &state).compile_count(), 3);
}

/// Every SystemVerilog and Verilog file is compiled, also one that no testbench needs, with the
/// arguments of its library, which its VHDL files share.
#[test]
fn sv_files_compile_with_their_library_arguments() {
    let mut fixture = Fixture::new();
    fixture.library("vunit_lib").library("pkgs").library("rtl");
    let vunit_pkg = fixture.temp.root.join("vunit_pkg.sv");
    add_sv(
        &mut fixture.project,
        "vunit_lib",
        &vunit_pkg,
        "package vunit_pkg;\nendpackage\n",
    );
    let pkg_path = fixture.temp.root.join("types_pkg.sv");
    add_sv(
        &mut fixture.project,
        "pkgs",
        &pkg_path,
        "package types_pkg;\nendpackage\n",
    );
    let unused_path = fixture.temp.root.join("unused.sv");
    add_sv(
        &mut fixture.project,
        "rtl",
        &unused_path,
        "module unused;\nendmodule\n",
    );
    let sv_path = fixture.temp.root.join("counter.sv");
    add_sv(
        &mut fixture.project,
        "rtl",
        &sv_path,
        "module counter;\n  import types_pkg::*;\nendmodule\n",
    );
    let top = fixture.add(
        "rtl",
        "top.vhd",
        "entity top is end;\narchitecture a of top is\n  component counter is end \
         component;\nbegin\n  u : counter;\nend;",
    );
    let rtl = fixture.project.find_library("rtl").unwrap();
    fixture.project.set_library_options(
        rtl,
        Options {
            relaxed: true,
            include_dirs: Some(vec![IncludeDir::new("/inc")]),
            ..Options::default()
        },
    );
    fixture.project.finish();

    let discovery = crate::discovery::discover(&fixture.project, &[]);
    let mut targets = targets(&fixture.project, &discovery);
    assert_eq!(targets.len(), 4);
    targets.push(top);
    let plan = fixture.plan(&targets, &CompileState::default());
    assert_eq!(plan.errors, []);
    assert_eq!(
        names(&plan),
        [
            "vunit_pkg.sv",
            "types_pkg.sv",
            "unused.sv",
            "counter.sv",
            "top.vhd"
        ]
    );
    for (index, file) in plan.files().iter().enumerate() {
        let command = plan.compile_command(&[index]);
        let sv_libraries: Vec<&str> = command
            .iter()
            .filter_map(|arg| arg.strip_prefix("--sv-library="))
            .collect();
        match file.library.as_str() {
            "rtl" => {
                assert!(command.iter().any(|arg| arg == "--include-dir=/inc"));
                assert_eq!(sv_libraries, ["pkgs", "vunit_lib"]);
            },
            "pkgs" => assert_eq!(sv_libraries, ["vunit_lib"]),
            _ => assert_eq!(sv_libraries, Vec::<&str>::new()),
        }
    }
    // A batch of VHDL and SystemVerilog files of one library is one call.
    let command = plan.compile_command(&[3, 4]);
    assert_eq!(
        command[command.len() - 2..],
        [sv_path.as_str(), plan.files()[4].path.as_str()]
    );
}

/// A library that `sv-libraries` names is left out of the `--sv-library` arguments while it has
/// no files in the plan, since it may have no archive: a VHDL library whose files no testbench
/// needs, and an empty one.
#[test]
fn sv_libraries_without_files_to_compile_are_left_out() {
    let mut fixture = Fixture::new();
    fixture
        .library("vunit_lib")
        .library("vhdl_lib")
        .library("empty_lib")
        .library("rtl");
    let vunit_pkg = fixture.temp.root.join("vunit_pkg.sv");
    add_sv(
        &mut fixture.project,
        "vunit_lib",
        &vunit_pkg,
        "package vunit_pkg;\nendpackage\n",
    );
    let pkg = fixture.add("vhdl_lib", "pkg.vhd", PKG);
    let module = fixture.temp.root.join("m.sv");
    add_sv(
        &mut fixture.project,
        "rtl",
        &module,
        "module m;\nendmodule\n",
    );
    let rtl = fixture.project.find_library("rtl").unwrap();
    fixture.project.set_library_options(
        rtl,
        Options {
            sv_libraries: Some(vec!["vhdl_lib".to_owned(), "empty_lib".to_owned()]),
            ..Options::default()
        },
    );
    fixture.project.finish();
    let sv_libraries = |args: &[String]| -> Vec<String> {
        args.iter()
            .filter_map(|arg| arg.strip_prefix("--sv-library="))
            .map(str::to_owned)
            .collect()
    };
    // The language server searches all of them.
    assert_eq!(
        sv_libraries(&fixture.project.analyze_args(rtl)),
        ["vhdl_lib", "empty_lib", "vunit_lib"]
    );

    let discovery = crate::discovery::discover(&fixture.project, &[]);
    let mut targets = targets(&fixture.project, &discovery);
    let plan = fixture.plan(&targets, &CompileState::default());
    assert_eq!(names(&plan), ["vunit_pkg.sv", "m.sv"]);
    assert_eq!(sv_libraries(&plan.compile_command(&[1])), ["vunit_lib"]);

    targets.push(pkg);
    let with_pkg = fixture.plan(&targets, &CompileState::default());
    assert_eq!(names(&with_pkg), ["vunit_pkg.sv", "pkg.vhd", "m.sv"]);
    assert_eq!(
        sv_libraries(&with_pkg.compile_command(&[2])),
        ["vhdl_lib", "vunit_lib"]
    );
    // `rtl` waits for `vhdl_lib`, whose archive it loads.
    assert_eq!(with_pkg.unit_dependencies[2], [0, 1]);
}

/// A library is scheduled after the libraries that its `--sv-library` arguments name, here
/// `vunit_lib`, also when none of its files depends on them.
#[test]
fn sv_libraries_are_scheduled_first() {
    let mut fixture = Fixture::new();
    fixture.library("vunit_lib").library("lib");
    let vunit_pkg = fixture.temp.root.join("vunit_pkg.sv");
    add_sv(
        &mut fixture.project,
        "vunit_lib",
        &vunit_pkg,
        "package vunit_pkg;\nendpackage\n",
    );
    let module = fixture.temp.root.join("m.sv");
    add_sv(
        &mut fixture.project,
        "lib",
        &module,
        "module m;\nendmodule\n",
    );
    fixture.project.finish();
    let discovery = crate::discovery::discover(&fixture.project, &[]);
    let plan = fixture.plan(
        &targets(&fixture.project, &discovery),
        &CompileState::default(),
    );
    assert_eq!(names(&plan), ["vunit_pkg.sv", "m.sv"]);
    // The files don't depend on each other, but `lib`'s unit waits for `vunit_lib`'s.
    assert!(plan.files().iter().all(|file| file.dependencies.is_empty()));
    assert_eq!(plan.units(), [vec![0], vec![1]]);
    assert_eq!(plan.unit_dependencies, [vec![], vec![0]]);
}

/// A library is scheduled after the libraries that the library clauses of its VHDL files name,
/// also when none of its files depends on theirs: here a component that a SystemVerilog module
/// binds to, which is only an implementation dependency.
#[test]
fn libraries_of_library_clauses_are_scheduled_first() {
    let mut fixture = Fixture::new();
    fixture.library("tb").library("rtl");
    let tb = fixture.add(
        "tb",
        "tb_counter.vhd",
        "library rtl, work;\n\
         entity tb_counter is generic (runner_cfg : string); end entity;\n\
         architecture tb of tb_counter is\n\
         component counter is end component;\n\
         begin\n  dut : component counter;\nend architecture;",
    );
    let counter = fixture.temp.root.join("counter.sv");
    add_sv(
        &mut fixture.project,
        "rtl",
        &counter,
        "module counter;\nendmodule\n",
    );
    fixture.project.finish();
    let discovery = crate::discovery::discover(&fixture.project, &[]);
    let targets = targets(&fixture.project, &discovery);
    assert!(targets.contains(&tb));
    let plan = fixture.plan(&targets, &CompileState::default());
    // The files don't depend on each other, and compile in the order they were added, but
    // `tb`'s unit waits for `rtl`'s.
    assert_eq!(names(&plan), ["tb_counter.vhd", "counter.sv"]);
    assert!(plan.files().iter().all(|file| file.dependencies.is_empty()));
    assert_eq!(plan.units(), [vec![0], vec![1]]);
    assert_eq!(plan.unit_dependencies, [vec![1], vec![]]);
}

/// The members of a `library`-mode library's group follow each other in collection order, share
/// one key and their dependencies, and form one batch.
#[test]
fn group_members_share_a_key_and_their_dependencies() {
    let mut fixture = Fixture::new();
    fixture.library("pkgs").library("rtl");
    let rtl = fixture.project.find_library("rtl").unwrap();
    fixture.project.set_library_options(
        rtl,
        Options {
            compilation_unit: Some(CompilationUnit::Library),
            ..Options::default()
        },
    );
    let sv = |into: &mut Fixture, library: &str, name: &str, code: &str| {
        let path = into.temp.root.join(name);
        add_sv(&mut into.project, library, &path, code)
    };
    sv(
        &mut fixture,
        "rtl",
        "b.sv",
        "module b;\n  import a_pkg::*;\nendmodule\n",
    );
    sv(&mut fixture, "rtl", "a.sv", "package a_pkg;\nendpackage\n");
    sv(&mut fixture, "pkgs", "p.sv", "package p;\nendpackage\n");
    sv(
        &mut fixture,
        "rtl",
        "c.sv",
        "module c;\n  import p::*;\nendmodule\n",
    );
    fixture.project.finish();
    let discovery = crate::discovery::discover(&fixture.project, &[]);
    let targets = targets(&fixture.project, &discovery);
    let plan = fixture.plan(&targets, &CompileState::default());
    assert_eq!(plan.errors, []);
    // `b.sv` imports a package of `a.sv`, but the group keeps the order of the collection.
    assert_eq!(names(&plan), ["p.sv", "b.sv", "a.sv", "c.sv"]);
    let members = &plan.files()[1..];
    assert!(members.iter().all(|file| file.group == Some(0)));
    assert_eq!(plan.files()[0].group, None);
    assert!(
        members
            .iter()
            .all(|file| file.compile_key == members[0].compile_key && file.dependencies == [0])
    );
    assert_eq!(plan.units(), [vec![0], vec![1, 2, 3]]);

    // A change of one member changes the key of all of them.
    let state = compiled_state(&plan);
    assert_eq!(fixture.plan(&targets, &state).compile_count(), 0);
    let mut changed = Fixture {
        project: Project::new(),
        ..fixture
    };
    changed.library("pkgs").library("rtl");
    changed.project.set_library_options(
        rtl,
        Options {
            compilation_unit: Some(CompilationUnit::Library),
            ..Options::default()
        },
    );
    sv(
        &mut changed,
        "rtl",
        "b.sv",
        "module b;\n  import a_pkg::*;\nendmodule\n",
    );
    sv(
        &mut changed,
        "rtl",
        "a.sv",
        "package a_pkg;\nendpackage\n// changed\n",
    );
    sv(&mut changed, "pkgs", "p.sv", "package p;\nendpackage\n");
    sv(
        &mut changed,
        "rtl",
        "c.sv",
        "module c;\n  import p::*;\nendmodule\n",
    );
    changed.project.finish();
    let changed_plan = changed.plan(&targets, &state);
    let recompiled: Vec<&str> = changed_plan
        .files()
        .iter()
        .filter(|file| file.needs_compile)
        .map(|file| file.path.file_name().unwrap())
        .collect();
    assert_eq!(recompiled, ["b.sv", "a.sv", "c.sv"]);
}

/// A compile key changes with the contents of an included file, with the creation of the file of
/// an include that found none, with a define, and with the order of the include directories.
#[test]
fn compile_keys_change_with_includes_and_sv_options() {
    let fixture = Fixture::new();
    let root = &fixture.temp.root;
    fixture.temp.write("inc_a/defs.svh", "`define A 1\n");
    fixture.temp.write("inc_b/defs.svh", "`define A 2\n");
    let code = "`include \"defs.svh\"\n`include \"missing.svh\"\nmodule m;\nendmodule\n";
    let path = fixture.temp.write("src/m.sv", code);
    let key_with = |include_dirs: &[&str], defines: &[(&str, &str)], time_scale: Option<&str>| {
        let mut project = Project::new();
        let library = project
            .add_library("lib", VhdlStandard::Vhdl2008, None)
            .unwrap();
        let include_dirs: Vec<Utf8PathBuf> =
            include_dirs.iter().map(|dir| root.join(dir)).collect();
        project.set_library_options(
            library,
            Options {
                include_dirs: Some(include_dirs.iter().map(IncludeDir::new).collect()),
                defines: defines
                    .iter()
                    .map(|&(name, text)| (name.to_owned(), Define::text(text)))
                    .collect(),
                default_timescale: time_scale.map(|time_scale| time_scale.parse().unwrap()),
                ..Options::default()
            },
        );
        let id = add_sv_including(&mut project, "lib", &path, code, &include_dirs);
        project.finish();
        let plan = CompilePlan::new(
            &PlanInput {
                layout: &fixture.layout,
                project: &project,
                targets: &[id],
                simulator: &fixture.simulator,
            },
            &CompileState::default(),
        );
        plan.files()[0].compile_key
    };
    let key =
        |include_dirs: &[&str], defines: &[(&str, &str)]| key_with(include_dirs, defines, None);
    let original = key(&["inc_a", "inc_b"], &[]);
    let dirs = ["inc_a", "inc_b"];
    let picoseconds = key_with(&dirs, &[], Some("1ns/1ps"));
    assert_ne!(picoseconds, original);
    assert_eq!(key_with(&dirs, &[], Some("1 ns / 1 ps")), picoseconds);
    assert_ne!(key_with(&dirs, &[], Some("1ns/10ps")), picoseconds);
    assert_eq!(key(&["inc_a", "inc_b"], &[]), original);
    assert_ne!(key(&["inc_b", "inc_a"], &[]), original);
    assert_ne!(key(&["inc_a", "inc_b"], &[("W", "8")]), original);
    fixture.temp.write("inc_a/defs.svh", "`define A 3\n");
    let edited = key(&["inc_a", "inc_b"], &[]);
    assert_ne!(edited, original);
    fixture.temp.write("inc_b/missing.svh", "");
    assert_ne!(key(&["inc_a", "inc_b"], &[]), edited);
}

/// A VHDL-only project gets no SystemVerilog arguments, and no `vunit_pkg.sv`.
#[test]
fn vhdl_only_projects_have_no_sv_arguments() {
    let workspace = TempRoot::new();
    workspace.write("src/tb.vhd", TB);
    workspace.write("src/ent.vhd", "entity ent is end entity;");
    let builtins_dir = builtins::extract(&workspace.root.join("risim-out/builtins")).unwrap();
    let mut spec = ProjectSpec::new();
    spec.options.include_dirs = Some(vec![IncludeDir::new("inc")]);
    spec.add_library("lib", ["src/*.vhd"]);
    let loaded = build_project(
        &workspace.root,
        &spec,
        &builtins_dir,
        &mut SourceCache::new(),
    );
    let project = &loaded.project;
    let discovery = crate::discovery::discover(project, &[]);
    let targets = targets(project, &discovery);
    let layout = OutputLayout::new(&workspace.root);
    let plan = CompilePlan::new(
        &PlanInput {
            layout: &layout,
            project,
            targets: &targets,
            simulator: &simulator(),
        },
        &CompileState::default(),
    );
    assert_eq!(plan.errors, []);
    assert!(!plan.files().is_empty());
    assert!(plan.files().iter().all(|file| file.kind == FileKind::Vhdl));
    for index in 0..plan.files().len() {
        let command = plan.compile_command(&[index]);
        assert!(
            !command
                .iter()
                .any(|arg| arg.starts_with("--sv-library") || arg.starts_with("--include-dir")),
            "{command:?}"
        );
    }
}

/// Libraries whose SystemVerilog dependencies form a cycle can't be compiled.
#[test]
fn sv_library_cycle_fails_the_plan() {
    let mut fixture = Fixture::new();
    fixture.library("a").library("b");
    let a_path = fixture.temp.root.join("a.sv");
    add_sv(
        &mut fixture.project,
        "a",
        &a_path,
        "package a_pkg;\n  import b_pkg::*;\nendpackage\n",
    );
    let b_path = fixture.temp.root.join("b.sv");
    add_sv(
        &mut fixture.project,
        "b",
        &b_path,
        "package b_pkg;\n  import a_pkg::*;\nendpackage\n",
    );
    fixture.project.finish();
    let discovery = crate::discovery::discover(&fixture.project, &[]);
    let plan = fixture.plan(
        &targets(&fixture.project, &discovery),
        &CompileState::default(),
    );
    assert_eq!(plan.errors, []);
    assert_eq!(plan.project_errors.len(), 1);
    assert!(plan.project_errors[0].message.contains("cycle"));
    assert!(plan.files().is_empty());
}

#[test]
fn deleted_library_archive_forces_compile_of_dependents() {
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

    // So does a missing archive in an existing directory, as a cancelled first compile leaves it.
    fs::create_dir_all(fixture.layout.library_dir("lib")).unwrap();
    assert_eq!(fixture.plan(&[user], &state).compile_count(), 2);
    fs::write(fixture.layout.library_archive("lib"), "").unwrap();
    assert_eq!(fixture.plan(&[user], &state).compile_count(), 0);
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
fn standards_other_than_2008_are_errors() {
    let mut fixture = Fixture::new();
    fixture
        .library("lib")
        .library_with_standard("old", VhdlStandard::Vhdl1993)
        .library_with_standard("new", VhdlStandard::Vhdl2019);
    fixture.add("old", "old.vhd", "package old_pkg is end package;");
    fixture.add("new", "new.vhd", "package new_pkg is end package;");
    let user = fixture.add(
        "lib",
        "user.vhd",
        "library old, new; use old.old_pkg.all; use new.new_pkg.all;\npackage user is end package;",
    );
    let plan = fixture.plan(&[user], &CompileState::default());
    let messages: Vec<&str> = plan
        .errors
        .iter()
        .map(|error| error.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "risim supports only VHDL-2008, not VHDL-93",
            "risim supports only VHDL-2008, not VHDL-2019",
        ]
    );
    assert!(plan.files().is_empty());
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
    assert_eq!(
        targets(&fixture.project, &discovery),
        [entity, architecture]
    );

    // VHDL configurations the test runs elaborate are targets too; nothing depends on them.
    let configuration = fixture.add(
        "lib",
        "cfg.vhd",
        "configuration Cfg1 of tb_split is for a end for; end configuration;",
    );
    fixture.add(
        "lib",
        "unused_cfg.vhd",
        "configuration cfg2 of tb_split is for a end for; end configuration;",
    );
    let configs = [crate::spec::TestConfigSpec {
        target: "lib.tb_split".to_owned(),
        vhdl_configuration_name: Some("CFG1".to_owned()),
        ..crate::spec::TestConfigSpec::default()
    }];
    let configured = crate::discovery::discover(&fixture.project, &configs);
    assert_eq!(
        targets(&fixture.project, &configured),
        [entity, architecture, configuration]
    );
}

#[test]
fn failure_output_is_capped() {
    let output_file = Utf8Path::new("/out/a.txt");
    let mut short = FailureOutput::default();
    assert_eq!(short.message(output_file), None);
    short.push("  first");
    short.push("second  ");
    assert_eq!(short.message(output_file).as_deref(), Some("first\nsecond"));

    let mut long = FailureOutput::default();
    for index in 0..1000 {
        long.push(&format!("line {index}"));
    }
    let message = long.message(output_file).unwrap();
    assert_eq!(message.lines().count(), FailureOutput::MAX_LINES + 1);
    assert!(message.ends_with("… (see /out/a.txt for the full output)"));

    let mut wide = FailureOutput::default();
    wide.push(&"x".repeat(FailureOutput::MAX_BYTES + 1));
    assert_eq!(
        wide.message(output_file).as_deref(),
        Some("… (see /out/a.txt for the full output)")
    );
}
