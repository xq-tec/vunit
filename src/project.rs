// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Libraries, source files and design units, and the dependencies between files.
//!
//! A port of `project.py`, `library.py`, `source_file.py` and `design_unit.py`, restricted to
//! VHDL. Differences from VUnit:
//!
//! - An ambiguous direct entity instantiation is reported as a diagnostic instead of aborting.
//! - Missing libraries, units and architectures are only logged, as in VUnit.
//! - Recompilation isn't decided here (see the `compile` module).
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fmt;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use thiserror::Error;

use crate::dependency_graph::CircularDependency;
use crate::dependency_graph::DependencyGraph;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::Range;
use crate::sources::ContentHash;
use crate::vhdl_parser::ReferenceType;
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

/// A VHDL source file in a library.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The absolute path.
    pub path: Utf8PathBuf,
    /// The library the file is compiled into.
    pub library: LibraryId,
    /// The standard the file is compiled with.
    pub vhdl_standard: VhdlStandard,
    /// The hash of the raw file contents.
    pub content_hash: ContentHash,
    /// The parse result; `None` if the file couldn't be parsed.
    pub design_file: Option<Arc<VhdlDesignFile>>,
    /// Design units, in the order VUnit registers them.
    pub design_units: Vec<DesignUnit>,
    /// References to other units, with `work` replaced by the file's library.
    pub dependencies: Vec<VhdlReference>,
}

impl SourceFile {
    /// Names of the instantiated components.
    pub fn component_instantiations(&self) -> &[String] {
        self.design_file
            .as_ref()
            .map_or(&[], |design_file| &design_file.component_instantiations)
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
    files: FxHashMap<Utf8PathBuf, FileId>,
    primary_units: FxHashMap<String, PrimaryUnit>,
    /// Architectures per entity name, in the order they were added.
    architectures: FxHashMap<String, Vec<(String, FileId)>>,
    package_bodies: FxHashMap<String, FileId>,
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
}

/// The libraries and source files of a project.
#[derive(Debug, Clone, Default)]
pub struct Project {
    libraries: Vec<Library>,
    libraries_by_lowercase_name: FxHashMap<String, LibraryId>,
    files: Vec<SourceFile>,
    diagnostics: Vec<Diagnostic>,
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
            files: FxHashMap::default(),
            primary_units: FxHashMap::default(),
            architectures: FxHashMap::default(),
            package_bodies: FxHashMap::default(),
        });
        self.libraries_by_lowercase_name.insert(lowercase, id);
        Ok(id)
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
            vhdl_standard: vhdl_standard
                .unwrap_or_else(|| self.libraries[library.index()].vhdl_standard),
            content_hash,
            design_file,
            design_units,
            dependencies,
        };
        self.register_design_units(id, &file);
        self.libraries[library.index()]
            .files
            .insert(path.to_owned(), id);
        self.files.push(file);
        id
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

    /// Warnings about duplicate design units.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Builds the dependency graph between files (`Project.create_dependency_graph`).
    ///
    /// With `implementation_dependencies`, users of a package also depend on its body, users of
    /// an entity on all its architectures, and component instantiations on the matching entity
    /// and its architectures. This is the graph for deciding which files a testbench needs.
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
        }
        if implementation_dependencies {
            for (id, file) in self.files() {
                for dependency in self.component_dependencies(file) {
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

    /// `_find_component_design_unit_dependencies`.
    fn component_dependencies(&self, file: &SourceFile) -> Vec<FileId> {
        let library = self.library(file.library);
        let mut result = Vec::new();
        for component in file.component_instantiations() {
            if let Some(primary) = library.primary_unit(component) {
                result.push(primary.file);
                result.extend(library.architectures(component).iter().map(|&(_, id)| id));
            } else {
                tracing::debug!(%component, "failed to find a matching entity for component");
            }
        }
        result
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
