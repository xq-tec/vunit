// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The task that owns the state of a workspace.
//!
//! The actor processes commands from the [`Workspace`](super::Workspace) handles, progress
//! reports of its compile and simulation tasks, and file system events. After every change it
//! publishes a new [`Snapshot`] and sends the events, flushing diagnostic changes before every
//! operation event so that clients see them in order.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

use camino::Utf8Path;
use rustc_hash::FxHashMap;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::Opened;
use super::RequestTag;
use super::Snapshot;
use super::WorkspaceEvent;
use super::WorkspaceEventKind;
use super::loader::Loaded;
use super::loader::Loader;
use super::loader::Model;
use super::operation::Operation;
use crate::compile;
use crate::compile::CompileContext;
use crate::compile::CompileEvent;
use crate::compile::CompileReport;
use crate::compile::CompileStatus;
use crate::compile::FileStatus;
use crate::compile::PlanInput;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::DiagnosticSource;
use crate::discovery::Testcase;
use crate::runner;
use crate::runner::SimulationContext;
use crate::runner::SimulationEvent;
use crate::runner::SimulationEvents;
use crate::runner::SimulationInput;
use crate::runner::SimulationPlan;
use crate::runner::SimulationRequest;
use crate::runner::TestcaseLocks;
use crate::runtime::Runtime;
use crate::store::CompileState;
use crate::store::FileKey;
use crate::store::OutputLayout;
use crate::store::OutputLock;
use crate::store::ResultStore;
use crate::store::TestCounts;
use crate::sync::lock_unpoisoned;
use crate::watch::Change;
use crate::watch::Debouncer;
use crate::watch::DirectoryWatcher;
use crate::watch::classify;

/// A request from a [`Workspace`](super::Workspace) handle.
#[derive(Debug)]
pub(super) enum Command {
    Compile {
        tag: Option<RequestTag>,
    },
    Simulate {
        requests: Vec<SimulationRequest>,
        tag: Option<RequestTag>,
    },
    CancelAll,
    Cancel(RequestTag),
    Close(oneshot::Sender<()>),
}

/// A report from a compile or simulation task.
#[derive(Debug)]
enum Internal {
    Compile(CompileEvent),
    /// The compile task ended; `None` if it panicked.
    CompileDone(Option<Box<(CompileState, CompileReport)>>),
    Simulation {
        id: u64,
        event: SimulationEvent,
    },
    /// The simulation task ended, also after a panic.
    SimulationDone {
        id: u64,
    },
}

/// The running compile, of a compile operation or of the compile phase of a simulate.
#[derive(Debug)]
struct RunningCompile {
    operation: Operation,
    /// The project being compiled; a simulate runs the testcases of this project, even if the
    /// project changes during the compile.
    model: Arc<Model>,
    cancel: CancellationToken,
}

/// A simulate operation running its tests.
#[derive(Debug)]
struct RunningSimulation {
    tags: Vec<RequestTag>,
    /// The number of tests to run.
    total: usize,
    finished: bool,
    counts: TestCounts,
    cancel: CancellationToken,
}

/// How the actor is shutting down.
#[derive(Debug, Default)]
struct Closing {
    replies: Vec<oneshot::Sender<()>>,
}

pub(super) struct Actor {
    root: Arc<Utf8Path>,
    layout: OutputLayout,
    runtime: Runtime,
    events: mpsc::UnboundedSender<WorkspaceEvent>,
    snapshot: watch::Sender<Arc<Snapshot>>,
    commands: mpsc::UnboundedReceiver<Command>,
    commands_closed: bool,
    internal_sender: mpsc::UnboundedSender<Internal>,
    internal: mpsc::UnboundedReceiver<Internal>,

    lock: Option<OutputLock>,
    loader: Arc<Mutex<Loader>>,
    model: Option<Arc<Model>>,
    testcases: Vec<Testcase>,
    /// `None` while a compile runs; the compile task owns it.
    compile_state: Option<CompileState>,
    results: Arc<ResultStore>,
    testcase_locks: Arc<TestcaseLocks>,

    /// Cancels everything when the workspace closes.
    cancel: CancellationToken,
    /// Cancels the current operations; replaced by a new child of `cancel` on every cancel.
    operations_cancel: CancellationToken,
    running_compile: Option<RunningCompile>,
    queued: Option<Operation>,
    simulations: FxHashMap<u64, RunningSimulation>,
    next_simulation_id: u64,
    closing: Option<Closing>,

