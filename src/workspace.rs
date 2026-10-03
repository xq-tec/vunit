// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! An open workspace: a project, its compile and simulation operations, and its events.
//!
//! Replaces event-cache's `tb_manager.rs` (`PendingAction`). One actor task per workspace owns
//! the project, the compile state and the results. [`Workspace`] handles send it commands, and
//! it publishes a [`Snapshot`] after every change and a [`WorkspaceEvent`] for every step.
//!
//! - **Project:** loaded at open, again whenever the watcher reports a change of the
//!   configuration file or the sources, and again before every operation, so that a file saved
//!   right before a request is included even if the watcher hasn't reported it yet. Only changed
//!   files are read. The testcase list is maintained continuously; file changes never start a
//!   compile.
//! - **Operations:** a compile, or a simulate, which compiles first and then runs the matching
//!   testcases. At most one compile runs per workspace. Requests arriving while a compile runs
//!   merge into one queued operation (see `operation.rs`). After its compile, a simulate runs its
//!   tests while the next operation may already compile.
//! - **Cancel:** [`Workspace::cancel_all`] cancels the running compile, the queued operation, and
//!   all running and waiting tests.
//!
//! AI NOTICE: Generated, minimally reviewed.

mod actor;
mod loader;
mod operation;

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;

use self::actor::Actor;
use self::actor::Command;
use self::loader::Loader;
use crate::builtins;
use crate::config;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::DiagnosticSource;
use crate::discovery::Testcase;
pub use crate::runner::SimulationRequest;
use crate::runtime::Runtime;
use crate::sources;
use crate::sources::SourceCache;
use crate::spec::ProjectSpec;
use crate::store::CompileState;
use crate::store::LockError;
use crate::store::OutputLayout;
use crate::store::OutputLock;
use crate::store::ResultStore;
use crate::store::TestOutcome;
use crate::store::TestResult;

/// Where the project of a workspace comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSource {
    /// A `risim-config.toml`, relative to the workspace root or absolute. The file is watched
    /// and read again when it changes.
    ConfigFile(Utf8PathBuf),
    /// A fixed project; only its source files are watched.
    Spec(ProjectSpec),
}

/// An opaque tag of a request, echoed in the events of its operation; clients use it for their
/// request IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestTag(pub String);

/// A workspace can't be opened.
#[derive(Debug, Error)]
pub enum OpenError {
    /// Another process holds the lock on `risim-out/`.
    #[error("{path} is locked by another process")]
    Locked {
        /// The lock file.
        path: Utf8PathBuf,
    },
    /// `risim-out/` can't be set up.
    #[error("failed to set up {path}: {source}")]
    Io {
        /// The file or directory that couldn't be set up.
        path: Utf8PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// The configuration file can't be read at all.
    #[error("{}: {}", .0, .0.source)]
    Config(#[from] config::ReadError),
}

impl From<LockError> for OpenError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Locked { path } => Self::Locked { path },
            LockError::Io { path, source } => Self::Io { path, source },
        }
    }
}

/// The diagnostics of a workspace, one set per [`DiagnosticSource`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticSets {
    sets: BTreeMap<DiagnosticSource, Vec<Diagnostic>>,
}

impl DiagnosticSets {
    /// The diagnostics of `source`.
    pub fn get(&self, source: DiagnosticSource) -> &[Diagnostic] {
        self.sets.get(&source).map_or(&[], Vec::as_slice)
    }

    /// All sets, including empty ones, in the order of [`DiagnosticSource::ALL`].
    pub fn iter(&self) -> impl Iterator<Item = (DiagnosticSource, &[Diagnostic])> {
        DiagnosticSource::ALL
            .into_iter()
            .map(|source| (source, self.get(source)))
    }

    fn set(&mut self, source: DiagnosticSource, diagnostics: Vec<Diagnostic>) {
        self.sets.insert(source, diagnostics);
    }
}

/// The state of a workspace at one point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The testcases, in run order.
    pub testcases: Vec<Testcase>,
    /// The diagnostics.
    pub diagnostics: DiagnosticSets,
    /// The last result of every testcase that has run.
    pub results: BTreeMap<String, TestResult>,
}

/// Something that happened in a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEvent {
    /// The root of the workspace.
    pub workspace_root: Arc<Utf8Path>,
    /// What happened.
    pub kind: WorkspaceEventKind,
}

/// What happened in a workspace. The snapshot is updated before the event is sent.
///
/// A simulate operation whose compile phase fails or is cancelled ends with
/// [`CompileFinished`](Self::CompileFinished) without success, and sends neither
/// [`SimulationStarted`](Self::SimulationStarted) nor
/// [`SimulationFinished`](Self::SimulationFinished). A queued operation dropped by a cancel
/// ends with `CompileFinished` without success and without `CompileStarted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceEventKind {
    /// The testcase list was loaded, or changed.
    TestcasesChanged(Vec<Testcase>),
    /// The diagnostics of `source` changed; they replace that set only.
    DiagnosticsChanged {
        /// The set that changed.
        source: DiagnosticSource,
        /// All diagnostics of the set.
        diagnostics: Vec<Diagnostic>,
    },
    /// A compile determined its compile set.
    CompileStarted {
        /// The tags of the operation.
        tags: Vec<RequestTag>,
        /// The number of files to compile.
        total: usize,
    },
    /// A compile process is about to start.
    FileCompiling {
        /// The library name.
        library: String,
        /// The source file.
        file: Utf8PathBuf,
        /// The 1-based number of this file among the files compiled so far.
        index: usize,
        /// The number of files to compile.
        total: usize,
    },
    /// A compile ended, including the compile phase of a simulate.
    CompileFinished {
        /// The tags of the operation.
        tags: Vec<RequestTag>,
        /// Whether every file of the compile set is compiled.
        success: bool,
        /// The patterns of a simulate operation; `None` for a compile operation.
        simulation_patterns: Option<Vec<String>>,
    },
    /// A simulate operation resolved its patterns.
    SimulationStarted {
        /// The tags of the operation.
        tags: Vec<RequestTag>,
        /// The testcases to run, sorted.
        testcases: Vec<String>,
    },
    /// The simulator of a testcase is about to be spawned.
    TestStarted {
        /// The testcase name.
        name: String,
        /// The simulator output file.
        output_path: Utf8PathBuf,
    },
    /// A testcase finished, failed to start, or was cancelled.
    TestFinished {
        /// The testcase name.
        name: String,
        /// How it ended.
        outcome: TestOutcome,
        /// The simulator output file.
        output_path: Utf8PathBuf,
        /// How long the simulator ran.
        duration: Duration,
    },
    /// All testcases of a simulate operation are done.
    SimulationFinished {
        /// The tags of the operation.
        tags: Vec<RequestTag>,
        /// The number of passed testcases.
        passed: usize,
        /// The number of failed testcases.
        failed: usize,
        /// The number of cancelled testcases.
        cancelled: usize,
    },
}

