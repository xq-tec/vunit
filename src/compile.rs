// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Compiling the files that the testbenches need.
//!
//! Replaces `compile_source_files` of `sim_if/__init__.py` and the recompile logic of
//! `project.py`:
//!
//! - **Compile set:** the testbench files, the files that declare the VHDL configurations the
//!   test runs elaborate, and everything they need, following implementation dependencies.
//!   Files no testbench uses aren't compiled.
//! - **Recompile decision:** every file gets a compile key, a Merkle-style hash over its
//!   contents, standard, options, the simulator identity and the keys of its direct
//!   dependencies. A file is compiled if its stored key differs, its library archive is
//!   missing, or one of its dependencies is compiled. A change anywhere upstream changes the keys
//!   of all dependents. Compiling a file removes the stored keys of dependents outside the
//!   compile set: risim analyses a dependent in another file again when it loads the archive,
//!   and drops it if that fails (it is obsolete, LRM 13.5), or at the next load of its library if
//!   it is in another one. A file whose design unit `risim analyze` reports as obsolete loses its
//!   stored key too, unless a later batch compiles it, also when the dependency graph doesn't
//!   know the dependency; the report itself is dropped, because it is about another file.
//! - **Scheduling:** libraries that depend on each other are merged into scheduling units. A
//!   unit starts once the units it depends on have finished, and units run in parallel. Within
//!   a unit, files compile in compile order. The consecutive files of one library that need
//!   compiling compile in one batch, one `risim analyze` with all of them, because every call
//!   loads and saves the archives of the libraries; the reports are mapped back to the files by
//!   their locations.
//! - **Failures:** the transitive dependents of a failed file are skipped, also those in the
//!   batch of the failed file, whose reports are dropped; all other files are still compiled.
//!   The stored keys of failed and cancelled files and of all their dependents are removed,
//!   because the analysis may have made their units obsolete.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::dependency_graph::CircularDependency;
use crate::dependency_graph::DependencyGraph;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::ReportLine;
use crate::diagnostics::Severity;
use crate::discovery::Discovery;
use crate::process;
use crate::project::FileId;
use crate::project::LibraryId;
use crate::project::Project;
use crate::project::UnitKind;
use crate::runtime::ProcessLimit;
use crate::simulator;
use crate::simulator::CompileArgs;
use crate::simulator::Simulator;
use crate::simulator::SimulatorIdentity;
use crate::sources::ContentHash;
use crate::spec::CompileOptions;
use crate::store::CompileKey;
use crate::store::CompileState;
use crate::store::FileKey;
use crate::store::FileState;
use crate::store::OutputLayout;
use crate::vhdl_standard::VhdlStandard;

#[cfg(test)]
mod tests;

/// The files a compile starts from.
///
/// These are the entity and architecture files of all testbenches, and the files declaring the
/// VHDL configurations the test runs elaborate. Nothing depends on a configuration declaration,
/// so it must be a target itself.
pub fn targets(project: &Project, discovery: &Discovery) -> Vec<FileId> {
    let mut seen = FxHashSet::default();
    let testbench_files = discovery
        .testbenches
        .iter()
        .flat_map(|testbench| [testbench.entity_file, testbench.architecture_file]);
    let configuration_files = discovery.runs.iter().filter_map(|run| {
        let name = run.configuration.vhdl_configuration_name.as_deref()?;
        let library = discovery.testbenches.get(run.testbench)?.library;
        let unit = project
            .library(library)
            .primary_unit(&name.to_ascii_lowercase())
            .filter(|unit| unit.kind == UnitKind::Configuration);
        if unit.is_none() {
            tracing::debug!(name, "VHDL configuration not found");
        }
        unit.map(|unit| unit.file)
    });
    testbench_files
        .chain(configuration_files)
        .filter(|&file| seen.insert(file))
        .collect()
}

/// Everything a compile plan is made from.
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    /// The output directory.
    pub layout: &'a OutputLayout,
    /// The project.
    pub project: &'a Project,
    /// The files to compile, together with everything they need (see [`targets`]).
    pub targets: &'a [FileId],
    /// The analysis options.
    pub compile_options: &'a CompileOptions,
    /// The simulator.
    pub simulator: &'a Simulator,
}

/// A file of the compile set.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    /// The file in the project.
    pub id: FileId,
    /// The key of the file in the compile state and diagnostics.
    pub key: FileKey,
    /// The library name.
    pub library: String,
    /// The source file.
    pub path: Utf8PathBuf,
    /// The compile key the file has now.
    pub compile_key: CompileKey,
    /// Whether the file must be compiled: its stored compile key differs, its library archive
    /// is missing, or a dependency must be compiled.
    pub needs_compile: bool,
    /// The standard of the file.
    pub vhdl_standard: VhdlStandard,
    /// The file receiving the compiler output.
    pub output_file: Utf8PathBuf,
    library_dir: Utf8PathBuf,
    /// Direct dependencies, as indices into [`CompilePlan::files`].
    dependencies: Vec<usize>,
}

