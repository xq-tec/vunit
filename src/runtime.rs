// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The resources shared by all workspaces of a process: the detected risim-runner and the analyzer,
//! the run options of every simulation, and the limits on concurrent compile and simulation
//! processes.
//!
//! Every workspace of a process should be opened through the same [`Runtime`],
//! so that the limits apply across workspaces. An operation can lower them for its own
//! processes with a [`ProcessLimit`].
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::num::NonZeroUsize;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;
use tokio::sync::AcquireError;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;

use crate::simulator::CheckRunOptions;
use crate::simulator::DetectError;
use crate::simulator::RunOptions;
use crate::simulator::Simulator;
use crate::workspace;
use crate::workspace::OpenError;
use crate::workspace::ProjectSource;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceEvent;

/// The settings of a [`Runtime`].
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// The analyzer, the riSim CLI `risim`, which compiles (`risim analyze`).
    pub analyzer: Utf8PathBuf,
    /// The risim-runner executable, which simulates (`risim-runner run`).
    pub runner: Utf8PathBuf,
    /// The run options of every simulation, unless the run options of its configuration set them
    /// ([`Simulator::simulate_command`]), such as the defaults that the application takes from its
    /// environment.
    pub run_defaults: RunOptions,
    /// Checks the run options of the `[run]` table of `risim-config.toml`, one key at a time;
    /// an error becomes a configuration diagnostic, and the key is left out
    /// ([`config::load`](crate::config::load)).
    ///
    /// `None` takes them unchecked, for the runner to check.
    pub check_run_options: Option<CheckRunOptions>,
    /// The maximum number of simulations running at once, over all workspaces. A paused GUI
    /// simulation counts as running.
    pub max_parallel_simulations: NonZeroUsize,
    /// The maximum number of compile processes running at once, over all workspaces.
    pub max_parallel_compiles: NonZeroUsize,
    /// The owner record of this process, an opaque text (by convention a JSON object), which
    /// every open workspace writes to `risim-out/owner.json` while it holds the lock.
    ///
    /// Another process that finds the workspace locked gets it in [`OpenError::Locked`].
    pub lock_owner: String,
}

/// The [`Runtime`] can't be created.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// risim-runner can't be run, or its version can't be determined.
    #[error(transparent)]
    Simulator(#[from] DetectError),
}

#[derive(Debug)]
struct Inner {
    simulator: Simulator,
    run_defaults: Arc<RunOptions>,
    check_run_options: Option<CheckRunOptions>,
    lock_owner: String,
    simulation_semaphore: Arc<Semaphore>,
    compile_semaphore: Arc<Semaphore>,
}

/// The resources shared by all workspaces of a process; cheap to clone.
#[derive(Debug, Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    /// Detects the risim-runner version (`--version`).
    ///
    /// # Errors
    ///
    /// Fails if risim-runner can't be run, its version can't be determined, or it reads another
    /// format of run options ([`Simulator::detect`]).
    pub async fn new(options: RuntimeOptions) -> Result<Self, RuntimeError> {
        let simulator = Simulator::detect(&options.runner, &options.analyzer).await?;
        tracing::info!(
            path = %simulator.path(),
            analyzer = %simulator.analyzer(),
            version = ?simulator.version(),
            "detected risim-runner"
        );
        Ok(Self {
            inner: Arc::new(Inner {
                simulator,
                run_defaults: Arc::new(options.run_defaults),
                check_run_options: options.check_run_options,
                lock_owner: options.lock_owner,
                simulation_semaphore: Arc::new(Semaphore::new(
                    options.max_parallel_simulations.get(),
                )),
                compile_semaphore: Arc::new(Semaphore::new(options.max_parallel_compiles.get())),
            }),
        })
    }

    /// The detected risim-runner.
    pub fn simulator(&self) -> &Simulator {
        &self.inner.simulator
    }

    /// The run options of every simulation ([`RuntimeOptions::run_defaults`]).
    pub(crate) fn run_defaults(&self) -> &Arc<RunOptions> {
        &self.inner.run_defaults
    }

    /// The check of the run options of `risim-config.toml`
    /// ([`RuntimeOptions::check_run_options`]).
    pub(crate) fn check_run_options(&self) -> Option<CheckRunOptions> {
        self.inner.check_run_options
    }

    /// The owner record of the workspace locks ([`RuntimeOptions::lock_owner`]).
    pub(crate) fn lock_owner(&self) -> &str {
        &self.inner.lock_owner
    }

    /// Limits the number of concurrent simulations.
    pub(crate) fn simulation_semaphore(&self) -> &Arc<Semaphore> {
        &self.inner.simulation_semaphore
    }

    /// The shared limit on concurrent simulations
    /// ([`RuntimeOptions::max_parallel_simulations`]), for a simulation that the process runs
    /// outside of a workspace, so that it shares the limit with the testcases.
    pub fn simulation_limit(&self) -> ProcessLimit {
        ProcessLimit::from(Arc::clone(&self.inner.simulation_semaphore))
    }

    /// Limits the number of concurrent compile processes.
    pub(crate) fn compile_semaphore(&self) -> &Arc<Semaphore> {
        &self.inner.compile_semaphore
    }

    /// Opens the workspace at `root`: acquires `risim-out/.lock` and writes the owner record,
    /// loads the stored state, loads the project and starts watching its files.
    ///
    /// Events are delivered losslessly through `events`, starting with the testcases and the
    /// diagnostics of the loaded project.
    ///
    /// # Errors
    ///
    /// Fails if another process holds the lock, `risim-out/` can't be set up, or the
    /// configuration file can't be read at all. A configuration with errors still opens and
    /// reports them as diagnostics.
    pub async fn open_workspace(
        &self,
        root: &Utf8Path,
        source: ProjectSource,
        events: mpsc::UnboundedSender<WorkspaceEvent>,
    ) -> Result<Workspace, OpenError> {
        workspace::open(self.clone(), root, source, events).await
    }
}

/// Limits the concurrent processes of an operation: by the shared semaphore of the [`Runtime`]
/// and, optionally, by a semaphore of the operation; cheap to clone.
#[derive(Debug, Clone)]
pub struct ProcessLimit {
    shared: Arc<Semaphore>,
    operation: Option<Arc<Semaphore>>,
}

/// Permission to run a process; released when dropped.
#[derive(Debug)]
pub struct ProcessPermit {
    _operation: Option<OwnedSemaphorePermit>,
    _shared: OwnedSemaphorePermit,
}

impl ProcessLimit {
    /// A limit by `shared` and, if given, by `operation` too.
    pub const fn new(shared: Arc<Semaphore>, operation: Option<Arc<Semaphore>>) -> Self {
        Self { shared, operation }
    }

    /// Waits for a permit of both semaphores.
    ///
    /// The permit of the operation comes first, so that a process waiting for it doesn't hold a
    /// shared permit that other operations could use.
    ///
    /// # Errors
    ///
    /// Fails if a semaphore is closed.
    pub async fn acquire(&self) -> Result<ProcessPermit, AcquireError> {
        let operation = match &self.operation {
            Some(semaphore) => Some(Arc::clone(semaphore).acquire_owned().await?),
            None => None,
        };
        let shared = Arc::clone(&self.shared).acquire_owned().await?;
        Ok(ProcessPermit {
            _operation: operation,
            _shared: shared,
        })
    }
}

impl From<Arc<Semaphore>> for ProcessLimit {
    /// A limit by `shared` alone.
    fn from(shared: Arc<Semaphore>) -> Self {
        Self::new(shared, None)
    }
}