/// A handle to an open workspace; cheap to clone.
///
/// Dropping all handles doesn't cancel anything: the workspace finishes its operations and then
/// closes itself. [`close`](Self::close) cancels them and closes it right away.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: Arc<Utf8Path>,
    commands: mpsc::UnboundedSender<Command>,
    snapshot: watch::Receiver<Arc<Snapshot>>,
}

impl Workspace {
    /// The workspace root.
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// The current state.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshot.borrow())
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::debug!(root = %self.root, "the workspace is closed");
        }
    }

    /// Compiles the files that the testbenches need.
    pub fn compile(&self, tag: Option<RequestTag>) {
        self.send(Command::Compile { tag });
    }

    /// Compiles, then runs the testcases matching `requests`.
    pub fn simulate(&self, requests: Vec<SimulationRequest>, tag: Option<RequestTag>) {
        self.send(Command::Simulate { requests, tag });
    }

    /// Cancels the running compile, the queued operation, and all running and waiting tests.
    pub fn cancel_all(&self) {
        self.send(Command::CancelAll);
    }

    /// Cancels everything, waits for the processes to end, saves the state and releases the
    /// lock. Other handles of the workspace stop working.
    pub async fn close(self) {
        let (reply, done) = oneshot::channel();
        self.send(Command::Close(reply));
        // An error means that the workspace is already closed.
        let _closed = done.await;
    }
}

/// Opens a workspace (see [`Runtime::open_workspace`]).
pub(crate) async fn open(
    runtime: Runtime,
    root: &Utf8Path,
    source: ProjectSource,
    events: mpsc::UnboundedSender<WorkspaceEvent>,
) -> Result<Workspace, OpenError> {
    let absolute = |path: &Utf8Path| {
        camino::absolute_utf8(path)
            .map(|path| sources::normalize(&path))
            .map_err(|error| OpenError::Io {
                path: path.to_owned(),
                source: error,
            })
    };
    let root = absolute(root)?;
    if !root.is_dir() {
        return Err(OpenError::Io {
            path: root,
            source: io::Error::new(
                io::ErrorKind::NotFound,
                "the workspace root isn't a directory",
            ),
        });
    }
    let layout = OutputLayout::new(&root);
    let loader = match source {
        ProjectSource::ConfigFile(path) => {
            let path = absolute(&root.join(path))?;
            Loader::new(root.clone(), layout.clone(), Some(path), None)
        },
        ProjectSource::Spec(spec) => Loader::new(root.clone(), layout.clone(), None, Some(spec)),
    };
    let opened = tokio::task::spawn_blocking(move || prepare(layout, loader))
        .await
        .map_err(|error| OpenError::Io {
            path: root.clone(),
            source: io::Error::other(error.to_string()),
        })??;

    let root: Arc<Utf8Path> = Arc::from(root.as_path());
    let (commands, command_receiver) = mpsc::unbounded_channel();
    let (actor, snapshot) =
        Actor::new(runtime, Arc::clone(&root), opened, command_receiver, events);
    tokio::spawn(actor.run());
    Ok(Workspace {
        root,
        commands,
        snapshot,
    })
}

/// The state of a workspace that was just opened.
struct Opened {
    layout: OutputLayout,
    lock: OutputLock,
    loader: Loader,
    loaded: loader::Loaded,
    compile_state: CompileState,
    results: ResultStore,
}

/// Reads the configuration, locks `risim-out/`, extracts the builtins, loads the stored state
/// and loads the project.
fn prepare(layout: OutputLayout, mut loader: Loader) -> Result<Opened, OpenError> {
    loader.read_config()?;
    let (lock, _previous) = OutputLock::acquire(&layout)?;
    let builtins_root = layout.builtins_root();
    let builtins_dir = match builtins::extract(&builtins_root) {
        Ok(dir) => dir,
        Err(source) => {
            lock.release();
            return Err(OpenError::Io {
                path: builtins_root,
                source,
            });
        },
    };
    loader.prepare(
        builtins_dir,
        SourceCache::load_persisted(&layout.parse_cache_file()),
    );
    let initial = loader.load(false);
    let compile_state = CompileState::load(&layout);
    let results = ResultStore::load(&layout);
    if let Some(model) = &initial.model {
        let names = model
            .discovery
            .runs
            .iter()
            .map(|run| run.testcase.name.as_str());
        if let Err(error) = results.retain_testcases(names) {
            tracing::warn!(%error, "failed to save the test results");
        }
    }
    Ok(Opened {
        layout,
        lock,
        loader,
        loaded: initial,
        compile_state,
        results,
    })
}