/// What a compile will do.
#[derive(Debug, Clone, Default)]
pub struct CompilePlan {
    files: Vec<PlannedFile>,
    /// Indices into `files` per scheduling unit, in compile order.
    units: Vec<Vec<usize>>,
    /// The units each unit depends on.
    unit_dependencies: Vec<Vec<usize>>,
    /// Edges from dependencies to dependents over all files of the project.
    graph: DependencyGraph<FileId>,
    /// The keys of all files of the project.
    all_keys: FxHashMap<FileId, FileKey>,
    /// The directory with the extracted builtins; their warnings and notes are dropped.
    builtins_root: Utf8PathBuf,
    /// The simulator that compiles, once the files are added.
    simulator: Option<Simulator>,
    /// The directories of all libraries.
    library_dirs: Vec<Utf8PathBuf>,
    /// The analysis options.
    flags: Vec<String>,
    /// Errors that prevent compiling: dependency cycles, unsupported standards.
    pub errors: Vec<Diagnostic>,
    /// Problems with the dependencies that don't prevent compiling.
    pub warnings: Vec<Diagnostic>,
}

impl CompilePlan {
    /// The compile set, in compile order.
    pub fn files(&self) -> &[PlannedFile] {
        &self.files
    }

    /// The scheduling units, as indices into [`files`](Self::files).
    pub fn units(&self) -> &[Vec<usize>] {
        &self.units
    }

    /// The number of files to compile.
    pub fn compile_count(&self) -> usize {
        self.files.iter().filter(|file| file.needs_compile).count()
    }

    /// The command line compiling the files `batch`, indices into [`files`](Self::files) of one
    /// library.
    ///
    /// Empty for a plan with errors or an empty batch.
    pub fn compile_command(&self, batch: &[usize]) -> Vec<String> {
        let (Some(simulator), Some(&first)) = (&self.simulator, batch.first()) else {
            return Vec::new();
        };
        let file = &self.files[first];
        let paths: Vec<&Utf8Path> = batch
            .iter()
            .map(|&index| self.files[index].path.as_path())
            .collect();
        // The standards were checked when the files were added.
        simulator
            .compile_command(&CompileArgs {
                library: &file.library,
                library_dir: &file.library_dir,
                vhdl_standard: file.vhdl_standard,
                library_dirs: &self.library_dirs,
                flags: &self.flags,
                files: &paths,
            })
            .unwrap_or_default()
    }

    /// Plans compiling `input.targets` given the compile `state`.
    pub fn new(input: &PlanInput<'_>, state: &CompileState) -> Self {
        let project = input.project;
        let mut plan = Self {
            builtins_root: input.layout.builtins_root(),
            ..Self::default()
        };

        let analysis = project.dependency_graph(false);
        let implementation = project.dependency_graph(true);
        let selected = implementation
            .graph
            .dependencies(input.targets.iter().copied());
        let selected_paths: FxHashSet<&Utf8Path> = selected
            .iter()
            .map(|&id| project.file(id).path.as_path())
            .collect();
        plan.warnings = analysis
            .diagnostics
            .into_iter()
            .filter(|diagnostic| {
                diagnostic
                    .file
                    .as_deref()
                    .is_none_or(|path| selected_paths.contains(path))
            })
            .collect();
        plan.graph = analysis.graph;
        plan.all_keys = project
            .files()
            .map(|(id, file)| (id, file_key(project, id, &file.path)))
            .collect();

        let order = match compile_order(&plan.graph, &selected) {
            Ok(order) => order,
            Err(cycle) => {
                plan.errors.push(project.circular_dependency(&cycle));
                return plan;
            },
        };

        let standards: BTreeSet<VhdlStandard> = order
            .iter()
            .map(|&id| project.file(id).vhdl_standard)
            .collect();
        plan.errors.extend(
            standards
                .into_iter()
                .filter_map(|standard| simulator::check_standard(standard).err())
                .map(|error| Diagnostic::error(error.to_string())),
        );
        if !plan.errors.is_empty() {
            return plan;
        }

        plan.add_files(input, state, &order);
        plan.schedule(project);
        plan
    }

