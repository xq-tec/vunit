// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Libraries, source files and design units, and the dependencies between files.
//!
//! A port of `project.py`, `library.py`, `source_file.py` and `design_unit.py`. Differences
//! from VUnit:
//!
//! - An ambiguous direct entity instantiation is reported as a diagnostic instead of aborting.
//! - Missing libraries, units and architectures are only logged, as in VUnit.
//! - A component instantiation matches entities with the component's name in all libraries
//!   that the file's context clauses make visible, not only in the file's own library. Other
//!   primary units with that name don't match, apart from SystemVerilog modules, which a
//!   component binds to too. See `Project::component_dependencies`.
//! - Recompilation isn't decided here (see the `compile` module).
//!
//! SystemVerilog and Verilog files register their definitions (modules, interfaces, programs,
//! and primitives) and packages in name spaces of their own, which are case-sensitive
//! (IEEE 1800 3.13), so an entity and a module of the same name coexist.
//! Each library with such files searches other libraries for the packages it imports and the
//! interfaces it uses: those of its `sv-libraries` option, or else those that
//! [`Project::finish`] derives from the scans, and always `vunit_lib` last
//! ([`Library::sv_dependencies`]).
//! Their analysis edges go to imported and referenced packages and used interfaces, searched as
//! hydrolyze searches them; instances of other definitions and of VHDL entities are
//! implementation edges only.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fmt;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use thiserror::Error;

use crate::builtins;
use crate::dependency_graph::CircularDependency;
use crate::dependency_graph::DependencyGraph;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::Range;
use crate::sources::ContentHash;
use crate::sources::FileKind;
use crate::spec::Options;
use crate::verilog_parser::SvDefinitionKind;
use crate::verilog_parser::SvDesignFile;
use crate::vhdl_parser::ReferenceType;
use crate::vhdl_parser::VhdlComponentInstantiation;
use crate::vhdl_parser::VhdlDesignFile;
use crate::vhdl_parser::VhdlReference;
use crate::vhdl_standard::VhdlStandard;

#[cfg(test)]
mod tests;

/// Identifies a library of a [`Project`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LibraryId(u32);

/// Identifies a source file of a [`Project`].
///
/// A file that is part of two libraries has two IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

impl LibraryId {
    const fn index(self) -> usize {
        self.0 as usize
    }
}

impl FileId {
    const fn index(self) -> usize {
        self.0 as usize
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "a project with more than u32::MAX entries can't be built in memory anyway"
)]
fn id_from_index(index: usize) -> u32 {
    u32::try_from(index).unwrap()
}

/// The kind of a design unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKind {
    /// An entity declaration.
    Entity,
    /// A package declaration or instantiation.
    Package,
    /// A context declaration.
    Context,
    /// A configuration declaration.
    Configuration,
    /// An architecture body.
    Architecture,
    /// A package body.
    PackageBody,
}

impl UnitKind {
    /// Whether this is a primary unit, which is visible in its library by name.
    pub const fn is_primary(self) -> bool {
        match self {
            Self::Entity | Self::Package | Self::Context | Self::Configuration => true,
            Self::Architecture | Self::PackageBody => false,
        }
    }
}

impl fmt::Display for UnitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Entity => "entity",
            Self::Package => "package",
            Self::Context => "context",
            Self::Configuration => "configuration",
            Self::Architecture => "architecture",
            Self::PackageBody => "package body",
        })
    }
}

/// A design unit in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignUnit {
    /// The lowercase unit name; for a package body, the package name.
    pub name: String,
    /// The kind of unit.
    pub kind: UnitKind,
    /// For secondary units, the lowercase name of the primary unit.
    pub primary_unit: Option<String>,
    /// The location of the name.
    pub range: Range,
}

/// A source file in a library: a VHDL, SystemVerilog, or Verilog file.
///
/// The VHDL fields stay empty for a SystemVerilog or Verilog file.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The absolute path.
    pub path: Utf8PathBuf,
    /// The library the file is compiled into.
    pub library: LibraryId,
    /// The kind of file: [`FileKind::Vhdl`], [`FileKind::SystemVerilog`], or
    /// [`FileKind::Verilog`].
    pub kind: FileKind,
    /// The standard the file is compiled with, if it is a VHDL file.
    pub vhdl_standard: VhdlStandard,
    /// The hash of the raw file contents.
    pub content_hash: ContentHash,
    /// The parse result of a VHDL file; `None` if the file couldn't be parsed.
    pub design_file: Option<Arc<VhdlDesignFile>>,
    /// Design units of a VHDL file, in the order VUnit registers them.
    pub design_units: Vec<DesignUnit>,
    /// References of a VHDL file to other units, with `work` replaced by the file's library.
    pub dependencies: Vec<VhdlReference>,
    /// The scan of a SystemVerilog or Verilog file.
    pub sv_file: Option<Arc<SvDesignFile>>,
    /// The hashes of the contents of the files that the scan's
    /// [`includes`](SvDesignFile::includes) found, in the same order; `None` for an include that
    /// wasn't found.
    pub include_hashes: Vec<Option<ContentHash>>,
}

impl SourceFile {
    /// The component instantiations, in source order.
    pub fn component_instantiations(&self) -> &[VhdlComponentInstantiation] {
        self.design_file
            .as_ref()
            .map_or(&[], |design_file| &design_file.component_instantiations)
    }

    /// The libraries that the library clauses of a VHDL file name, lowercase, in source order.
    pub fn library_clauses(&self) -> &[String] {
        self.design_file
            .as_ref()
            .map_or(&[], |design_file| &design_file.library_clauses)
    }
}

/// Where a primary unit is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimaryUnit {
    /// The kind of unit.
    pub kind: UnitKind,
    /// The file defining it.
    pub file: FileId,
}

