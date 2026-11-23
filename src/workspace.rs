// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! An open workspace: a project, its compile and simulation operations, and its events.
//!
//! One actor task per workspace owns the project, the compile state and the results.
//! [`Workspace`] handles send it commands, and it publishes a [`Snapshot`] after every change
//! and a [`WorkspaceEvent`] for every step.
//!
//! - **Project:** loaded at open, again whenever the watcher reports a change of the
//!   configuration file or the sources, and again before every operation, so that a file saved
//!   right before a request is included even if the watcher hasn't reported it yet. Only changed
//!   files are read. The testcase list is maintained continuously; file changes never start a
//!   compile.
//! - **Operations:** a compile, or a simulate, which compiles first and then runs the matching
//!   testcases. At most one compile runs per workspace. A request arriving while a compile runs
//!   merges into the last queued operation, if there is one (see `operation.rs`), so at most one
//!   operation waits, except that simulates with different run options queue one behind the
//!   other. After its compile, a simulate runs its tests while the next operation may
//!   already compile.
//! - **Cancel:** [`Workspace::cancel_all`] cancels the running compile, the queued operations,
//!   and all running and waiting tests. [`Workspace::cancel`] cancels only what one request asked
//!   for: an operation that serves other requests too keeps running for them.
//! - **Output directory:** `risim-out/` may be deleted or replaced while the workspace is open.
//!   The watcher reports it, and every operation checks first that the lock file is still the
//!   locked one.
//!   The running compile and simulations are cancelled then, since their files are gone, and
//!   the layout is detached, so that nothing writes to the directory any more
//!   ([`OutputLayout::detach`]); a state file is never written into a missing `risim-out/`.
//!   The next operation waits until the directory has been quiet for [`OUTPUT_QUIET`] and the
//!   cancelled simulations ended.
//!   Then it locks the directory again and starts over like a newly opened workspace: it
//!   compiles everything, with the builtins extracted again, and the results are those that
//!   the directory has.
//!   Missing builtins alone are extracted again, and a missing workspace root isn't created.
//!   If the directory can't be set up, a project error says why, and the next operation tries
//!   again; if another process locked it, the workspace closes with
//!   [`WorkspaceEventKind::OutputTaken`].
//!   A workspace closed without its directory saves nothing.
//!
//! [`OUTPUT_QUIET`]: crate::watch::OUTPUT_QUIET
//!
//! AI NOTICE: Generated, minimally reviewed.

mod actor;
mod loader;
mod operation;

use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroU32;
use std::num::NonZeroUsize;
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
use self::loader::ProjectConfig;
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
use crate::store;
use crate::store::CompileState;
use crate::store::LockError;
use crate::store::OutputLayout;
use crate::store::OutputLock;
use crate::store::ResultStore;
use crate::store::TestCounts;
use crate::store::TestOutcome;
use crate::store::TestResult;
use crate::threads::ThreadState;