    /// Adds the files of `order` with their compile keys.
    fn add_files(&mut self, input: &PlanInput<'_>, state: &CompileState, order: &[FileId]) {
        let project = input.project;
        self.simulator = Some(input.simulator.clone());
        self.library_dirs = library_dirs(project, input.layout);
        self.flags.clone_from(&input.compile_options.a_flags);
        let simulator_hash = identity_hash(input.simulator.identity());
        let positions: FxHashMap<FileId, usize> = order
            .iter()
            .enumerate()
            .map(|(position, &id)| (id, position))
            .collect();

        for &id in order {
            let file = project.file(id);
            let library = &project.library(file.library).name;
            let dependencies: Vec<usize> = self
                .graph
                .direct_dependencies(id)
                .filter_map(|dependency| positions.get(&dependency).copied())
                .collect();
            let compile_key = compile_key(&CompileKeyInput {
                content_hash: file.content_hash,
                library,
                vhdl_standard: file.vhdl_standard,
                flags: &input.compile_options.a_flags,
                simulator_hash,
                dependencies: dependencies
                    .iter()
                    .map(|&dependency| self.files[dependency].compile_key)
                    .collect(),
            });
            let key = self.all_keys[&id].clone();
            let library_dir = input.layout.library_dir(library);
            // Recompiling a file may make its dependents obsolete, even if its compile key is
            // unchanged, for example because its library archive was deleted.
            let needs_compile = state
                .files
                .get(&key)
                .is_none_or(|stored| stored.compile_key != compile_key)
                || !input.layout.library_archive(library).is_file()
                || dependencies
                    .iter()
                    .any(|&dependency| self.files[dependency].needs_compile);
            self.files.push(PlannedFile {
                id,
                key,
                library: library.clone(),
                path: file.path.clone(),
                compile_key,
                needs_compile,
                vhdl_standard: file.vhdl_standard,
                output_file: input.layout.compile_output_file(library, &file.path),
                library_dir,
                dependencies,
            });
        }
    }

    /// Groups the files into scheduling units: libraries that depend on each other, directly
    /// or indirectly, form one unit.
    fn schedule(&mut self, project: &Project) {
        let mut libraries: Vec<LibraryId> = Vec::new();
        let mut library_index: FxHashMap<LibraryId, usize> = FxHashMap::default();
        let library_of: Vec<usize> = self
            .files
            .iter()
            .map(|file| {
                let library = project.file(file.id).library;
                *library_index.entry(library).or_insert_with(|| {
                    libraries.push(library);
                    libraries.len() - 1
                })
            })
            .collect();

        let mut library_graph = DependencyGraph::new();
        for library in 0..libraries.len() {
            library_graph.add_node(library);
        }
        for (index, file) in self.files.iter().enumerate() {
            for &dependency in &file.dependencies {
                if library_of[dependency] != library_of[index] {
                    library_graph.add_dependency(library_of[dependency], library_of[index]);
                }
            }
        }
        // `reachable[a]` holds library `a` and the libraries it depends on, directly or
        // indirectly.
        let reachable: Vec<FxHashSet<usize>> = (0..libraries.len())
            .map(|library| library_graph.dependencies([library]))
            .collect();

        // Libraries that reach each other are strongly connected. Units are numbered in the
        // order their first file appears in the compile order.
        let mut unit_of_library: Vec<Option<usize>> = vec![None; libraries.len()];
        let mut unit_of_file = Vec::with_capacity(self.files.len());
        for &library in &library_of {
            let unit = if let Some(unit) = unit_of_library[library] {
                unit
            } else {
                let unit = self.units.len();
                self.units.push(Vec::new());
                for other in 0..libraries.len() {
                    if reachable[library].contains(&other) && reachable[other].contains(&library) {
                        unit_of_library[other] = Some(unit);
                    }
                }
                unit
            };
            unit_of_file.push(unit);
        }
        for (index, &unit) in unit_of_file.iter().enumerate() {
            self.units[unit].push(index);
        }
        self.unit_dependencies = self
            .units
            .iter()
            .enumerate()
            .map(|(unit, files)| {
                let dependencies: BTreeSet<usize> = files
                    .iter()
                    .flat_map(|&file| &self.files[file].dependencies)
                    .map(|&dependency| unit_of_file[dependency])
                    .filter(|&dependency| dependency != unit)
                    .collect();
                dependencies.into_iter().collect()
            })
            .collect();
    }
}

/// The directories of all libraries, passed to the simulator with `-L`.
pub(crate) fn library_dirs(project: &Project, layout: &OutputLayout) -> Vec<Utf8PathBuf> {
    project
        .libraries()
        .map(|(_, library)| {
            library
                .external_path
                .clone()
                .unwrap_or_else(|| layout.library_dir(&library.name))
        })
        .collect()
}

fn file_key(project: &Project, id: FileId, path: &Utf8Path) -> FileKey {
    let library = project.file(id).library;
    FileKey::new(&project.library(library).name, path)
}

/// Sorts `selected` in compile order, considering only dependencies within `selected`.
fn compile_order(
    graph: &DependencyGraph<FileId>,
    selected: &FxHashSet<FileId>,
) -> Result<Vec<FileId>, CircularDependency<FileId>> {
    let mut subgraph = DependencyGraph::new();
    for &id in graph.nodes() {
        if selected.contains(&id) {
            subgraph.add_node(id);
        }
    }
    let nodes = subgraph.nodes().to_vec();
    for id in nodes {
        for dependency in graph.direct_dependencies(id) {
            if selected.contains(&dependency) {
                subgraph.add_dependency(dependency, id);
            }
        }
    }
    subgraph.toposort()
}