/// A library of a [`Project`].
#[derive(Debug, Clone)]
pub struct Library {
    /// The name, in the case it was defined with.
    pub name: String,
    /// The default standard of the library's files.
    pub vhdl_standard: VhdlStandard,
    /// For a precompiled library, its directory.
    pub external_path: Option<Utf8PathBuf>,
    /// The effective options of the library's files, which give their `risim analyze`
    /// arguments ([`Project::analyze_args`]); the default unless
    /// [`Project::set_library_options`] sets them.
    pub options: Options,
    files: FxHashMap<Utf8PathBuf, FileId>,
    primary_units: FxHashMap<String, PrimaryUnit>,
    /// Architectures per entity name, in the order they were added.
    architectures: FxHashMap<String, Vec<(String, FileId)>>,
    package_bodies: FxHashMap<String, FileId>,
    /// Whether the library has SystemVerilog or Verilog files.
    has_sv: bool,
    /// The SystemVerilog definitions by name.
    sv_definitions: FxHashMap<String, (SvDefinitionKind, FileId)>,
    /// The names of [`sv_definitions`](Self::sv_definitions) by their lowercase form.
    sv_definition_names: FxHashMap<String, Vec<String>>,
    /// The SystemVerilog packages by name.
    sv_packages: FxHashMap<String, FileId>,
    /// The libraries that the SystemVerilog files search after this one, with the first use
    /// that added each; set by [`Project::finish`].
    sv_dependencies: Vec<(LibraryId, SvDependencyUse)>,
}

/// Why a library is one of the [SystemVerilog dependencies](Library::sv_dependencies) of
/// another: the first use that added it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SvDependencyUse {
    /// The library's `sv-libraries` option names it.
    Listed,
    /// `import package::…` in a file.
    Import { file: FileId, package: String },
    /// `package::name` in a file.
    PackageReference { file: FileId, package: String },
    /// The interface `interface` used in a file, as an instance or a port.
    Interface { file: FileId, interface: String },
    /// `vunit_lib`, which every library with SystemVerilog or Verilog files searches.
    VunitLib,
}

impl Library {
    /// Whether the library is precompiled.
    pub const fn is_external(&self) -> bool {
        self.external_path.is_some()
    }

    /// The primary unit `name` (lowercase).
    pub fn primary_unit(&self, name: &str) -> Option<PrimaryUnit> {
        self.primary_units.get(name).copied()
    }

    /// The architectures of the entity `name` (lowercase), in the order they were added.
    ///
    /// Empty if `name` isn't an entity.
    pub fn architectures(&self, name: &str) -> &[(String, FileId)] {
        match self.primary_units.get(name) {
            Some(unit) if unit.kind == UnitKind::Entity => {
                self.architectures.get(name).map_or(&[], Vec::as_slice)
            },
            _ => &[],
        }
    }

    /// The file with the body of package `name` (lowercase), if it has one.
    pub fn package_body(&self, name: &str) -> Option<FileId> {
        self.package_bodies.get(name).copied()
    }

    /// The ID of the file at `path` in this library.
    pub fn file(&self, path: &Utf8Path) -> Option<FileId> {
        self.files.get(path).copied()
    }

    /// Whether the library has SystemVerilog or Verilog files.
    pub const fn has_sv(&self) -> bool {
        self.has_sv
    }

    /// The SystemVerilog definition (module, interface, program, or primitive) `name`, which is
    /// case-sensitive, with the file that defines it.
    pub fn sv_definition(&self, name: &str) -> Option<(SvDefinitionKind, FileId)> {
        self.sv_definitions.get(name).copied()
    }

    /// The file with the SystemVerilog package `name`, which is case-sensitive.
    pub fn sv_package(&self, name: &str) -> Option<FileId> {
        self.sv_packages.get(name).copied()
    }

    /// The libraries that the library's SystemVerilog files search for packages and
    /// definitions after the library itself, in order, as `risim analyze` gets them
    /// (`--sv-library`).
    ///
    /// They are those of the `sv-libraries` option as written, or else those that the imports,
    /// package references, and interface uses of the files find in other libraries, in the
    /// order of their first use.
    /// In a project with SystemVerilog or Verilog files, `vunit_lib` comes last, also when the
    /// files use its packages, unless the option names it elsewhere; it isn't repeated.
    /// Empty for a library without SystemVerilog or Verilog files, and until
    /// [`Project::finish`].
    pub fn sv_dependencies(&self) -> impl ExactSizeIterator<Item = LibraryId> + '_ {
        self.sv_dependencies.iter().map(|&(library, _)| library)
    }

    /// The files of the SystemVerilog modules whose names equal `name` case-insensitively.
    fn sv_modules_matching(&self, name: &str) -> impl Iterator<Item = FileId> + '_ {
        self.sv_definition_names
            .get(&name.to_ascii_lowercase())
            .into_iter()
            .flatten()
            .filter_map(|name| match self.sv_definitions.get(name) {
                Some(&(SvDefinitionKind::Module | SvDefinitionKind::Macromodule, file)) => {
                    Some(file)
                },
                _ => None,
            })
    }
}

/// A library can't be added.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LibraryError {
    /// `work` refers to the current library.
    #[error("a library can't be named 'work', which refers to the current library")]
    Work,
    /// Library names are case-insensitive.
    #[error(
        "library name '{name}' isn't unique: library names are case-insensitive, and '{existing}' \
         is already defined"
    )]
    Duplicate {
        /// The rejected name.
        name: String,
        /// The name of the existing library.
        existing: String,
    },
}

/// Checks `name` as the name of a new library and returns it in lowercase.
///
/// `existing` returns the name of the library with the given lowercase name, if there is one.
fn check_library_name(
    name: &str,
    existing: impl FnOnce(&str) -> Option<String>,
) -> Result<String, LibraryError> {
    let lowercase = name.to_ascii_lowercase();
    if lowercase == "work" {
        return Err(LibraryError::Work);
    }
    if let Some(existing) = existing(&lowercase) {
        return Err(LibraryError::Duplicate {
            name: name.to_owned(),
            existing,
        });
    }
    Ok(lowercase)
}

/// The names of the libraries defined so far, for checking the names of new ones.
#[derive(Debug, Clone, Default)]
pub struct LibraryNames {
    /// The names by their lowercase form.
    by_lowercase: FxHashMap<String, String>,
}

