// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The resources shared by all workspaces of a process: the detected risim-ghdl and the limits
//! on concurrent compile and simulation processes.
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

use crate::simulator::DetectError;
use crate::simulator::Simulator;
use crate::workspace;
use crate::workspace::OpenError;
use crate::workspace::ProjectSource;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceEvent;

/// The settings of a [`Runtime`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// The risim-ghdl executable.
    pub risim_ghdl: Utf8PathBuf,
    /// The maximum number of simulations running at once, over all workspaces. A paused GUI
    /// simulation counts as running.
    pub max_parallel_simulations: NonZeroUsize,
    /// The maximum number of compile processes running at once, over all workspaces.
    pub max_parallel_compiles: NonZeroUsize,
}

/// The [`Runtime`] can't be created.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// risim-ghdl can't be run, or its version can't be determined.
    #[error(transparent)]
    Simulator(#[from] DetectError),
}

#[derive(Debug)]
struct Inner {
    simulator: Simulator,
    simulation_semaphore: Arc<Semaphore>,
    compile_semaphore: Arc<Semaphore>,
}

/// The resources shared by all workspaces of a process; cheap to clone.
#[derive(Debug, Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    /// Detects the risim-ghdl version (`--version`).
    ///
    /// # Errors
    ///
    /// Fails if risim-ghdl can't be run or its version can't be determined.
    pub async fn new(options: RuntimeOptions) -> Result<Self, RuntimeError> {
        let simulator = Simulator::detect(&options.risim_ghdl).await?;
        tracing::info!(
            path = %simulator.path(),
            version = ?simulator.version(),
            "detected risim-ghdl"
        );
        Ok(Self {
            inner: Arc::new(Inner {
                simulator,
                simulation_semaphore: Arc::new(Semaphore::new(
                    options.max_parallel_simulations.get(),
                )),
                compile_semaphore: Arc::new(Semaphore::new(options.max_parallel_compiles.get())),
            }),
        })
    }

    /// The detected risim-ghdl.
    pub fn simulator(&self) -> &Simulator {
        &self.inner.simulator
    }

    /// Limits the number of concurrent simulations.
    pub(crate) fn simulation_semaphore(&self) -> &Arc<Semaphore> {
        &self.inner.simulation_semaphore
    }

    /// Limits the number of concurrent compile processes.
    pub(crate) fn compile_semaphore(&self) -> &Arc<Semaphore> {
        &self.inner.compile_semaphore
    }

    /// Opens the workspace at `root`: acquires `risim-out/.lock`, loads the stored state, loads
    /// the project and starts watching its files.
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