/// What a compile key is computed from.
///
/// The `-L` paths of external libraries aren't part of it. Their libraries can't be configured
/// yet; once they can, a changed path or a recompiled external library must change the key.
struct CompileKeyInput<'a> {
    content_hash: ContentHash,
    library: &'a str,
    vhdl_standard: VhdlStandard,
    flags: &'a [String],
    simulator_hash: [u8; 32],
    dependencies: Vec<CompileKey>,
}

fn update_str(hasher: &mut blake3::Hasher, text: &str) {
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}

fn compile_key(input: &CompileKeyInput<'_>) -> CompileKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"risim-vunit compile key 1\0");
    hasher.update(&input.content_hash.0);
    update_str(&mut hasher, &input.library.to_ascii_lowercase());
    update_str(&mut hasher, input.vhdl_standard.vunit_name());
    hasher.update(&(input.flags.len() as u64).to_le_bytes());
    for flag in input.flags {
        update_str(&mut hasher, flag);
    }
    hasher.update(&input.simulator_hash);
    let mut dependencies = input.dependencies.clone();
    dependencies.sort_unstable();
    dependencies.dedup();
    hasher.update(&(dependencies.len() as u64).to_le_bytes());
    for dependency in dependencies {
        hasher.update(&dependency.0);
    }
    CompileKey(*hasher.finalize().as_bytes())
}

fn identity_hash(identity: &SimulatorIdentity) -> [u8; 32] {
    let json = serde_json::to_vec(identity).unwrap_or_default();
    *blake3::hash(&json).as_bytes()
}

// -------------------------------------------------------------------------------------------------
// Execution
// -------------------------------------------------------------------------------------------------

/// Progress of a compile, for live status reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileEvent {
    /// The compile set is known; `total` files need compiling.
    Started {
        /// The number of files to compile.
        total: usize,
    },
    /// A compile process is about to start. Previous diagnostics of the file are obsolete.
    FileStarted {
        /// The file key.
        key: FileKey,
        /// The library name.
        library: String,
        /// The source file.
        file: Utf8PathBuf,
        /// The 1-based count of files whose compilation has started, including this one.
        index: usize,
        /// The number of files to compile. Files skipped after failures are never started, so
        /// `index` may not reach `total`.
        total: usize,
    },
    /// The compiler reported a diagnostic for the file being compiled.
    Diagnostic {
        /// The key of the file being compiled.
        key: FileKey,
        /// The diagnostic; it may refer to another file.
        diagnostic: Diagnostic,
    },
    /// A file was compiled, failed, was cancelled, or was skipped.
    FileFinished {
        /// The file key.
        key: FileKey,
        /// What happened.
        status: FileStatus,
        /// All diagnostics of the file, replacing those reported before.
        diagnostics: Vec<Diagnostic>,
    },
}

/// What happened to a file of the compile set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatus {
    /// The stored compile key matched; the file wasn't compiled.
    UpToDate,
    /// The file was compiled successfully.
    Compiled,
    /// Compiling the file failed.
    Failed,
    /// The file wasn't compiled because a dependency failed or was skipped.
    Skipped,
    /// The compile process was terminated.
    Cancelled,
    /// The compile was cancelled before the file was started.
    NotStarted,
}

/// The overall result of a compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompileStatus {
    /// Every file of the compile set is compiled.
    Succeeded,
    /// A file failed or was skipped, or the plan has errors.
    Failed,
    /// The compile was cancelled before it finished.
    Cancelled,
}

/// The result of a compile.
#[derive(Debug, Clone)]
pub struct CompileReport {
    /// The overall result.
    pub status: CompileStatus,
    /// What happened to each file of the compile set, in compile order.
    pub files: Vec<(FileKey, FileStatus)>,
    /// The compile diagnostics of all files in the compile set: the stored warnings of
    /// up-to-date files, and the messages of compiled files.
    pub diagnostics: BTreeMap<FileKey, Vec<Diagnostic>>,
    /// The planning errors and warnings, which belong to the project diagnostics.
    pub project_diagnostics: Vec<Diagnostic>,
}

impl CompileReport {
    /// The number of files with `status`.
    pub fn count(&self, status: FileStatus) -> usize {
        self.files
            .iter()
            .filter(|(_, file_status)| *file_status == status)
            .count()
    }
}

/// The shared resources of a compile.
#[derive(Debug, Clone)]
pub struct CompileContext {
    /// The working directory of the compile processes.
    pub workspace_root: Utf8PathBuf,
    /// Limits the number of concurrent compile processes.
    pub limit: ProcessLimit,
    /// Receives the progress.
    pub events: mpsc::UnboundedSender<CompileEvent>,
    /// Cancels the compile.
    pub cancel: CancellationToken,
}