impl LibraryNames {
    /// Creates an empty set of names.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `name`.
    ///
    /// # Errors
    ///
    /// Fails like [`Project::add_library`]; the name isn't added then.
    pub fn insert(&mut self, name: &str) -> Result<(), LibraryError> {
        let lowercase =
            check_library_name(name, |lowercase| self.by_lowercase.get(lowercase).cloned())?;
        self.by_lowercase.insert(lowercase, name.to_owned());
        Ok(())
    }

    /// Returns `true` if `name` is one of the names, compared case-insensitively.
    pub fn contains(&self, name: &str) -> bool {
        self.by_lowercase.contains_key(&name.to_ascii_lowercase())
    }
}

/// The libraries and source files of a project.
#[derive(Debug, Clone, Default)]
pub struct Project {
    libraries: Vec<Library>,
    libraries_by_lowercase_name: FxHashMap<String, LibraryId>,
    files: Vec<SourceFile>,
    diagnostics: Vec<Diagnostic>,
    /// The errors for cycles between SystemVerilog dependencies, among `diagnostics`.
    sv_cycles: Vec<Diagnostic>,
}

/// A dependency graph and the problems found while building it.
#[derive(Debug, Clone)]
pub struct DependencyAnalysis {
    /// Edges point from dependencies to dependent files.
    pub graph: DependencyGraph<FileId>,
    /// Errors for ambiguous entity instantiations.
    pub diagnostics: Vec<Diagnostic>,
}

/// Files in compile order, and the problems found while ordering them.
#[derive(Debug, Clone)]
pub struct CompileOrder {
    /// The files in compile order, or the cycle that prevents ordering them.
    pub files: Result<Vec<FileId>, CircularDependency<FileId>>,
    /// Errors for ambiguous entity instantiations and dependency cycles.
    pub diagnostics: Vec<Diagnostic>,
}

/// Libraries that are never reported as missing.
const BUILTIN_LIBRARIES: [&str; 2] = ["ieee", "std"];

impl Project {
    /// Creates an empty project.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a library.
    ///
    /// # Errors
    ///
    /// Fails if the name is `work` or matches an existing library case-insensitively.
    pub fn add_library(
        &mut self,
        name: &str,
        vhdl_standard: VhdlStandard,
        external_path: Option<Utf8PathBuf>,
    ) -> Result<LibraryId, LibraryError> {
        let lowercase = check_library_name(name, |lowercase| {
            self.libraries_by_lowercase_name
                .get(lowercase)
                .map(|existing| self.libraries[existing.index()].name.clone())
        })?;
        let id = LibraryId(id_from_index(self.libraries.len()));
        self.libraries.push(Library {
            name: name.to_owned(),
            vhdl_standard,
            external_path,
            options: Options::default(),
            files: FxHashMap::default(),
            primary_units: FxHashMap::default(),
            architectures: FxHashMap::default(),
            package_bodies: FxHashMap::default(),
            has_sv: false,
            sv_definitions: FxHashMap::default(),
            sv_definition_names: FxHashMap::default(),
            sv_packages: FxHashMap::default(),
            sv_dependencies: Vec::new(),
        });
        self.libraries_by_lowercase_name.insert(lowercase, id);
        Ok(id)
    }

    /// Sets the effective options of `library`'s files.
    pub fn set_library_options(&mut self, library: LibraryId, options: Options) {
        self.libraries[library.index()].options = options;
    }

    /// Adds a VHDL file to `library` and registers its design units.
    ///
    /// `design_file` is `None` if the file couldn't be parsed. Adding the same path to the
    /// same library again returns the existing file.
    pub fn add_source_file(
        &mut self,
        library: LibraryId,
        path: &Utf8Path,
        vhdl_standard: Option<VhdlStandard>,
        content_hash: ContentHash,
        design_file: Option<Arc<VhdlDesignFile>>,
    ) -> FileId {
        if let Some(existing) = self.libraries[library.index()].file(path) {
            return existing;
        }
        let id = FileId(id_from_index(self.files.len()));
        let library_name = self.libraries[library.index()].name.clone();
        let design_units = design_file
            .as_deref()
            .map(design_units_of)
            .unwrap_or_default();
        let dependencies = design_file
            .as_deref()
            .map(|design_file| dependencies_of(design_file, &library_name))
            .unwrap_or_default();
        let file = SourceFile {
            path: path.to_owned(),
            library,
            kind: FileKind::Vhdl,
            vhdl_standard: vhdl_standard
                .unwrap_or_else(|| self.libraries[library.index()].vhdl_standard),
            content_hash,
            design_file,
            design_units,
            dependencies,
            sv_file: None,
            include_hashes: Vec::new(),
        };
        self.register_design_units(id, &file);
        self.libraries[library.index()]
            .files
            .insert(path.to_owned(), id);
        self.files.push(file);
        id
    }

    /// Adds a SystemVerilog or Verilog file of `kind` to `library` and registers its
    /// definitions and packages.
    ///
    /// `sv_file` is the scan of the file, `None` if it couldn't be scanned, and `include_hashes`
    /// are the hashes of the files its includes found ([`SourceFile::include_hashes`]). Adding the
    /// same path to the same library again returns the existing file.
    pub fn add_sv_file(
        &mut self,
        library: LibraryId,
        path: &Utf8Path,
        kind: FileKind,
        content_hash: ContentHash,
        sv_file: Option<Arc<SvDesignFile>>,
        include_hashes: Vec<Option<ContentHash>>,
    ) -> FileId {
        if let Some(existing) = self.libraries[library.index()].file(path) {
            return existing;
        }
        let id = FileId(id_from_index(self.files.len()));
        let file = SourceFile {
            path: path.to_owned(),
            library,
            kind,
            vhdl_standard: self.libraries[library.index()].vhdl_standard,
            content_hash,
            design_file: None,
            design_units: Vec::new(),
            dependencies: Vec::new(),
            sv_file,
            include_hashes,
        };
        self.register_sv_names(id, &file);
        let library = &mut self.libraries[library.index()];
        library.has_sv = true;
        library.files.insert(path.to_owned(), id);
        self.files.push(file);
        id
    }