    watcher: Option<DirectoryWatcher>,
    watch_events: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    debouncer: Debouncer,
    /// The watcher failed and reported it in `watcher_diagnostics`.
    watch_failed: bool,

    config_diagnostics: Vec<Diagnostic>,
    project_diagnostics: Vec<Diagnostic>,
    /// The planning errors and warnings of the last compile; part of the project set.
    plan_diagnostics: Vec<Diagnostic>,
    watcher_diagnostics: Vec<Diagnostic>,
    compile_diagnostics: BTreeMap<FileKey, Vec<Diagnostic>>,
    /// Per testcase; `None` holds the problems with the patterns of the last simulation.
    simulation_diagnostics: BTreeMap<Option<String>, Vec<Diagnostic>>,

    /// The sets that changed since the last publish.
    dirty: BTreeSet<DiagnosticSource>,
    results_dirty: bool,
    testcases_dirty: bool,
    /// What was published last.
    published: Snapshot,
    testcases_sent: bool,
}

impl Actor {
    pub(super) fn new(
        runtime: Runtime,
        root: Arc<Utf8Path>,
        opened: Opened,
        commands: mpsc::UnboundedReceiver<Command>,
        events: mpsc::UnboundedSender<WorkspaceEvent>,
    ) -> (Self, watch::Receiver<Arc<Snapshot>>) {
        let Opened {
            layout,
            lock,
            loader,
            loaded,
            compile_state,
            results,
        } = opened;
        let (snapshot, snapshot_receiver) = watch::channel(Arc::new(Snapshot::default()));
        let (internal_sender, internal) = mpsc::unbounded_channel();
        let (watch_sender, watch_events) = mpsc::unbounded_channel();
        let mut watcher_diagnostics = Vec::new();
        let watcher = match DirectoryWatcher::new(watch_sender) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                watcher_diagnostics.push(watch_failure(&error));
                None
            },
        };
        let compile_diagnostics = compile_state
            .files
            .iter()
            .map(|(key, file)| (key.clone(), file.diagnostics.clone()))
            .collect();
        let cancel = CancellationToken::new();
        let operations_cancel = cancel.child_token();
        let watch_failed = watcher.is_none();
        let mut actor = Self {
            root,
            layout,
            runtime,
            events,
            snapshot,
            commands,
            commands_closed: false,
            internal_sender,
            internal,
            lock: Some(lock),
            loader: Arc::new(Mutex::new(loader)),
            model: None,
            testcases: Vec::new(),
            compile_state: Some(compile_state),
            results: Arc::new(results),
            testcase_locks: Arc::default(),
            cancel,
            operations_cancel,
            running_compile: None,
            queued: None,
            simulations: FxHashMap::default(),
            next_simulation_id: 0,
            closing: None,
            watcher,
            watch_events,
            debouncer: Debouncer::default(),
            watch_failed,
            config_diagnostics: Vec::new(),
            project_diagnostics: Vec::new(),
            plan_diagnostics: Vec::new(),
            watcher_diagnostics,
            compile_diagnostics,
            simulation_diagnostics: BTreeMap::new(),
            dirty: DiagnosticSource::ALL.into_iter().collect(),
            results_dirty: true,
            testcases_dirty: true,
            published: Snapshot::default(),
            testcases_sent: false,
        };
        actor.apply_loaded(loaded);
        // The snapshot is complete before the workspace handle is returned.
        actor.publish();
        (actor, snapshot_receiver)
    }

    fn loader(&self) -> MutexGuard<'_, Loader> {
        lock_loader(&self.loader)
    }

    pub(super) async fn run(mut self) {
        tracing::debug!(root = %self.root, "workspace opened");
        loop {
            if self.closing.is_some() && self.is_idle() {
                break;
            }
            let deadline = self.debouncer.deadline();
            tokio::select! {
                biased;
                Some(first) = self.internal.recv() => {
                    self.handle_internal(first).await;
                    // Handle a burst of progress reports before publishing.
                    for _ in 0..256 {
                        let Ok(next) = self.internal.try_recv() else {
                            break;
                        };
                        self.handle_internal(next).await;
                    }
                },
                command = self.commands.recv(), if !self.commands_closed => {
                    if let Some(command) = command {
                        self.handle_command(command).await;
                    } else {
                        // All handles are gone: finish the operations, then close.
                        self.commands_closed = true;
                        self.closing.get_or_insert_default();
                    }
                },
                Some(event) = self.watch_events.recv() => self.handle_watch_event(event),
                () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)),
                    if deadline.is_some() =>
                {
                    if let Some(change) = self.debouncer.take() {
                        self.reload(change).await;
                    }
                },
            }
            self.publish();
        }
        self.finish_close();
    }

    fn is_idle(&self) -> bool {
        self.running_compile.is_none() && self.queued.is_none() && self.simulations.is_empty()
    }

    // ---------------------------------------------------------------------------------------------
    // Commands
    // ---------------------------------------------------------------------------------------------

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Compile { tag } => self.request(Operation::compile(tag)).await,
            Command::Simulate { requests, tag } => {
                self.request(Operation::simulate(requests, tag)).await;
            },
            Command::CancelAll => self.cancel_all(),
            Command::Cancel(tag) => self.cancel_request(&tag),
            Command::Close(reply) => {
                self.cancel.cancel();
                self.drop_queued();
                self.closing.get_or_insert_default().replies.push(reply);
            },
        }
    }

    /// Starts `operation`, or merges it into the queued one if a compile is running.
    async fn request(&mut self, operation: Operation) {
        if self.closing.is_some() {
            tracing::debug!(root = %self.root, "ignoring a request to a closing workspace");
            return;
        }
        if self.running_compile.is_none() && self.queued.is_none() {
            self.start(operation).await;
        } else {
            self.queued = Some(match self.queued.take() {
                Some(queued) => queued.merge(operation),
                None => operation,
            });
        }
    }

    fn cancel_all(&mut self) {
        tracing::debug!(root = %self.root, "cancelling all operations");
        self.operations_cancel.cancel();
        self.operations_cancel = self.cancel.child_token();
        self.drop_queued();
    }

    /// Cancels what the request with `tag` asked for (see [`Workspace::cancel`]).
    ///
    /// [`Workspace::cancel`]: super::Workspace::cancel
    fn cancel_request(&mut self, tag: &RequestTag) {
        tracing::debug!(root = %self.root, ?tag, "cancelling a request");
        let only_tag = |tags: &[RequestTag]| tags.len() == 1 && tags[0] == *tag;
        let mut ended = Vec::new();
        if let Some(queued) = &mut self.queued
            && queued.tags().contains(tag)
        {
            if only_tag(queued.tags()) {
                self.drop_queued();
            } else {
                queued.remove_tag(tag);
                ended.push(WorkspaceEventKind::CompileFinished {
                    tags: vec![tag.clone()],
                    success: false,
                    simulation_patterns: queued.simulation_patterns(),
                });
            }
        }
        if let Some(running) = &mut self.running_compile
            && running.operation.tags().contains(tag)
        {
            if only_tag(running.operation.tags()) {
                // The compile reports its end with this tag.
                running.cancel.cancel();
            } else {
                running.operation.remove_tag(tag);
                ended.push(WorkspaceEventKind::CompileFinished {
                    tags: vec![tag.clone()],
                    success: false,
                    simulation_patterns: running.operation.simulation_patterns(),
                });
            }
        }
        for simulation in self.simulations.values_mut() {
            if simulation.finished || !simulation.tags.contains(tag) {
                continue;
            }
            if only_tag(&simulation.tags) {
                // The simulation reports its end with this tag.
                simulation.cancel.cancel();
            } else {
                simulation.tags.retain(|other| other != tag);
                let TestCounts {
                    passed,
                    failed,
                    cancelled,
                } = simulation.counts;
                let unfinished = simulation.total.saturating_sub(passed + failed + cancelled);
                ended.push(WorkspaceEventKind::SimulationFinished {
                    tags: vec![tag.clone()],
                    counts: TestCounts {
                        passed,
                        failed,
                        cancelled: cancelled + unfinished,
                    },
                });
            }
        }
        for kind in ended {
            self.emit(kind);
        }
    }

    /// Drops the queued operation; its requests end without success.
    fn drop_queued(&mut self) {
        if let Some(queued) = self.queued.take() {
            self.emit(WorkspaceEventKind::CompileFinished {
                tags: queued.tags().to_vec(),
                success: false,
                simulation_patterns: queued.simulation_patterns(),
            });
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Compile
    // ---------------------------------------------------------------------------------------------

    /// Starts the compile of `operation`; no compile may be running.
    async fn start(&mut self, operation: Operation) {
        // The watcher may not have reported a file saved right before the request yet. Loading
        // the project again only reads changed files, so every operation does it.
        self.debouncer.take();
        self.reload(Change::Config).await;
        let Some(model) = self.model.clone() else {
            // The configuration never had a valid specification; its diagnostics explain why.
            self.emit(WorkspaceEventKind::CompileStarted {
                tags: operation.tags().to_vec(),
                total: 0,
            });
            self.emit(WorkspaceEventKind::CompileFinished {
                tags: operation.tags().to_vec(),
                success: false,
                simulation_patterns: operation.simulation_patterns(),
            });
            return;
        };
        let mut state = self.compile_state.take().unwrap_or_else(|| {
            tracing::error!("no compile state; loading it again");
            CompileState::load(&self.layout)
        });
        let cancel = self.operations_cancel.child_token();
        let (events, mut receiver) = mpsc::unbounded_channel();
        let compiled_model = Arc::clone(&model);
        let context = CompileContext {
            workspace_root: self.root.to_path_buf(),
            semaphore: Arc::clone(self.runtime.compile_semaphore()),
            events,
            cancel: cancel.clone(),
        };
        let layout = self.layout.clone();
        let runtime = self.runtime.clone();
        let internal = self.internal_sender.clone();
        tokio::spawn(async move {
            let mut done = DoneGuard {
                sender: internal.clone(),
                message: Some(Internal::CompileDone(None)),
            };
            let compile = async {
                let targets = compile::targets(&model.project, &model.discovery);
                let input = PlanInput {
                    layout: &layout,
                    project: &model.project,
                    targets: &targets,
                    compile_options: &model.spec.compile_options,
                    simulator: runtime.simulator(),
                };
                let report = compile::compile(&input, &mut state, &context).await;
                // Ends the forwarding below.
                drop(context);
                report
            };
            let forward = async {
                while let Some(event) = receiver.recv().await {
                    send_internal(&internal, Internal::Compile(event));
                }
            };
            let (report, ()) = tokio::join!(compile, forward);
            done.message = Some(Internal::CompileDone(Some(Box::new((state, report)))));
        });
        self.running_compile = Some(RunningCompile {
            operation,
            model: compiled_model,
            cancel,
        });
    }

    fn handle_compile_event(&mut self, event: CompileEvent) {
        let tags = self
            .running_compile
            .as_ref()
            .map(|running| running.operation.tags().to_vec())
            .unwrap_or_default();
        match event {
            CompileEvent::Started { total } => {
                self.emit(WorkspaceEventKind::CompileStarted { tags, total });
            },
            CompileEvent::FileStarted {
                key,
                library,
                file,
                index,
                total,
            } => {
                self.compile_diagnostics.insert(key, Vec::new());
                self.dirty.insert(DiagnosticSource::Compile);
                self.emit(WorkspaceEventKind::FileCompiling {
                    library,
                    file,
                    index,
                    total,
                });
            },
            CompileEvent::Diagnostic { key, diagnostic } => {
                self.compile_diagnostics
                    .entry(key)
                    .or_default()
                    .push(diagnostic);
                self.dirty.insert(DiagnosticSource::Compile);
            },
            CompileEvent::FileFinished {
                key,
                status,
                diagnostics,
            } => {
                if status == FileStatus::Skipped {
                    self.compile_diagnostics.remove(&key);
                } else {
                    self.compile_diagnostics.insert(key, diagnostics);
                }
                self.dirty.insert(DiagnosticSource::Compile);
            },
        }
    }

    async fn handle_compile_done(&mut self, result: Option<Box<(CompileState, CompileReport)>>) {
        let Some(running) = self.running_compile.take() else {
            tracing::error!("a compile ended that wasn't running");
            return;
        };
        let success = if let Some(result) = result {
            let (state, report) = *result;
            self.compile_state = Some(state);
            self.update_compile_diagnostics(&report);
            self.plan_diagnostics = report.project_diagnostics;
            report.status == CompileStatus::Succeeded && !running.cancel.is_cancelled()
        } else {
            tracing::error!(root = %self.root, "the compile task panicked");
            // The stored state may predate files the compile already analysed, which made their
            // dependents obsolete; only an empty state is safe, and recompiles everything.
            self.compile_state = Some(CompileState::default());
            self.plan_diagnostics = vec![Diagnostic::error("the compile failed unexpectedly")];
            false
        };
        self.dirty.insert(DiagnosticSource::Compile);
        self.dirty.insert(DiagnosticSource::Project);
        // Closing saves the parse cache too, but a crash would lose it.
        let loader = Arc::clone(&self.loader);
        tokio::task::spawn_blocking(move || lock_loader(&loader).save_cache());
        self.emit(WorkspaceEventKind::CompileFinished {
            tags: running.operation.tags().to_vec(),
            success,
            simulation_patterns: running.operation.simulation_patterns(),
        });
        if success && let Operation::Simulate { requests, tags } = running.operation {
            self.start_simulation(&running.model, &requests, tags);
        }
        if let Some(queued) = self.queued.take() {
            self.start(queued).await;
        }
    }

    /// Replaces the compile diagnostics with those of `report`.
    ///
    /// Files that weren't compiled keep their diagnostics: all files if the compile couldn't be
    /// planned, and the files that a cancel kept from starting. Their problems are still there.
    fn update_compile_diagnostics(&mut self, report: &CompileReport) {
        if report.files.is_empty() && report.status != CompileStatus::Succeeded {
            return;
        }
        let mut diagnostics = report.diagnostics.clone();
        for (key, status) in &report.files {
            if *status == FileStatus::NotStarted
                && let Some(previous) = self.compile_diagnostics.remove(key)
            {
                diagnostics.insert(key.clone(), previous);
            }
        }
        self.compile_diagnostics = diagnostics;
    }

    // ---------------------------------------------------------------------------------------------
    // Simulation
    // ---------------------------------------------------------------------------------------------

    /// Runs the testcases of `model` that match `requests`.
    fn start_simulation(
        &mut self,
        model: &Model,
        requests: &[SimulationRequest],
        tags: Vec<RequestTag>,
    ) {
        let plan = SimulationPlan::new(
            &SimulationInput {
                layout: &self.layout,
                project: &model.project,
                discovery: &model.discovery,
                sim_options: &model.spec.sim_options,
            },
            requests,
        );
        let id = self.next_simulation_id;
        self.next_simulation_id += 1;
        let cancel = self.operations_cancel.child_token();
        self.simulations.insert(
            id,
            RunningSimulation {
                tags,
                total: plan.tests.len(),
                finished: false,
                counts: TestCounts::default(),
                cancel: cancel.clone(),
            },
        );
        let internal = self.internal_sender.clone();
        // The events go straight into the actor's channel, so that the events of all
        // simulations stay in emission order.
        let events = SimulationEvents::new({
            let internal = internal.clone();
            move |event| send_internal(&internal, Internal::Simulation { id, event })
        });
        let context = SimulationContext {
            workspace_root: self.root.to_path_buf(),
            simulator: self.runtime.simulator().clone(),
            semaphore: Arc::clone(self.runtime.simulation_semaphore()),
            results: Arc::clone(&self.results),
            testcase_locks: Arc::clone(&self.testcase_locks),
            events,
            cancel,
        };
        tokio::spawn(async move {
            let _done = DoneGuard {
                sender: internal,
                message: Some(Internal::SimulationDone { id }),
            };
            let _report = runner::simulate(plan, &context).await;
        });
    }

    fn handle_simulation_event(&mut self, id: u64, event: SimulationEvent) {
        let Some(simulation) = self.simulations.get_mut(&id) else {
            tracing::error!(id, "an event of an unknown simulation");
            return;
        };
        match event {
            SimulationEvent::Started { testcases } => {
                let tags = simulation.tags.clone();
                if self.simulation_diagnostics.remove(&None).is_some() {
                    self.dirty.insert(DiagnosticSource::Simulation);
                }
                self.emit(WorkspaceEventKind::SimulationStarted { tags, testcases });
            },
            SimulationEvent::Diagnostic {
                testcase,
                diagnostic,
            } => {
                self.simulation_diagnostics
                    .entry(testcase)
                    .or_default()
                    .push(diagnostic);
                self.dirty.insert(DiagnosticSource::Simulation);
            },
            SimulationEvent::TestStarted { name, output_path } => {
                if self
                    .simulation_diagnostics
                    .remove(&Some(name.clone()))
                    .is_some()
                {
                    self.dirty.insert(DiagnosticSource::Simulation);
                }
                let tags = simulation.tags.clone();
                self.emit(WorkspaceEventKind::TestStarted {
                    tags,
                    name,
                    output_path,
                });
            },
            SimulationEvent::TestFinished {
                name,
                outcome,
                output_path,
                duration,
            } => {
                simulation.counts.record(outcome);
                let tags = simulation.tags.clone();
                self.results_dirty = true;
                self.emit(WorkspaceEventKind::TestFinished {
                    tags,
                    name,
                    outcome,
                    output_path,
                    duration,
                });
            },
            SimulationEvent::Finished { counts } => {
                simulation.finished = true;
                let tags = simulation.tags.clone();
                self.emit(WorkspaceEventKind::SimulationFinished { tags, counts });
            },
        }
    }

    fn handle_simulation_done(&mut self, id: u64) {
        let Some(simulation) = self.simulations.remove(&id) else {
            return;
        };
        if !simulation.finished {
            tracing::error!(root = %self.root, "a simulation task panicked");
            self.results_dirty = true;
            self.emit(WorkspaceEventKind::SimulationFinished {
                tags: simulation.tags,
                counts: simulation.counts,
            });
        }
    }

    async fn handle_internal(&mut self, message: Internal) {
        match message {
            Internal::Compile(event) => self.handle_compile_event(event),
            Internal::CompileDone(result) => self.handle_compile_done(result).await,
            Internal::Simulation { id, event } => self.handle_simulation_event(id, event),
            Internal::SimulationDone { id } => self.handle_simulation_done(id),
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Project
    // ---------------------------------------------------------------------------------------------

    fn handle_watch_event(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(event) => {
                if let Some(watcher) = &mut self.watcher {
                    if event.need_rescan() {
                        watcher.forget_all();
                    } else {
                        watcher.forget_removed(&event);
                    }
                }
                let change = if event.need_rescan() {
                    // Events were lost.
                    Some(Change::Config)
                } else {
                    let loader = self.loader();
                    classify(&event, loader.config_file(), self.layout.root())
                };
                if let Some(change) = change {
                    self.debouncer.add(change, Instant::now());
                }
            },
            Err(error) => {
                if !self.watch_failed {
                    self.watcher_diagnostics.push(watch_failure(&error));
                    self.dirty.insert(DiagnosticSource::Project);
                    self.watch_failed = true;
                }
            },
        }
    }

    /// Loads the project again, reading the configuration file only for [`Change::Config`].
    async fn reload(&mut self, change: Change) {
        let project_loader = Arc::clone(&self.loader);
        let read_config = change == Change::Config;
        let result =
            tokio::task::spawn_blocking(move || lock_loader(&project_loader).load(read_config))
                .await;
        match result {
            Ok(loaded) => self.apply_loaded(loaded),
            Err(error) => tracing::error!(%error, "loading the project failed"),
        }
    }

    fn apply_loaded(&mut self, loaded: Loaded) {
        if let Some(model) = loaded.model {
            let testcases = model.discovery.testcases();
            if testcases != self.testcases {
                self.testcases = testcases;
                self.testcases_dirty = true;
                let names: BTreeSet<&str> = self
                    .testcases
                    .iter()
                    .map(|testcase| testcase.name.as_str())
                    .collect();
                self.simulation_diagnostics.retain(|testcase, _| {
                    testcase.as_deref().is_none_or(|name| names.contains(name))
                });
                self.dirty.insert(DiagnosticSource::Simulation);
            }
            self.model = Some(model);
        }
        self.config_diagnostics = loaded.config_diagnostics;
        self.project_diagnostics = loaded.project_diagnostics;
        self.dirty.insert(DiagnosticSource::Config);
        self.dirty.insert(DiagnosticSource::Project);
        if let Some(watcher) = &mut self.watcher {
            for (dir, error) in watcher.update(&loaded.watch_targets) {
                if matches!(error.kind, notify::ErrorKind::PathNotFound) {
                    // Deleted in the meantime; its parent reports the deletion.
                    tracing::debug!(%dir, "the directory to watch is gone");
                    continue;
                }
                tracing::warn!(%dir, %error, "failed to watch");
                if !self.watch_failed {
                    self.watcher_diagnostics.push(watch_failure(&error));
                    self.watch_failed = true;
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Publishing
    // ---------------------------------------------------------------------------------------------

    fn diagnostics(&self, source: DiagnosticSource) -> Vec<Diagnostic> {
        match source {
            DiagnosticSource::Config => self.config_diagnostics.clone(),
            DiagnosticSource::Project => self
                .project_diagnostics
                .iter()
                .chain(&self.plan_diagnostics)
                .chain(&self.watcher_diagnostics)
                .cloned()
                .collect(),
            DiagnosticSource::Compile => self
                .compile_diagnostics
                .values()
                .flatten()
                .cloned()
                .collect(),
            DiagnosticSource::Simulation => self
                .simulation_diagnostics
                .values()
                .flatten()
                .cloned()
                .collect(),
        }
    }

    /// Publishes a new snapshot if anything changed, and sends the events for the changes.
    ///
    /// The snapshot is updated before the events are sent, so that a client reading it after an
    /// event never sees an older state.
    fn publish(&mut self) {
        let mut events = Vec::new();
        if self.testcases_dirty {
            self.testcases_dirty = false;
            if !self.testcases_sent || self.published.testcases != self.testcases {
                self.testcases_sent = true;
                self.published.testcases.clone_from(&self.testcases);
                events.push(WorkspaceEventKind::TestcasesChanged(self.testcases.clone()));
            }
        }
        for source in std::mem::take(&mut self.dirty) {
            let diagnostics = self.diagnostics(source);
            if diagnostics != self.published.diagnostics.get(source) {
                self.published.diagnostics.set(source, diagnostics.clone());
                events.push(WorkspaceEventKind::DiagnosticsChanged {
                    source,
                    diagnostics,
                });
            }
        }
        let mut changed = !events.is_empty();
        if self.results_dirty {
            self.results_dirty = false;
            let results = self.results.snapshot();
            if results != self.published.results {
                self.published.results = results;
                changed = true;
            }
        }
        if changed {
            self.snapshot.send_replace(Arc::new(self.published.clone()));
        }
        for kind in events {
            self.send(kind);
        }
    }

    fn send(&self, kind: WorkspaceEventKind) {
        let event = WorkspaceEvent {
            workspace_root: Arc::clone(&self.root),
            kind,
        };
        if self.events.send(event).is_err() {
            tracing::trace!("nobody listens to workspace events");
        }
    }

    /// Publishes pending changes, then sends `kind`.
    fn emit(&mut self, kind: WorkspaceEventKind) {
        self.publish();
        self.send(kind);
    }

    // ---------------------------------------------------------------------------------------------
    // Close
    // ---------------------------------------------------------------------------------------------

    fn finish_close(mut self) {
        self.publish();
        self.watcher = None;
        if let Some(state) = &self.compile_state
            && let Err(error) = state.save(&self.layout)
        {
            tracing::warn!(%error, "failed to save the compile state");
        }
        self.loader().save_cache();
        if let Some(lock) = self.lock.take() {
            lock.release();
        }
        tracing::debug!(root = %self.root, "workspace closed");
        if let Some(closing) = self.closing.take() {
            for reply in closing.replies {
                // The closing handle may have been dropped.
                let _dropped = reply.send(());
            }
        }
    }
}

fn lock_loader(loader: &Mutex<Loader>) -> MutexGuard<'_, Loader> {
    // A panic while loading leaves the loader usable: every load starts over.
    lock_unpoisoned(loader)
}

fn send_internal(sender: &mpsc::UnboundedSender<Internal>, message: Internal) {
    if sender.send(message).is_err() {
        tracing::trace!("the workspace actor is gone");
    }
}

fn watch_failure(error: &notify::Error) -> Diagnostic {
    Diagnostic::warning(format!(
        "watching the files failed ({error}); changes show up at the next compile or simulation"
    ))
}

/// Sends its message when dropped, also when the task panics.
struct DoneGuard {
    sender: mpsc::UnboundedSender<Internal>,
    message: Option<Internal>,
}

impl Drop for DoneGuard {
    fn drop(&mut self) {
        if let Some(message) = self.message.take() {
            send_internal(&self.sender, message);
        }
    }
}