/// Compiles the compile set of `input`: adopts the simulator, plans, runs the compile
/// processes, updates `state` and saves it.
///
/// While compile processes run, the saved state has [`CompileState::compiling`] set, so that a
/// crash in the meantime discards the libraries.
pub async fn compile(
    input: &PlanInput<'_>,
    state: &mut CompileState,
    context: &CompileContext,
) -> CompileReport {
    let mut setup_errors = Vec::new();
    if let Err(error) = state.use_simulator(input.simulator.identity(), input.layout) {
        setup_errors.push(Diagnostic::error(format!(
            "failed to delete the libraries of the previous simulator: {error}"
        )));
    }
    let plan = CompilePlan::new(input, state);
    if plan.compile_count() > 0 {
        // Marks the libraries as changing, so that a crash before the end discards them.
        state.compiling = true;
        if let Err(error) = state.save(input.layout) {
            tracing::warn!(%error, "failed to save the compile state");
        }
    }
    let mut report = execute(plan, state, context).await;
    prune_state(state, input.project);
    state.compiling = false;
    if let Err(error) = state.save(input.layout) {
        tracing::warn!(%error, "failed to save the compile state");
    }
    if !setup_errors.is_empty() {
        report.status = CompileStatus::Failed;
        report.project_diagnostics.extend(setup_errors);
    }
    report
}

/// Removes the state of files that aren't part of the project anymore.
fn prune_state(state: &mut CompileState, project: &Project) {
    let keys: FxHashSet<FileKey> = project
        .files()
        .map(|(id, file)| file_key(project, id, &file.path))
        .collect();
    state.files.retain(|key, _| keys.contains(key));
}

struct Shared {
    plan: CompilePlan,
    context: CompileContext,
    started: AtomicUsize,
    total: usize,
    /// The number of batches started so far, which orders the batches of one library.
    batches: AtomicUsize,
    /// The files whose design units a batch reported as obsolete, with the number of the batch.
    obsolete: Mutex<Vec<(FileKey, usize)>>,
    /// The files that a batch compiled, with the number of the last such batch.
    compiled_in: Mutex<FxHashMap<FileKey, usize>>,
}

struct FileResult {
    status: FileStatus,
    diagnostics: Vec<Diagnostic>,
}

impl FileResult {
    const fn without_diagnostics(status: FileStatus) -> Self {
        Self {
            status,
            diagnostics: Vec::new(),
        }
    }
}

/// Runs the compile processes of `plan` and updates `state`; doesn't save it.
pub async fn execute(
    compile_plan: CompilePlan,
    state: &mut CompileState,
    context: &CompileContext,
) -> CompileReport {
    let total = compile_plan.compile_count();
    emit(&context.events, CompileEvent::Started { total });
    let mut project_diagnostics = compile_plan.warnings.clone();
    if !compile_plan.errors.is_empty() {
        project_diagnostics.extend(compile_plan.errors.iter().cloned());
        return CompileReport {
            status: CompileStatus::Failed,
            files: Vec::new(),
            diagnostics: BTreeMap::new(),
            project_diagnostics,
        };
    }

    let shared = Arc::new(Shared {
        plan: compile_plan,
        context: context.clone(),
        started: AtomicUsize::new(0),
        total,
        batches: AtomicUsize::new(0),
        obsolete: Mutex::new(Vec::new()),
        compiled_in: Mutex::new(FxHashMap::default()),
    });
    let plan = &shared.plan;
    let mut results: Vec<Option<FileResult>> = (0..plan.files.len()).map(|_| None).collect();
    let mut bad: FxHashSet<usize> = FxHashSet::default();
    let mut missing: Vec<usize> = plan.unit_dependencies.iter().map(Vec::len).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); plan.units.len()];
    for (unit, dependencies) in plan.unit_dependencies.iter().enumerate() {
        for &dependency in dependencies {
            dependents[dependency].push(unit);
        }
    }

    let mut tasks = JoinSet::new();
    for (unit, &count) in missing.iter().enumerate() {
        if count == 0 {
            spawn_unit(&mut tasks, &shared, unit, &bad);
        }
    }
    while let Some(joined) = tasks.join_next().await {
        let (unit, unit_results) = match joined {
            Ok(result) => result,
            Err(error) => {
                // A panic in a unit; its files stay without result and count as failed.
                tracing::error!(%error, "compile task failed");
                continue;
            },
        };
        for (index, result) in unit_results {
            if !matches!(result.status, FileStatus::UpToDate | FileStatus::Compiled) {
                bad.insert(index);
            }
            results[index] = Some(result);
        }
        for &dependent in &dependents[unit] {
            missing[dependent] -= 1;
            if missing[dependent] == 0 {
                spawn_unit(&mut tasks, &shared, dependent, &bad);
            }
        }
    }

    let report = finish(plan, results, state, project_diagnostics);
    forget_obsolete(&shared, state);
    report
}