    /// Registers the definitions and packages of an SV file, and warns about duplicates in
    /// each name space, like [`Self::register_design_units`].
    fn register_sv_names(&mut self, id: FileId, file: &SourceFile) {
        let Some(sv_file) = &file.sv_file else {
            return;
        };
        let library = &mut self.libraries[file.library.index()];
        let mut duplicates = Vec::new();
        for definition in &sv_file.definitions {
            let previous = library
                .sv_definitions
                .insert(definition.name.clone(), (definition.kind, id));
            let names = library
                .sv_definition_names
                .entry(definition.name.to_ascii_lowercase())
                .or_default();
            if !names.contains(&definition.name) {
                names.push(definition.name.clone());
            }
            if let Some((_, previous)) = previous {
                duplicates.push((
                    definition.kind.to_string(),
                    &definition.name,
                    definition.name_location,
                    previous,
                ));
            }
        }
        for package in &sv_file.packages {
            if let Some(previous) = library.sv_packages.insert(package.name.clone(), id) {
                duplicates.push((
                    "package".to_owned(),
                    &package.name,
                    package.name_location,
                    previous,
                ));
            }
        }
        for (kind, name, location, previous) in duplicates {
            let previous_path = if previous == id {
                &file.path
            } else {
                &self.files[previous.index()].path
            };
            self.diagnostics.push(
                Diagnostic::warning(format!(
                    "{kind} '{name}' previously defined in {previous_path}"
                ))
                .in_file(&file.path)
                .at(location),
            );
        }
    }

    /// Completes the project once all files are added: derives the SystemVerilog dependencies
    /// of the libraries ([`Library::sv_dependencies`]), and reports an error for each cycle
    /// between them.
    ///
    /// Call it once.
    pub fn finish(&mut self) {
        let has_sv = self.libraries.iter().any(|library| library.has_sv);
        let vunit_lib = self.find_library(builtins::VUNIT_LIB);
        for index in 0..self.libraries.len() {
            let id = LibraryId(id_from_index(index));
            let dependencies = if self.libraries[index].has_sv {
                self.sv_library_dependencies(id, vunit_lib.filter(|_| has_sv))
            } else {
                Vec::new()
            };
            self.libraries[index].sv_dependencies = dependencies;
        }
        for cycle in self.sv_library_cycles() {
            let diagnostic = self.sv_library_cycle(&cycle);
            self.sv_cycles.push(diagnostic.clone());
            self.diagnostics.push(diagnostic);
        }
    }

    /// The SystemVerilog dependencies of `library`, with their first use, as
    /// [`Library::sv_dependencies`] describes them; `vunit_lib` is the library to add last.
    ///
    /// The derivation doesn't search `vunit_lib`, which comes last anyway, so that a name is
    /// attributed to the library that the final search order finds it in.
    fn sv_library_dependencies(
        &self,
        library: LibraryId,
        vunit_lib: Option<LibraryId>,
    ) -> Vec<(LibraryId, SvDependencyUse)> {
        let mut dependencies: Vec<(LibraryId, SvDependencyUse)> = Vec::new();
        let mut add = |dependency: LibraryId, first_use: SvDependencyUse| {
            if dependency != library && !dependencies.iter().any(|&(other, _)| other == dependency)
            {
                dependencies.push((dependency, first_use));
            }
        };
        let own = self.library(library);
        if let Some(listed) = &own.options.sv_libraries {
            // Unknown names were reported with the configuration.
            for dependency in listed.iter().filter_map(|name| self.find_library(name)) {
                add(dependency, SvDependencyUse::Listed);
            }
        } else {
            let package_library = |package: &str| {
                self.libraries()
                    .find(|&(other, candidate)| {
                        other != library
                            && Some(other) != vunit_lib
                            && candidate.sv_packages.contains_key(package)
                    })
                    .map(|(other, _)| other)
            };
            let interface_library = |name: &str| {
                self.libraries()
                    .find(|&(other, candidate)| {
                        other != library
                            && Some(other) != vunit_lib
                            && matches!(
                                candidate.sv_definitions.get(name),
                                Some((SvDefinitionKind::Interface, _))
                            )
                    })
                    .map(|(other, _)| other)
            };
            for (file_id, file) in self.files() {
                let Some(sv_file) = file.sv_file.as_deref().filter(|_| file.library == library)
                else {
                    continue;
                };
                for package in &sv_file.imports {
                    if !own.sv_packages.contains_key(package)
                        && let Some(dependency) = package_library(package)
                    {
                        let package = package.clone();
                        add(
                            dependency,
                            SvDependencyUse::Import {
                                file: file_id,
                                package,
                            },
                        );
                    }
                }
                for package in &sv_file.package_references {
                    if !own.sv_packages.contains_key(package)
                        && let Some(dependency) = package_library(package)
                    {
                        let package = package.clone();
                        add(
                            dependency,
                            SvDependencyUse::PackageReference {
                                file: file_id,
                                package,
                            },
                        );
                    }
                }
                for name in &sv_file.instances {
                    if !own.sv_definitions.contains_key(name)
                        && let Some(dependency) = interface_library(name)
                    {
                        add(
                            dependency,
                            SvDependencyUse::Interface {
                                file: file_id,
                                interface: name.clone(),
                            },
                        );
                    }
                }
            }
        }
        if let Some(vunit_lib) = vunit_lib {
            add(vunit_lib, SvDependencyUse::VunitLib);
        }
        dependencies
    }

