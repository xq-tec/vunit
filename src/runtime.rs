// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The resources shared by all workspaces of a process.
//!
//! They are the detected risim-runner and the analyzer, the limit on concurrent compile
//! processes, the budget of simulation threads, and the provider of the simulations' leases
//! ([`crate::threads`]).
//!
//! Every workspace of a process should be opened through the same [`Runtime`],
//! so that the limits apply across workspaces. An operation can lower them for its own
//! processes with a [`ProcessLimit`] or a [`SimulationLimit`](crate::threads::SimulationLimit).
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::num::NonZeroU32;
use std::num::NonZeroUsize;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;
use tokio::sync::AcquireError;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;

use crate::simulator::DetectError;
use crate::simulator::Simulator;
use crate::threads::LeaseProvider;
use crate::threads::ThreadBudget;
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
    /// The simulation threads of all workspaces together; a paused GUI simulation holds its
    /// threads.
    ///
    /// The caller may keep a clone, to resize it.
    pub threads: ThreadBudget,
    /// The most threads of the budget that one simulation takes: the threads that a simulation
    /// can use.
    ///
    /// A simulation's share of the free threads is capped by it ([`crate::threads`]).
    pub max_threads_per_test: NonZeroU32,
    /// The maximum number of compile processes running at once, over all workspaces.
    pub max_parallel_compiles: NonZeroUsize,
    /// Grants the leases of the simulations; [`NoLeases`](crate::threads::NoLeases) grants
    /// empty ones.
    pub leases: Arc<dyn LeaseProvider>,
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
    lock_owner: String,
    threads: ThreadBudget,
    max_threads_per_test: NonZeroU32,
    leases: Arc<dyn LeaseProvider>,
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
    /// Fails if risim-runner can't be run or its version can't be determined.
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
                lock_owner: options.lock_owner,
                threads: options.threads,
                max_threads_per_test: options.max_threads_per_test,
                leases: options.leases,
                compile_semaphore: Arc::new(Semaphore::new(options.max_parallel_compiles.get())),
            }),
        })
    }

    /// The detected risim-runner.
    pub fn simulator(&self) -> &Simulator {
        &self.inner.simulator
    }

    /// The owner record of the workspace locks ([`RuntimeOptions::lock_owner`]).
    pub(crate) fn lock_owner(&self) -> &str {
        &self.inner.lock_owner
    }

    /// The simulation threads of all workspaces ([`RuntimeOptions::threads`]).
    pub fn threads(&self) -> &ThreadBudget {
        &self.inner.threads
    }

    /// The most threads that one simulation takes ([`RuntimeOptions::max_threads_per_test`]).
    pub(crate) fn max_threads_per_test(&self) -> NonZeroU32 {
        self.inner.max_threads_per_test
    }

    /// Grants the leases of the simulations ([`RuntimeOptions::leases`]).
    pub(crate) fn leases(&self) -> &Arc<dyn LeaseProvider> {
        &self.inner.leases
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

/// Limits the concurrent compile processes of an operation: by the shared semaphore of the
/// [`Runtime`] and, optionally, by a semaphore of the operation; cheap to clone.
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