/// Removes the stored keys of the files whose design units a batch reported as obsolete, unless
/// a later batch compiled them, so that they are compiled again.
fn forget_obsolete(shared: &Shared, state: &mut CompileState) {
    let compiled_in = shared
        .compiled_in
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let obsolete = shared
        .obsolete
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (key, batch) in obsolete.iter() {
        if compiled_in.get(key).is_none_or(|compiled| compiled < batch) {
            state.files.remove(key);
        }
    }
}

type UnitResults = (usize, Vec<(usize, FileResult)>);

/// Starts compiling `unit`; `failed` are the files whose dependents must be skipped.
fn spawn_unit(
    tasks: &mut JoinSet<UnitResults>,
    shared: &Arc<Shared>,
    unit: usize,
    failed: &FxHashSet<usize>,
) {
    let shared = Arc::clone(shared);
    let failed = failed.clone();
    tasks.spawn(async move { run_unit(&shared, unit, failed).await });
}

fn emit(events: &mpsc::UnboundedSender<CompileEvent>, event: CompileEvent) {
    if events.send(event).is_err() {
        tracing::trace!("nobody listens to compile events");
    }
}

/// Updates `state` from the results and builds the report.
fn finish(
    plan: &CompilePlan,
    results: Vec<Option<FileResult>>,
    state: &mut CompileState,
    project_diagnostics: Vec<Diagnostic>,
) -> CompileReport {
    let mut report = CompileReport {
        status: CompileStatus::Succeeded,
        files: Vec::with_capacity(plan.files.len()),
        diagnostics: BTreeMap::new(),
        project_diagnostics,
    };
    let mut invalid = Vec::new();
    let mut compiled = FxHashSet::default();
    for (file, result) in plan.files.iter().zip(results) {
        let result = result.unwrap_or(FileResult::without_diagnostics(FileStatus::Failed));
        match result.status {
            FileStatus::UpToDate => {
                if let Some(stored) = state.files.get(&file.key) {
                    report
                        .diagnostics
                        .insert(file.key.clone(), stored.diagnostics.clone());
                }
            },
            FileStatus::Compiled => {
                compiled.insert(file.id);
                state.files.insert(
                    file.key.clone(),
                    FileState {
                        compile_key: file.compile_key,
                        diagnostics: result.diagnostics.clone(),
                    },
                );
                report
                    .diagnostics
                    .insert(file.key.clone(), result.diagnostics);
            },
            FileStatus::Failed | FileStatus::Cancelled => {
                invalid.push(file.id);
                report
                    .diagnostics
                    .insert(file.key.clone(), result.diagnostics);
            },
            FileStatus::Skipped | FileStatus::NotStarted => {},
        }
        report.status = match (report.status, result.status) {
            (_, FileStatus::Cancelled | FileStatus::NotStarted) | (CompileStatus::Cancelled, _) => {
                CompileStatus::Cancelled
            },
            (_, FileStatus::Failed | FileStatus::Skipped) | (CompileStatus::Failed, _) => {
                CompileStatus::Failed
            },
            (status, _) => status,
        };
        report.files.push((file.key.clone(), result.status));
    }
    // Compiled files may make their dependents obsolete, so those must be compiled again unless
    // they were compiled after them.
    let obsolete = plan
        .graph
        .dependents(compiled.iter().copied())
        .into_iter()
        .filter(|id| !compiled.contains(id));
    for id in plan.graph.dependents(invalid).into_iter().chain(obsolete) {
        if let Some(key) = plan.all_keys.get(&id) {
            state.files.remove(key);
        }
    }
    report
}

async fn run_unit(shared: &Shared, unit: usize, mut bad: FxHashSet<usize>) -> UnitResults {
    let plan = &shared.plan;
    let files = &plan.units[unit];
    let mut results = Vec::with_capacity(files.len());
    let mut position = 0;
    while position < files.len() {
        let first = files[position];
        let file = &plan.files[first];
        if !file.needs_compile {
            results.push((first, FileResult::without_diagnostics(FileStatus::UpToDate)));
            position += 1;
            continue;
        }
        if depends_on(file, &bad) {
            bad.insert(first);
            let status = if shared.context.cancel.is_cancelled() {
                FileStatus::NotStarted
            } else {
                emit(
                    &shared.context.events,
                    CompileEvent::FileFinished {
                        key: file.key.clone(),
                        status: FileStatus::Skipped,
                        diagnostics: Vec::new(),
                    },
                );
                FileStatus::Skipped
            };
            results.push((first, FileResult::without_diagnostics(status)));
            position += 1;
            continue;
        }
        // The batch: this file and the following ones of its library that need compiling.
        let mut batch = vec![first];
        position += 1;
        while let Some(&next) = files.get(position) {
            let candidate = &plan.files[next];
            if candidate.library != file.library
                || !candidate.needs_compile
                || depends_on(candidate, &bad)
            {
                break;
            }
            batch.push(next);
            position += 1;
        }
        for (member, result) in batch
            .iter()
            .copied()
            .zip(compile_batch(shared, &batch).await)
        {
            if !matches!(result.status, FileStatus::UpToDate | FileStatus::Compiled) {
                bad.insert(member);
            }
            results.push((member, result));
        }
    }
    (unit, results)
}