    /// The cycles between the SystemVerilog dependencies of the libraries, each as the libraries
    /// along it, starting with the first one that the search reached.
    fn sv_library_cycles(&self) -> Vec<Vec<LibraryId>> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum State {
            New,
            OnPath,
            Done,
        }
        fn visit(
            project: &Project,
            library: LibraryId,
            states: &mut [State],
            path: &mut Vec<LibraryId>,
            cycles: &mut Vec<Vec<LibraryId>>,
        ) {
            states[library.index()] = State::OnPath;
            path.push(library);
            for dependency in project.library(library).sv_dependencies() {
                match states[dependency.index()] {
                    State::New => visit(project, dependency, states, path, cycles),
                    State::OnPath => {
                        if let Some(start) = path.iter().position(|&other| other == dependency) {
                            cycles.push(path[start..].to_vec());
                        }
                    },
                    State::Done => {},
                }
            }
            path.pop();
            states[library.index()] = State::Done;
        }
        let mut states = vec![State::New; self.libraries.len()];
        let mut cycles = Vec::new();
        for (library, _) in self.libraries() {
            if states[library.index()] == State::New {
                visit(self, library, &mut states, &mut Vec::new(), &mut cycles);
            }
        }
        cycles
    }

    /// The error for the cycle `cycle` of [`Self::sv_library_cycles`]: the libraries, and for
    /// each edge the use or the `sv-libraries` list that formed it.
    fn sv_library_cycle(&self, cycle: &[LibraryId]) -> Diagnostic {
        let mut edges = Vec::new();
        let mut first_file = None;
        for (index, &library) in cycle.iter().enumerate() {
            let next = cycle[(index + 1) % cycle.len()];
            let name = &self.library(library).name;
            let next_name = &self.library(next).name;
            let first_use = self
                .library(library)
                .sv_dependencies
                .iter()
                .find(|&&(dependency, _)| dependency == next)
                .map(|(_, first_use)| first_use);
            let file_path = |file: &FileId| self.file(*file).path.as_str();
            let reason = match first_use {
                Some(SvDependencyUse::Listed) | None => {
                    format!("its 'sv-libraries' list names '{next_name}'")
                },
                Some(SvDependencyUse::Import { file, package }) => {
                    first_file.get_or_insert(*file);
                    format!(
                        "{} imports package '{package}' of '{next_name}'",
                        file_path(file)
                    )
                },
                Some(SvDependencyUse::PackageReference { file, package }) => {
                    first_file.get_or_insert(*file);
                    format!(
                        "{} refers to package '{package}' of '{next_name}'",
                        file_path(file)
                    )
                },
                Some(SvDependencyUse::Interface { file, interface }) => {
                    first_file.get_or_insert(*file);
                    format!(
                        "{} uses interface '{interface}' of '{next_name}'",
                        file_path(file)
                    )
                },
                Some(SvDependencyUse::VunitLib) => {
                    format!("it searches '{next_name}' like every SystemVerilog library")
                },
            };
            edges.push(format!(
                "library '{name}' depends on '{next_name}': {reason}"
            ));
        }
        let names: Vec<&str> = cycle
            .iter()
            .chain(cycle.first())
            .map(|&library| self.library(library).name.as_str())
            .collect();
        let diagnostic = Diagnostic::error(format!(
            "SystemVerilog libraries depend on each other in a cycle, which can't be compiled: \
             {}; {}; an explicit 'sv-libraries' list in [libraries.NAME.options] breaks the cycle",
            names.join(" -> "),
            edges.join("; ")
        ));
        match first_file {
            Some(file) => diagnostic.in_file(&self.file(file).path),
            None => diagnostic,
        }
    }

    /// The errors for cycles between the SystemVerilog dependencies of the libraries, which
    /// [`Self::finish`] found; they are among [`Self::diagnostics`], too.
    pub fn sv_library_cycle_errors(&self) -> &[Diagnostic] {
        &self.sv_cycles
    }

    /// The `risim analyze` arguments of the files of `library`, from its options.
    ///
    /// A library with SystemVerilog or Verilog files gets all of its options, with its
    /// [SystemVerilog dependencies](Library::sv_dependencies) as `sv-libraries`; any other gets
    /// only those that apply to VHDL ([`Options::without_sv_keys`]).
    pub fn analyze_args(&self, library: LibraryId) -> Vec<String> {
        self.analyze_args_searching(library, |_| true)
    }

    /// [`Self::analyze_args`], with only those SystemVerilog dependencies for which `searched`
    /// is true as `sv-libraries`.
    pub fn analyze_args_searching(
        &self,
        library: LibraryId,
        mut searched: impl FnMut(LibraryId) -> bool,
    ) -> Vec<String> {
        let library = self.library(library);
        if !library.has_sv {
            return library.options.without_sv_keys().analyze_args();
        }
        let mut options = library.options.clone();
        options.sv_libraries = Some(
            library
                .sv_dependencies()
                .filter(|&dependency| searched(dependency))
                .map(|dependency| self.library(dependency).name.clone())
                .collect(),
        );
        options.analyze_args()
    }

    /// `Library.add_vhdl_design_units`, which warns about duplicate units.
    fn register_design_units(&mut self, id: FileId, file: &SourceFile) {
        let library = &mut self.libraries[file.library.index()];
        let mut duplicates = Vec::new();
        for unit in &file.design_units {
            let previous = match unit.kind {
                kind @ (UnitKind::Entity
                | UnitKind::Package
                | UnitKind::Context
                | UnitKind::Configuration) => library
                    .primary_units
                    .insert(unit.name.clone(), PrimaryUnit { kind, file: id })
                    .map(|previous| previous.file),
                UnitKind::Architecture => {
                    let entity = unit.primary_unit.clone().unwrap_or_default();
                    let architectures = library.architectures.entry(entity).or_default();
                    if let Some((_, previous_file)) = architectures
                        .iter_mut()
                        .find(|(name, _)| *name == unit.name)
                    {
                        // Like a Python dict, a redefinition keeps the original position.
                        Some(std::mem::replace(previous_file, id))
                    } else {
                        architectures.push((unit.name.clone(), id));
                        None
                    }
                },
                UnitKind::PackageBody => library.package_bodies.insert(unit.name.clone(), id),
            };
            if let Some(previous) = previous {
                duplicates.push((unit, previous));
            }
        }
        for (unit, previous) in duplicates {
            let previous_path = if previous == id {
                &file.path
            } else {
                &self.files[previous.index()].path
            };
            self.diagnostics.push(
                Diagnostic::warning(format!(
                    "{} '{}' previously defined in {previous_path}",
                    unit.kind, unit.name
                ))
                .in_file(&file.path)
                .at(Some(unit.range)),
            );
        }
    }

    /// The libraries, in the order they were added.
    pub fn libraries(&self) -> impl ExactSizeIterator<Item = (LibraryId, &Library)> {
        self.libraries
            .iter()
            .enumerate()
            .map(|(index, library)| (LibraryId(id_from_index(index)), library))
    }

    /// The library `id`.
    pub fn library(&self, id: LibraryId) -> &Library {
        &self.libraries[id.index()]
    }

    /// The library named `name`, compared case-insensitively.
    pub fn find_library(&self, name: &str) -> Option<LibraryId> {
        self.libraries_by_lowercase_name
            .get(&name.to_ascii_lowercase())
            .copied()
    }

    /// The source files, in the order they were added.
    pub fn files(&self) -> impl ExactSizeIterator<Item = (FileId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(index, file)| (FileId(id_from_index(index)), file))
    }

    /// The source file `id`.
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.index()]
    }

    /// Warnings about duplicate design units, SystemVerilog definitions, and packages, and
    /// errors for cycles between the SystemVerilog dependencies of libraries.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Builds the dependency graph between files (`Project.create_dependency_graph`).
    ///
    /// A SystemVerilog file depends on the files of the packages it imports or refers to and of
    /// the interfaces it uses ([`Self::sv_analysis_dependencies`]).
    /// With `implementation_dependencies`, users of a package also depend on its body, users of
    /// an entity on all its architectures, component instantiations on matching entities and
    /// SystemVerilog modules in the visible libraries, and SystemVerilog instances on the
    /// definitions and entities they name ([`Self::sv_implementation_dependencies`]). This is
    /// the graph for deciding which files a testbench needs.
    pub fn dependency_graph(&self, implementation_dependencies: bool) -> DependencyAnalysis {
        let mut analysis = DependencyAnalysis {
            graph: DependencyGraph::new(),
            diagnostics: Vec::new(),
        };
        for (id, _) in self.files() {
            analysis.graph.add_node(id);
        }
        for (id, file) in self.files() {
            for dependency in self.reference_dependencies(
                file,
                implementation_dependencies,
                &mut analysis.diagnostics,
            ) {
                self.add_dependency(&mut analysis.graph, dependency, id);
            }
        }
        for (id, file) in self.files() {
            for dependency in self.secondary_unit_dependencies(file) {
                self.add_dependency(&mut analysis.graph, dependency, id);
            }
            for dependency in self.sv_analysis_dependencies(file) {
                self.add_dependency(&mut analysis.graph, dependency, id);
            }
        }
        if implementation_dependencies {
            for (id, file) in self.files() {
                for dependency in self.component_dependencies(id, file) {
                    self.add_dependency(&mut analysis.graph, dependency, id);
                }
                for dependency in self.sv_implementation_dependencies(file) {
                    self.add_dependency(&mut analysis.graph, dependency, id);
                }
            }
        }
        analysis
    }

    fn add_dependency(
        &self,
        graph: &mut DependencyGraph<FileId>,
        dependency: FileId,
        dependent: FileId,
    ) {
        // A file in two libraries doesn't depend on itself.
        if self.file(dependency).path == self.file(dependent).path {
            return;
        }
        if graph.add_dependency(dependency, dependent) {
            tracing::trace!(
                dependent = %self.file(dependent).path,
                dependency = %self.file(dependency).path,
                "adding dependency"
            );
        }
    }

    /// `_find_other_vhdl_design_unit_dependencies`.
    fn reference_dependencies(
        &self,
        file: &SourceFile,
        implementation_dependencies: bool,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Vec<FileId> {
        let mut result = Vec::new();
        // Each ambiguous entity is reported once per file, however often it is instantiated.
        let mut ambiguous: FxHashSet<(&str, &str)> = FxHashSet::default();
        for reference in &file.dependencies {
            let Some(library_id) = self.find_library(&reference.library) else {
                if !BUILTIN_LIBRARIES.contains(&reference.library.as_str()) {
                    tracing::warn!(file = %file.path, library = %reference.library, "failed to find library");
                }
                continue;
            };
            let library = self.library(library_id);
            let Some(primary_unit) = library.primary_unit(&reference.design_unit) else {
                if !library.is_external() {
                    tracing::warn!(
                        file = %file.path,
                        unit = %reference.design_unit,
                        library = %library.name,
                        "failed to find a primary design unit"
                    );
                }
                continue;
            };
            result.push(primary_unit.file);

            if reference.is_entity_reference() {
                let architectures = library.architectures(&reference.design_unit);
                let names: Vec<Option<&str>> = if reference.references_all_names_within()
                    || (reference.name_within.is_none() && implementation_dependencies)
                {
                    architectures
                        .iter()
                        .map(|(name, _)| Some(name.as_str()))
                        .collect()
                } else {
                    vec![reference.name_within.as_deref()]
                };
                for name in names {
                    let Some(name) = name else {
                        if architectures.len() > 1
                            && ambiguous.insert((&reference.library, &reference.design_unit))
                        {
                            diagnostics.push(self.ambiguous_architecture(
                                file,
                                reference,
                                architectures,
                            ));
                        }
                        continue;
                    };
                    if let Some(&(_, architecture_file)) = architectures
                        .iter()
                        .find(|(candidate, _)| candidate == name)
                    {
                        result.push(architecture_file);
                    } else {
                        tracing::warn!(
                            file = %file.path,
                            architecture = %name,
                            entity = %format!("{}.{}", library.name, reference.design_unit),
                            "failed to find architecture"
                        );
                    }
                }
            } else if reference.is_package_reference()
                && implementation_dependencies
                && let Some(body) = library.package_body(&reference.design_unit)
            {
                result.push(body);
            }
        }
        result
    }

    fn ambiguous_architecture(
        &self,
        file: &SourceFile,
        reference: &VhdlReference,
        architectures: &[(String, FileId)],
    ) -> Diagnostic {
        let choices: Vec<String> = architectures
            .iter()
            .map(|(name, architecture_file)| {
                format!("{name} ({})", self.file(*architecture_file).path)
            })
            .collect();
        Diagnostic::error(format!(
            "ambiguous direct entity instantiation of {}.{}: remove all but one architecture or \
             specify one of: {}",
            reference.library,
            reference.design_unit,
            choices.join(", ")
        ))
        .in_file(&file.path)
    }

    /// The files that an SV file needs for its analysis: those of the packages it imports or
    /// refers to, and those of the interfaces it uses, searched in its library and then in the
    /// library's SystemVerilog dependencies, in order, as hydrolyze searches them.
    ///
    /// An instance name counts if the first definition of that name is an interface; the
    /// scanner can't tell interface ports from instances.
    fn sv_analysis_dependencies(&self, file: &SourceFile) -> Vec<FileId> {
        let Some(sv_file) = &file.sv_file else {
            return Vec::new();
        };
        let own = self.library(file.library);
        let search: Vec<&Library> = std::iter::once(own)
            .chain(own.sv_dependencies().map(|library| self.library(library)))
            .collect();
        let packages = sv_file
            .imports
            .iter()
            .chain(&sv_file.package_references)
            .filter_map(|package| {
                search
                    .iter()
                    .find_map(|library| library.sv_package(package))
            });
        let interfaces = sv_file.instances.iter().filter_map(|name| {
            match search
                .iter()
                .find_map(|library| library.sv_definition(name))
            {
                Some((SvDefinitionKind::Interface, interface)) => Some(interface),
                _ => None,
            }
        });
        packages.chain(interfaces).collect()
    }

    /// The implementation dependencies of the instances of an SV file
    /// (`_find_verilog_module_dependencies`): the first definition of each name, searched in the
    /// file's library, its SystemVerilog dependencies, and then all libraries; and the first
    /// VHDL entity whose name equals it case-insensitively, searched the same way, with its
    /// architectures.
    fn sv_implementation_dependencies(&self, file: &SourceFile) -> Vec<FileId> {
        let Some(sv_file) = &file.sv_file else {
            return Vec::new();
        };
        if sv_file.instances.is_empty() {
            return Vec::new();
        }
        let own = self.library(file.library);
        let mut search = vec![file.library];
        for library in own
            .sv_dependencies()
            .chain(self.libraries().map(|(id, _)| id))
        {
            if !search.contains(&library) {
                search.push(library);
            }
        }
        let mut result = Vec::new();
        for name in &sv_file.instances {
            if let Some((_, definition)) = search
                .iter()
                .find_map(|&library| self.library(library).sv_definition(name))
            {
                result.push(definition);
            }
            let entity = name.to_ascii_lowercase();
            if let Some(library) =
                search
                    .iter()
                    .map(|&library| self.library(library))
                    .find(|library| {
                        library
                            .primary_unit(&entity)
                            .is_some_and(|unit| unit.kind == UnitKind::Entity)
                    })
                && let Some(unit) = library.primary_unit(&entity)
            {
                result.push(unit.file);
                result.extend(library.architectures(&entity).iter().map(|&(_, id)| id));
            }
        }
        result
    }

    /// `_find_primary_secondary_design_unit_dependencies`.
    fn secondary_unit_dependencies(&self, file: &SourceFile) -> Vec<FileId> {
        let library = self.library(file.library);
        file.design_units
            .iter()
            .filter(|unit| !unit.kind.is_primary())
            .filter_map(|unit| {
                let primary_name = unit.primary_unit.as_deref().unwrap_or_default();
                let found = library.primary_unit(primary_name);
                if found.is_none() {
                    tracing::warn!(
                        file = %file.path,
                        unit = %primary_name,
                        library = %library.name,
                        "failed to find a primary design unit"
                    );
                }
                found.map(|primary| primary.file)
            })
            .collect()
    }

    /// `_find_component_design_unit_dependencies`, extended to the libraries of the visible
    /// context.
    ///
    /// A component matches an entity with its simple name, together with all the entity's
    /// architectures, in every library of [`Self::visible_libraries`]. This holds for `pkg.comp`
    /// and `lib.pkg.comp` too, because default binding first looks for a visible entity with
    /// the component's simple name (IEEE 1076-2008 §7.3.3 a, b). Their package and library are
    /// visible anyway: `pkg` through a `use` clause, and `lib` through the package reference the
    /// parser adds for `lib.pkg.comp`. Other primary units with the component's name don't
    /// match, apart from SystemVerilog modules whose names equal the component's
    /// case-insensitively, which a component binds to, too.
    fn component_dependencies(&self, file_id: FileId, file: &SourceFile) -> Vec<FileId> {
        let instantiations = file.component_instantiations();
        if instantiations.is_empty() {
            return Vec::new();
        }
        let libraries = self.visible_libraries(file_id, file);
        let mut result = Vec::new();
        for instantiation in instantiations {
            let component = instantiation.component.as_str();
            let mut found = false;
            for &library_id in &libraries {
                let library = self.library(library_id);
                if let Some(primary) = library.primary_unit(component)
                    && primary.kind == UnitKind::Entity
                {
                    found = true;
                    result.push(primary.file);
                    result.extend(library.architectures(component).iter().map(|&(_, id)| id));
                }
                let before = result.len();
                result.extend(library.sv_modules_matching(component));
                found |= result.len() > before;
            }
            if !found {
                tracing::debug!(
                    file = %file.path,
                    %component,
                    "failed to find a matching entity for component"
                );
            }
        }
        result
    }

    /// Collects the source libraries that the context clauses of `file` make visible.
    ///
    /// The result starts with the file's own library, followed by every other non-external
    /// library named in a package or context reference, in first-reference order. The context
    /// clauses are those of the file itself, of the files with the primary units of its
    /// secondary units, and, transitively, of the files with the context declarations they
    /// reference. `use` clauses inside used packages don't count (IEEE 1076-2008 §12.4,
    /// NOTE 2). Like the rest of the parser, this works per file, not per design unit.
    ///
    /// Besides `use` clauses, package references come from `lib.pkg.comp` component
    /// instantiations and from package instantiations. The latter don't make their library
    /// visible, so they can add a library too many.
    fn visible_libraries(&self, file_id: FileId, file: &SourceFile) -> Vec<LibraryId> {
        let own_library = self.library(file.library);
        let mut reference_lists: Vec<&[VhdlReference]> = vec![&file.dependencies];
        // The primary unit of a secondary unit is often in the file itself.
        let mut visited: FxHashSet<FileId> = FxHashSet::from_iter([file_id]);
        for unit in file
            .design_units
            .iter()
            .filter(|unit| !unit.kind.is_primary())
        {
            let primary_name = unit.primary_unit.as_deref().unwrap_or_default();
            if let Some(primary) = own_library.primary_unit(primary_name)
                && visited.insert(primary.file)
            {
                reference_lists.push(&self.file(primary.file).dependencies);
            }
        }

        let mut libraries = vec![file.library];
        let mut seen_libraries: FxHashSet<LibraryId> = FxHashSet::from_iter([file.library]);
        let mut index = 0;
        while let Some(&references) = reference_lists.get(index) {
            index += 1;
            for reference in references {
                if !matches!(
                    reference.reference_type,
                    ReferenceType::Package | ReferenceType::Context
                ) {
                    continue;
                }
                let Some(library_id) = self.find_library(&reference.library) else {
                    continue;
                };
                let library = self.library(library_id);
                if library.is_external() {
                    continue;
                }
                if seen_libraries.insert(library_id) {
                    libraries.push(library_id);
                }
                if reference.reference_type == ReferenceType::Context
                    && let Some(primary) = library.primary_unit(&reference.design_unit)
                    && primary.kind == UnitKind::Context
                    && visited.insert(primary.file)
                {
                    reference_lists.push(&self.file(primary.file).dependencies);
                }
            }
        }
        libraries
    }

    /// Returns the files to compile in compile order.
    ///
    /// Without targets, these are all files. With targets, they are the targets and everything
    /// they need, following implementation dependencies (`get_minimal_file_set_in_compile_order`).
    /// The order comes from the dependencies without implementation dependencies, so a
    /// dependency cycle anywhere in the project prevents ordering.
    pub fn compile_order(&self, targets: Option<&[FileId]>) -> CompileOrder {
        let analysis = self.dependency_graph(false);
        let mut diagnostics = analysis.diagnostics;
        let selected: Option<FxHashSet<FileId>> = targets.map(|targets| {
            let implementation = self.dependency_graph(true);
            implementation.graph.dependencies(targets.iter().copied())
        });
        let files = analysis.graph.toposort().map(|sorted| match &selected {
            Some(selected) => sorted
                .into_iter()
                .filter(|id| selected.contains(id))
                .collect(),
            None => sorted,
        });
        if let Err(cycle) = &files {
            diagnostics.push(self.circular_dependency(cycle));
        }
        CompileOrder { files, diagnostics }
    }

    /// An error listing the files of a dependency cycle.
    pub fn circular_dependency(&self, cycle: &CircularDependency<FileId>) -> Diagnostic {
        let path: Vec<&str> = cycle
            .path
            .iter()
            .map(|&id| self.file(id).path.as_str())
            .collect();
        let diagnostic =
            Diagnostic::error(format!("found circular dependency: {}", path.join(" -> ")));
        match cycle.path.first() {
            Some(&first) => diagnostic.in_file(&self.file(first).path),
            None => diagnostic,
        }
    }
}