/// Where the project of a workspace comes from.
#[expect(
    clippy::large_enum_variant,
    reason = "a workspace has one project source, which is moved rarely"
)]
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
        /// The owner record of the process that holds the lock, if it wrote one (see
        /// [`RuntimeOptions::lock_owner`](crate::RuntimeOptions::lock_owner)).
        owner: Option<String>,
    },
    /// `risim-out/` can't be set up.
    #[error("failed to set up {path}")]
    Io {
        /// The file or directory that couldn't be set up.
        path: Utf8PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// The configuration file can't be read at all.
    #[error(transparent)]
    Config(#[from] config::ReadError),
}

impl From<LockError> for OpenError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Locked { path, owner } => Self::Locked { path, owner },
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

/// What happened in a workspace.
///
/// The snapshot is updated before the event is sent.
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
        /// The 1-based count of files whose compilation has started, including this one.
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
    /// A testcase waits for threads, or its simulation's threads changed
    /// ([`SimulationEvent::TestThreads`](crate::runner::SimulationEvent::TestThreads)).
    TestThreads {
        /// The tags of the simulate operation.
        tags: Vec<RequestTag>,
        /// The testcase name.
        name: String,
        /// What its simulation does with its threads.
        state: ThreadState,
    },
    /// The simulator of a testcase is about to be spawned.
    TestStarted {
        /// The tags of the simulate operation.
        tags: Vec<RequestTag>,
        /// The testcase name.
        name: String,
        /// The simulator output file.
        output_path: Utf8PathBuf,
        /// Whether the simulation starts paused: the effective GUI mode of the testcase, after
        /// merging the requests of the operation and after the rule that a GUI pattern
        /// matching several testcases runs them without GUI mode.
        gui: bool,
    },
    /// A testcase finished, failed to start, or was cancelled.
    TestFinished {
        /// The tags of the simulate operation.
        tags: Vec<RequestTag>,
        /// The testcase name.
        name: String,
        /// How it ended.
        outcome: TestOutcome,
        /// The simulator output file (`output.txt`). For a test cancelled before it started,
        /// the file belongs to an earlier run, if any.
        output_path: Utf8PathBuf,
        /// How long the simulator ran.
        ///
        /// Zero if it never started.
        duration: Duration,
    },
    /// All testcases of a simulate operation are done.
    SimulationFinished {
        /// The tags of the operation.
        tags: Vec<RequestTag>,
        /// The number of testcases per outcome.
        counts: TestCounts,
    },
    /// Another process locked `risim-out/` after it was deleted or replaced, so the workspace
    /// closed; this is its last event.
    ///
    /// The fields are those of [`OpenError::Locked`], which opening the workspace again would
    /// return.
    OutputTaken {
        /// The lock file.
        lock: Utf8PathBuf,
        /// The owner record of the process that holds the lock, if it wrote one.
        owner: Option<String>,
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
    ///
    /// `max_parallel` limits the compile processes of the request, in addition to the limit of
    /// the [`Runtime`]. Merged requests use the smallest limit.
    pub fn compile(&self, tag: Option<RequestTag>, max_parallel: Option<NonZeroUsize>) {
        self.send(Command::Compile { tag, max_parallel });
    }

    /// Compiles, then runs the testcases matching `requests`.
    ///
    /// `max_parallel` limits the compile and simulation processes of the request, and `threads`
    /// the simulation threads of its simulations together (the operation's thread cap), in
    /// addition to the limits of the [`Runtime`]. Merged requests use the smallest limits.
    pub fn simulate(
        &self,
        requests: Vec<SimulationRequest>,
        tag: Option<RequestTag>,
        max_parallel: Option<NonZeroUsize>,
        threads: Option<NonZeroU32>,
    ) {
        self.send(Command::Simulate {
            requests,
            tag,
            max_parallel,
            threads,
        });
    }

    /// Cancels the running compile, the queued operations, and all running and waiting tests.
    pub fn cancel_all(&self) {
        self.send(Command::CancelAll);
    }

    /// Cancels what the request with `tag` asked for.
    ///
    /// An operation whose only tag is `tag` is cancelled like by [`cancel_all`](Self::cancel_all).
    /// An operation that has other tags too keeps running for them and loses `tag`; for `tag`,
    /// the operation ends right away: with
    /// [`CompileFinished`](WorkspaceEventKind::CompileFinished) without success during its compile,
    /// or with [`SimulationFinished`](WorkspaceEventKind::SimulationFinished) counting its
    /// unfinished tests as cancelled afterwards. Operations without `tag` aren't affected.
    pub fn cancel(&self, tag: RequestTag) {
        self.send(Command::Cancel(tag));
    }

    /// Cancels everything, waits for the processes to end, saves the state and releases the
    /// lock. Other handles of the workspace stop working.
    ///
    /// A workspace that lost `risim-out/` saves nothing, and only unlocks its lock.
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
    let source = match source {
        ProjectSource::ConfigFile(path) => ProjectSource::ConfigFile(absolute(&root.join(path))?),
        ProjectSource::Spec(spec) => ProjectSource::Spec(spec),
    };
    let config = ProjectConfig::new(source, runtime.check_run_options());
    let prepare_root = root.clone();
    let owner = runtime.lock_owner().to_owned();
    let opened = tokio::task::spawn_blocking(move || prepare(prepare_root, layout, config, &owner))
        .await
        .map_err(|error| OpenError::Io {
            path: root.clone(),
            source: io::Error::other(error),
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

/// Reads the configuration, locks `risim-out/` for `owner`, extracts the builtins, loads the
/// stored state and loads the project.
fn prepare(
    root: Utf8PathBuf,
    layout: OutputLayout,
    mut config: ProjectConfig,
    owner: &str,
) -> Result<Opened, OpenError> {
    config.read()?;
    let (lock, _previous) = OutputLock::acquire(&layout, owner)?;
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
    let cache = SourceCache::load_persisted(&layout.parse_cache_file());
    let mut loader = Loader::new(root, layout.clone(), config, builtins_dir, cache);
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

/// `risim-out/`, opened again after it was deleted or replaced while the workspace was open.
struct Reopened {
    lock: OutputLock,
    results: ResultStore,
}

/// Locks `risim-out/` again for `owner`, starts its compile state over, extracts the builtins
/// and [attaches](OutputLayout::attach) `layout`.
///
/// What is left of the directory, or what the operations that ran without the lock wrote to a
/// new one, doesn't match the compile state.
/// The state file and the libraries are deleted, so that the next compile compiles everything.
/// The test results are read from the directory, as on open.
/// A workspace root that is gone isn't created again.
fn reopen_output(layout: &OutputLayout, owner: &str) -> Result<Reopened, OpenError> {
    if let Some(workspace_root) = layout.root().parent()
        && !workspace_root.is_dir()
    {
        return Err(OpenError::Io {
            path: workspace_root.to_owned(),
            source: io::Error::new(io::ErrorKind::NotFound, "the workspace root is gone"),
        });
    }
    let (lock, _previous) = OutputLock::acquire(layout, owner)?;
    let set_up = || {
        for stale in [layout.state_file(), layout.libraries_dir()] {
            store::remove_path(&stale).map_err(|source| OpenError::Io {
                path: stale.clone(),
                source,
            })?;
        }
        let builtins_root = layout.builtins_root();
        builtins::extract(&builtins_root).map_err(|source| OpenError::Io {
            path: builtins_root,
            source,
        })
    };
    if let Err(error) = set_up() {
        lock.release();
        return Err(error);
    }
    layout.attach();
    Ok(Reopened {
        lock,
        results: ResultStore::load(layout),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::TempRoot;

    #[test]
    fn reopening_starts_the_output_over() {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        let (lost, _) = OutputLock::acquire(&layout, "first").unwrap();
        // What the operations without the lock wrote to a recreated directory.
        fs::remove_dir_all(layout.root()).unwrap();
        fs::create_dir_all(layout.root()).unwrap();
        CompileState::default().save(&layout).unwrap();
        let archive = temp.write("risim-out/libraries/lib/lib.hlib", "");
        let output = temp.write("risim-out/compile_output/lib/a.txt", "");
        layout.detach();

        let Reopened { lock, results } = reopen_output(&layout, "second").unwrap();
        lost.abandon();
        assert!(lock.is_current());
        assert!(layout.is_writable());
        assert_eq!(fs::read_to_string(layout.owner_file()).unwrap(), "second");
        assert!(!layout.state_file().exists());
        assert!(!archive.exists());
        assert!(output.exists());
        assert!(layout.builtins_root().join(builtins::hash()).is_dir());
        assert!(results.snapshot().is_empty());

        // Locked by another process in the meantime.
        let error = reopen_output(&layout, "third").err();
        assert!(
            matches!(&error, Some(OpenError::Locked { owner: Some(owner), .. }) if owner == "second"),
            "{error:?}"
        );
        lock.release();

        // The whole workspace was deleted: it isn't created again.
        let gone = TempRoot::new();
        let gone_layout = OutputLayout::new(&gone.root.join("workspace"));
        let gone_error = reopen_output(&gone_layout, "fourth").err();
        assert!(
            matches!(&gone_error, Some(OpenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound),
            "{gone_error:?}"
        );
        assert!(!gone.root.join("workspace").exists());
    }
}