/// The code of the warning of `risim analyze` about a design unit of another file that the
/// call made obsolete.
const OBSOLETE_DESIGN_UNIT: &str = "obsolete-design-unit";

/// Whether `file` depends directly on one of the files `files`.
fn depends_on(file: &PlannedFile, files: &FxHashSet<usize>) -> bool {
    file.dependencies
        .iter()
        .any(|dependency| files.contains(dependency))
}

/// Whether `diagnostics` has an error.
fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}

/// Compiles the files `batch` of one library in one process, and returns their results in the
/// same order.
async fn compile_batch(shared: &Shared, batch: &[usize]) -> Vec<FileResult> {
    let context = &shared.context;
    let not_started = || {
        batch
            .iter()
            .map(|_| FileResult::without_diagnostics(FileStatus::NotStarted))
            .collect()
    };
    if context.cancel.is_cancelled() {
        return not_started();
    }
    let _permit = tokio::select! {
        permit = context.limit.acquire() => match permit {
            Ok(permit) => permit,
            Err(_closed) => return not_started(),
        },
        () = context.cancel.cancelled() => return not_started(),
    };

    let number = shared.batches.fetch_add(1, Ordering::Relaxed);
    let mut output = BatchOutput::new(shared, batch, number);
    let first = output.files[0];
    output.start(0);
    let result = async {
        fs::create_dir_all(&first.library_dir)?;
        process::run_piped(
            &shared.plan.compile_command(batch),
            &context.workspace_root,
            &first.output_file,
            &context.cancel,
            |line| output.push_line(line),
        )
        .await
    }
    .await;
    // Every file of the batch gets the whole output.
    for file in &output.files[1..] {
        if let Err(error) = fs::copy(&first.output_file, &file.output_file) {
            tracing::debug!(%error, file = %file.output_file, "failed to copy the compile output");
        }
    }
    output.finish(&result)
}

/// The progress and the reports of a batch.
struct BatchOutput<'a> {
    shared: &'a Shared,
    /// The number of the batch ([`Shared::batches`]).
    number: usize,
    /// The indices of the files into [`CompilePlan::files`].
    batch: &'a [usize],
    files: Vec<&'a PlannedFile>,
    /// Whether a file has been reported as started.
    ///
    /// The first file starts with the batch, the others at their first report or at the end of
    /// the batch, so that a cancel keeps the others from starting.
    started: Vec<bool>,
    /// The diagnostics of each file.
    diagnostics: Vec<Vec<Diagnostic>>,
    /// The output that isn't a JSON report, such as a crash message.
    text: FailureOutput,
    /// The sources the reports refer to, for converting their byte offsets.
    sources: FxHashMap<Utf8PathBuf, Option<Vec<u8>>>,
}

impl<'a> BatchOutput<'a> {
    fn new(shared: &'a Shared, batch: &'a [usize], number: usize) -> Self {
        Self {
            shared,
            number,
            batch,
            files: batch
                .iter()
                .map(|&index| &shared.plan.files[index])
                .collect(),
            started: vec![false; batch.len()],
            diagnostics: vec![Vec::new(); batch.len()],
            text: FailureOutput::default(),
            sources: FxHashMap::default(),
        }
    }

    /// Reports that the file `member` of the batch starts compiling, unless it did already.
    fn start(&mut self, member: usize) {
        if std::mem::replace(&mut self.started[member], true) {
            return;
        }
        let file = self.files[member];
        emit(
            &self.shared.context.events,
            CompileEvent::FileStarted {
                key: file.key.clone(),
                library: file.library.clone(),
                file: file.path.clone(),
                index: self.shared.started.fetch_add(1, Ordering::Relaxed) + 1,
                total: self.shared.total,
            },
        );
    }