/// `VHDLSourceFile._find_design_units`.
fn design_units_of(design_file: &VhdlDesignFile) -> Vec<DesignUnit> {
    let primary = |name: &str, kind, range| DesignUnit {
        name: name.to_owned(),
        kind,
        primary_unit: None,
        range,
    };
    let mut units: Vec<DesignUnit> = design_file
        .entities
        .iter()
        .map(|entity| primary(&entity.identifier, UnitKind::Entity, entity.range))
        .collect();
    for (kind, named) in [
        (UnitKind::Context, &design_file.contexts),
        (UnitKind::Package, &design_file.packages),
    ] {
        units.extend(
            named
                .iter()
                .map(|unit| primary(&unit.identifier, kind, unit.range)),
        );
    }
    units.extend(
        design_file
            .architectures
            .iter()
            .map(|architecture| DesignUnit {
                name: architecture.identifier.clone(),
                kind: UnitKind::Architecture,
                primary_unit: Some(architecture.entity.clone()),
                range: architecture.range,
            }),
    );
    units.extend(design_file.configurations.iter().map(|configuration| {
        primary(
            &configuration.identifier,
            UnitKind::Configuration,
            configuration.range,
        )
    }));
    units.extend(design_file.package_bodies.iter().map(|body| DesignUnit {
        name: body.identifier.clone(),
        kind: UnitKind::PackageBody,
        primary_unit: Some(body.identifier.clone()),
        range: body.range,
    }));
    units
}

/// `VHDLSourceFile._find_dependencies`: `work` becomes the file's library, and configurations
/// reference all architectures of their entity.
fn dependencies_of(design_file: &VhdlDesignFile, library_name: &str) -> Vec<VhdlReference> {
    let mut references: Vec<VhdlReference> = design_file
        .references
        .iter()
        .map(|reference| {
            let mut reference = reference.clone();
            if reference.library == "work" {
                library_name.clone_into(&mut reference.library);
            }
            reference
        })
        .collect();
    references.extend(design_file.configurations.iter().map(|configuration| {
        VhdlReference::new(
            ReferenceType::Entity,
            library_name,
            &configuration.entity,
            Some("all"),
        )
    }));
    references
}