    /// Takes a line of the output: a report goes to the file of its location, otherwise to the
    /// first one.
    fn push_line(&mut self, line: &str) {
        let report = match ReportLine::parse(line) {
            ReportLine::Diagnostic(report) => report,
            ReportLine::Other => return,
            ReportLine::Text => {
                self.text.push(line);
                return;
            },
        };
        let root = &self.shared.context.workspace_root;
        let report_path = report
            .location
            .as_ref()
            .and_then(|location| location.path(root));
        // A design unit of a file outside the batch that the batch made obsolete; that file is
        // compiled again ([`forget_obsolete`]).
        if report.code.as_deref() == Some(OBSOLETE_DESIGN_UNIT)
            && let Some(path) = &report_path
            && !self.files.iter().any(|file| file.path == *path)
        {
            let key = FileKey::new(&self.files[0].library, path);
            self.shared
                .obsolete
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((key, self.number));
            return;
        }
        // VUnit's and OSVVM's sources trigger warnings that users can't do anything about.
        if report.severity != Severity::Error
            && report_path
                .as_ref()
                .is_some_and(|path| path.starts_with(&self.shared.plan.builtins_root))
        {
            return;
        }
        let member = report_path
            .as_ref()
            .and_then(|path| self.files.iter().position(|file| file.path == *path))
            .unwrap_or(0);
        for location_path in report
            .location
            .iter()
            .chain(
                report
                    .notes
                    .iter()
                    .filter_map(|note| note.location.as_ref()),
            )
            .filter_map(|location| location.path(root))
        {
            self.sources
                .entry(location_path)
                .or_insert_with_key(|key| fs::read(key).ok());
        }
        let sources = &self.sources;
        let converted = report.to_diagnostics(root, &self.files[member].path, |source| {
            sources.get(source).and_then(Option::as_deref)
        });
        self.start(member);
        for diagnostic in converted {
            emit(
                &self.shared.context.events,
                CompileEvent::Diagnostic {
                    key: self.files[member].key.clone(),
                    diagnostic: diagnostic.clone(),
                },
            );
            self.diagnostics[member].push(diagnostic);
        }
    }

    /// Returns the results of the files after the process ended with `result`.
    ///
    /// A file that depends on a failed or skipped one of the batch is skipped, and its reports
    /// are dropped. A failure of the process that no report explains, or an exit status other
    /// than 1, the status of reported errors, such as a crash, fails every file without errors.
    fn finish(mut self, result: &io::Result<process::Outcome>) -> Vec<FileResult> {
        let first = self.files[0];
        let failure = match result {
            Ok(process::Outcome::Exited(status))
                if !status.success()
                    && (status.code() != Some(1)
                        || !self.diagnostics.iter().any(|file| has_errors(file))) =>
            {
                Some(
                    self.text
                        .message(&first.output_file)
                        .unwrap_or_else(|| format!("risim analyze failed ({status})")),
                )
            },
            Ok(_) => None,
            Err(error) => Some(format!("failed to run risim analyze: {error}")),
        };
        let cancelled = matches!(result, Ok(process::Outcome::Cancelled));
        let mut failed = FxHashSet::default();
        let mut results = Vec::with_capacity(self.files.len());
        for member in 0..self.files.len() {
            if cancelled && !self.started[member] {
                results.push(FileResult::without_diagnostics(FileStatus::NotStarted));
                continue;
            }
            self.start(member);
            let file = self.files[member];
            let mut file_diagnostics = std::mem::take(&mut self.diagnostics[member]);
            let status = if depends_on(file, &failed) {
                file_diagnostics.clear();
                FileStatus::Skipped
            } else if cancelled {
                FileStatus::Cancelled
            } else if has_errors(&file_diagnostics) {
                FileStatus::Failed
            } else if let Some(message) = &failure {
                file_diagnostics.push(Diagnostic::error(message.clone()).in_file(&file.path));
                FileStatus::Failed
            } else {
                FileStatus::Compiled
            };
            if matches!(status, FileStatus::Skipped | FileStatus::Failed) {
                failed.insert(self.batch[member]);
            }
            if status == FileStatus::Compiled {
                self.shared
                    .compiled_in
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(file.key.clone(), self.number);
            }
            emit(
                &self.shared.context.events,
                CompileEvent::FileFinished {
                    key: file.key.clone(),
                    status,
                    diagnostics: file_diagnostics.clone(),
                },
            );
            results.push(FileResult {
                status,
                diagnostics: file_diagnostics,
            });
        }
        results
    }
}

/// The beginning of the output of a compile process, which becomes the message of a failure
/// without a parsable error. The output file has all of it.
#[derive(Debug, Default)]
struct FailureOutput {
    text: String,
    lines: usize,
    truncated: bool,
}

impl FailureOutput {
    /// At most this many lines are kept…
    const MAX_LINES: usize = 50;
    /// …and at most about this many bytes.
    const MAX_BYTES: usize = 8 * 1024;

    fn push(&mut self, line: &str) {
        if self.truncated {
            return;
        }
        if self.lines == Self::MAX_LINES || self.text.len() + line.len() > Self::MAX_BYTES {
            self.truncated = true;
            return;
        }
        if self.lines > 0 {
            self.text.push('\n');
        }
        self.text.push_str(line);
        self.lines += 1;
    }

    /// The trimmed output, pointing to `output_file` if it was cut off; `None` if it is empty.
    fn message(&self, output_file: &Utf8Path) -> Option<String> {
        let text = self.text.trim();
        if self.truncated {
            let separator = if text.is_empty() { "" } else { "\n" };
            Some(format!(
                "{text}{separator}… (see {output_file} for the full output)"
            ))
        } else {
            (!text.is_empty()).then(|| text.to_owned())
        }
    }
}
