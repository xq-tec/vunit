// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The simulation threads: the budget shared by all workspaces of a process, the limits of an
//! operation, and the leases that the owner of the [`Runtime`](crate::Runtime) grants to each
//! simulation.
//!
//! - **Budget:** [`ThreadBudget`] is a resizable semaphore of threads.
//!   A simulation takes one thread, waiting for it if needed, and then up to its share without
//!   waiting: the free threads divided by the tests of its operation that haven't started yet,
//!   but at least one, and at most the threads that a simulation can use
//!   ([`RuntimeOptions::max_threads_per_test`](crate::RuntimeOptions::max_threads_per_test)).
//!   So 5 tests on 10 threads get 2 each, and 100 tests get 1 each, if they can use them.
//!   A GUI simulation takes one thread, since it starts paused.
//! - **Resizing:** growing the budget frees the new threads at once; shrinking it takes the free
//!   threads at once and the others as their simulations end, so that the threads in use drain
//!   to the new size.
//! - **Operation:** [`SimulationLimit`] adds the limits of an operation: its number of processes
//!   (`max_parallel`) and its thread cap, a [`ThreadBudget`] of its own.
//! - **Leases:** before spawning the simulator, a test asks the [`LeaseProvider`] of the runtime
//!   for a [`Lease`] of its threads: run options of the simulator, and a guard that is kept until the
//!   simulator exits.
//!   The provider may wait, and reports what the test waits for through [`LeaseEvents`].
//!   [`NoLeases`] grants empty leases at once.
//!
//! Waits are reported as [`SimulationEvent::TestThreads`]: a test that doesn't get a thread of
//! the budget at once reports [`WaitReason::Budget`], for itself and the tests of its simulation
//! queued behind it, once per test; the provider reports the other states.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::AcquireError;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;

use crate::runner::SimulationEvent;
use crate::runner::SimulationEvents;
use crate::simulator::RunOptions;
use crate::sync::lock_unpoisoned;

// -------------------------------------------------------------------------------------------------
// States
// -------------------------------------------------------------------------------------------------

/// What the simulation of a test does with its threads, as [`SimulationEvent::TestThreads`]
/// reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadState {
    /// The test waits for threads.
    Waiting(WaitReason),
    /// The simulation runs with `threads` threads.
    Running {
        /// The number of threads.
        threads: u32,
    },
}

/// Why a test waits for threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitReason {
    /// All threads of the [`ThreadBudget`] of the runtime are in use.
    Budget {
        /// The size of the budget.
        threads: u32,
    },
    /// All threads of the machine are in use, by the simulations of all processes; reported by
    /// the [`LeaseProvider`].
    MachineSlots {
        /// The threads of the machine.
        threads: u32,
    },
    /// All threads of a pool shared by several machines are in use; reported by the
    /// [`LeaseProvider`].
    FloatingPool {
        /// The size of the pool.
        pool: u32,
    },
}

// -------------------------------------------------------------------------------------------------
// Budget
// -------------------------------------------------------------------------------------------------

/// A resizable number of simulation threads; cheap to clone, and clones share the threads.
#[derive(Debug, Clone)]
pub struct ThreadBudget {
    inner: Arc<BudgetInner>,
}

#[derive(Debug)]
struct BudgetInner {
    semaphore: Arc<Semaphore>,
    state: Mutex<BudgetState>,
}

#[derive(Debug)]
struct BudgetState {
    /// The size of the budget.
    threads: NonZeroU32,
    /// The threads that a shrink couldn't take yet because they were in use; returned threads
    /// pay it off before they are freed.
    debt: usize,
}

impl ThreadBudget {
    /// A budget of `threads` threads.
    pub fn new(threads: NonZeroU32) -> Self {
        Self {
            inner: Arc::new(BudgetInner {
                semaphore: Arc::new(Semaphore::new(to_usize(threads.get()))),
                state: Mutex::new(BudgetState { threads, debt: 0 }),
            }),
        }
    }

    /// The size of the budget.
    pub fn threads(&self) -> NonZeroU32 {
        lock_unpoisoned(&self.inner.state).threads
    }

    /// The number of threads that are free now.
    pub fn available(&self) -> usize {
        self.inner.semaphore.available_permits()
    }

    /// Changes the size of the budget to `threads`.
    ///
    /// Growing frees the new threads at once. Shrinking takes the free threads at once, and the
    /// threads in use as they are returned, so that a running simulation keeps its threads.
    pub fn resize(&self, threads: NonZeroU32) {
        let mut state = lock_unpoisoned(&self.inner.state);
        let old = to_usize(state.threads.get());
        let new = to_usize(threads.get());
        if new < old {
            let shrink = old - new;
            let taken = self.inner.semaphore.forget_permits(shrink);
            state.debt += shrink - taken;
        } else {
            let grow = new - old;
            let paid = grow.min(state.debt);
            state.debt -= paid;
            self.inner.semaphore.add_permits(grow - paid);
        }
        state.threads = threads;
    }

    /// Waits for one thread, and then takes up to `share` threads without waiting.
    ///
    /// # Errors
    ///
    /// Fails if the budget is closed, which it never is.
    pub async fn acquire(&self, share: NonZeroU32) -> Result<ThreadPermit, AcquireError> {
        let mut permit = self.acquire_one().await?;
        self.extend(&mut permit, share.get());
        Ok(permit)
    }

    /// Waits for one thread.
    async fn acquire_one(&self) -> Result<ThreadPermit, AcquireError> {
        let permit = Arc::clone(&self.inner.semaphore).acquire_owned().await?;
        Ok(self.permit(permit))
    }

    /// Takes one thread if one is free.
    fn try_acquire_one(&self) -> Option<ThreadPermit> {
        let permit = Arc::clone(&self.inner.semaphore).try_acquire_owned().ok()?;
        Some(self.permit(permit))
    }

    /// Adds free threads to `permit` without waiting, until it has `threads` threads.
    fn extend(&self, permit: &mut ThreadPermit, threads: u32) {
        let wanted = threads.saturating_sub(permit.threads());
        let free = u32::try_from(self.available()).unwrap_or(u32::MAX);
        let extra = wanted.min(free);
        if extra == 0 {
            return;
        }
        // Another acquisition may take the free threads first; one thread is enough then.
        if let Ok(more) = Arc::clone(&self.inner.semaphore).try_acquire_many_owned(extra)
            && let Some(held) = &mut permit.permit
        {
            held.merge(more);
        }
    }

    fn permit(&self, permit: OwnedSemaphorePermit) -> ThreadPermit {
        ThreadPermit {
            budget: Arc::clone(&self.inner),
            permit: Some(permit),
        }
    }
}

/// Threads of a [`ThreadBudget`]; returned when dropped.
#[derive(Debug)]
pub struct ThreadPermit {
    budget: Arc<BudgetInner>,
    /// `None` only while dropping.
    permit: Option<OwnedSemaphorePermit>,
}

impl ThreadPermit {
    /// The number of threads.
    pub fn threads(&self) -> u32 {
        self.permit.as_ref().map_or(0, |permit| {
            u32::try_from(permit.num_permits()).unwrap_or(u32::MAX)
        })
    }

    /// Returns the threads above `threads`.
    fn shrink_to(&mut self, threads: u32) {
        let Some(permit) = &mut self.permit else {
            return;
        };
        let surplus = permit.num_permits().saturating_sub(to_usize(threads));
        if surplus > 0
            && let Some(returned) = permit.split(surplus)
        {
            self.budget.release(returned);
        }
    }
}

impl Drop for ThreadPermit {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            self.budget.release(permit);
        }
    }
}

impl BudgetInner {
    /// Returns the threads of `permit`, paying off the debt of a shrink first.
    fn release(&self, mut permit: OwnedSemaphorePermit) {
        let mut state = lock_unpoisoned(&self.state);
        let paid = state.debt.min(permit.num_permits());
        if paid > 0
            && let Some(forgotten) = permit.split(paid)
        {
            forgotten.forget();
            state.debt -= paid;
        }
        drop(permit);
    }
}

/// The share of a test: `free` threads divided by the `waiting` tests, but at least one.
pub fn share(free: usize, waiting: usize) -> NonZeroU32 {
    let share = free.checked_div(waiting).unwrap_or(free);
    u32::try_from(share)
        .ok()
        .and_then(NonZeroU32::new)
        .unwrap_or(NonZeroU32::MIN)
}

/// Converts a thread count, which always fits.
fn to_usize(threads: u32) -> usize {
    usize::try_from(threads).unwrap_or(usize::MAX)
}

// -------------------------------------------------------------------------------------------------
// Limit of an operation
// -------------------------------------------------------------------------------------------------

/// Limits the simulations of an operation: by the [`ThreadBudget`] of the runtime and,
/// optionally, by the operation's number of processes and its thread cap; cheap to clone.
#[derive(Debug, Clone)]
pub struct SimulationLimit {
    budget: ThreadBudget,
    processes: Option<Arc<Semaphore>>,
    threads: Option<ThreadBudget>,
}

/// Permission to run a simulation with [`threads`](Self::threads) threads; released when
/// dropped.
#[derive(Debug)]
pub struct SimulationPermit {
    _process: Option<OwnedSemaphorePermit>,
    _cap: Option<ThreadPermit>,
    threads: ThreadPermit,
}

impl SimulationPermit {
    /// The number of threads.
    pub fn threads(&self) -> u32 {
        self.threads.threads()
    }
}

impl SimulationLimit {
    /// A limit by `budget` and, if given, by the semaphore of the operation's processes and the
    /// operation's thread cap.
    pub const fn new(
        budget: ThreadBudget,
        processes: Option<Arc<Semaphore>>,
        threads: Option<ThreadBudget>,
    ) -> Self {
        Self {
            budget,
            processes,
            threads,
        }
    }

    /// Waits for a process permit and threads, for one of `waiting` tests that haven't started,
    /// with at most `max_threads` threads.
    ///
    /// The limits of the operation come first, so that a test waiting for them doesn't hold
    /// threads of the budget that other operations could use.
    /// `on_budget_wait` is called with the size of the budget if no thread of it is free.
    ///
    /// # Errors
    ///
    /// Fails if a semaphore is closed.
    pub async fn acquire(
        &self,
        waiting: usize,
        max_threads: NonZeroU32,
        on_budget_wait: impl FnOnce(NonZeroU32),
    ) -> Result<SimulationPermit, AcquireError> {
        let process = match &self.processes {
            Some(semaphore) => Some(Arc::clone(semaphore).acquire_owned().await?),
            None => None,
        };
        let mut cap = match &self.threads {
            Some(threads) => {
                let mut permit = threads.acquire_one().await?;
                let share = share(threads.available() + 1, waiting).min(max_threads);
                threads.extend(&mut permit, share.get());
                Some(permit)
            },
            None => None,
        };
        let limit = cap
            .as_ref()
            .and_then(|cap| NonZeroU32::new(cap.threads()))
            .unwrap_or(max_threads);
        let mut threads = if let Some(permit) = self.budget.try_acquire_one() {
            permit
        } else {
            on_budget_wait(self.budget.threads());
            self.budget.acquire_one().await?
        };
        let share = share(self.budget.available() + 1, waiting).min(limit);
        self.budget.extend(&mut threads, share.get());
        if let Some(cap) = &mut cap {
            cap.shrink_to(threads.threads());
        }
        Ok(SimulationPermit {
            _process: process,
            _cap: cap,
            threads,
        })
    }
}

impl From<ThreadBudget> for SimulationLimit {
    /// A limit by `budget` alone.
    fn from(budget: ThreadBudget) -> Self {
        Self::new(budget, None, None)
    }
}

// -------------------------------------------------------------------------------------------------
// Leases
// -------------------------------------------------------------------------------------------------

/// The future of [`LeaseProvider::lease`].
pub type LeaseFuture<'a> = Pin<Box<dyn Future<Output = Result<Lease, String>> + Send + 'a>>;

/// Grants the simulations of a runtime the right to use their threads.
pub trait LeaseProvider: Send + Sync + fmt::Debug {
    /// Grants the simulation of `test`, which holds `threads` threads of the budget, its lease.
    ///
    /// It may wait, and reports what the test waits for through `events`; a cancel drops the
    /// future.
    /// The lease's guard may keep a clone of `events`, to report the states of the running
    /// simulator until it is dropped, which is before the test's
    /// [`SimulationEvent::TestFinished`].
    /// An error fails the test with its message.
    fn lease<'a>(&'a self, test: &'a str, threads: u32, events: &'a LeaseEvents)
    -> LeaseFuture<'a>;
}

/// The lease of a simulation.
pub struct Lease {
    /// Run options of the simulator, such as `lease-dir`, which go over the simulation's others
    /// ([`SimulateArgs::lease_options`](crate::simulator::SimulateArgs::lease_options)).
    pub options: RunOptions,
    /// Kept until the simulator has exited, or, if the test ends without waiting for that (an
    /// error while waiting, or a dropped task), until it is killed.
    pub guard: Box<dyn Any + Send>,
}

impl Lease {
    /// A lease without run options and without a guard.
    pub fn empty() -> Self {
        Self {
            options: RunOptions::new(),
            guard: Box::new(()),
        }
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lease")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

/// A [`LeaseProvider`] that grants empty leases at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLeases;

impl LeaseProvider for NoLeases {
    fn lease<'a>(
        &'a self,
        _test: &'a str,
        _threads: u32,
        _events: &'a LeaseEvents,
    ) -> LeaseFuture<'a> {
        Box::pin(std::future::ready(Ok(Lease::empty())))
    }
}

/// Reports the thread states of one test, for a [`LeaseProvider`].
#[derive(Debug, Clone)]
pub struct LeaseEvents {
    name: String,
    events: SimulationEvents,
}

impl LeaseEvents {
    /// The reporter of the test `name`, which sends to `events`.
    pub const fn new(name: String, events: SimulationEvents) -> Self {
        Self { name, events }
    }

    /// Reports `state` as [`SimulationEvent::TestThreads`].
    pub fn report(&self, state: ThreadState) {
        self.events.send(SimulationEvent::TestThreads {
            name: self.name.clone(),
            state,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn threads(count: u32) -> NonZeroU32 {
        NonZeroU32::new(count).expect("non-zero")
    }

    #[test]
    fn shares_divide_the_free_threads() {
        assert_eq!(share(10, 5), threads(2));
        assert_eq!(share(10, 100), threads(1));
        assert_eq!(share(10, 3), threads(3));
        assert_eq!(share(0, 3), threads(1));
        assert_eq!(share(4, 0), threads(4));
    }

    #[tokio::test]
    async fn acquire_takes_up_to_the_share_without_waiting() {
        let budget = ThreadBudget::new(threads(4));
        let first = budget.acquire(threads(3)).await.unwrap();
        assert_eq!(first.threads(), 3);
        let second = budget.acquire(threads(3)).await.unwrap();
        assert_eq!(second.threads(), 1);
        assert_eq!(budget.available(), 0);
        drop(first);
        assert_eq!(budget.available(), 3);
        drop(second);
        assert_eq!(budget.available(), 4);
    }

    #[tokio::test]
    async fn a_shrink_drains_as_permits_return() {
        let budget = ThreadBudget::new(threads(4));
        let held = budget.acquire(threads(3)).await.unwrap();
        budget.resize(threads(2));
        assert_eq!(budget.threads(), threads(2));
        // The free thread is taken at once, one of the held ones when they return.
        assert_eq!(budget.available(), 0);
        drop(held);
        assert_eq!(budget.available(), 2);

        // A grow pays off a debt first.
        let again = budget.acquire(threads(2)).await.unwrap();
        budget.resize(threads(1));
        budget.resize(threads(3));
        assert_eq!(budget.available(), 1);
        drop(again);
        assert_eq!(budget.available(), 3);
    }

    #[tokio::test]
    async fn the_operation_cap_bounds_the_threads() {
        let budget = ThreadBudget::new(threads(8));
        let limit = SimulationLimit::new(budget.clone(), None, Some(ThreadBudget::new(threads(3))));
        let mut waited = false;
        let first = limit
            .acquire(1, NonZeroU32::MAX, |_| waited = true)
            .await
            .unwrap();
        assert_eq!(first.threads(), 3);
        assert_eq!(budget.available(), 5);
        assert!(!waited);
        drop(first);
        // A GUI test takes one thread.
        let gui = limit.acquire(1, NonZeroU32::MIN, |_| {}).await.unwrap();
        assert_eq!(gui.threads(), 1);
        assert_eq!(budget.available(), 7);
    }

    #[tokio::test]
    async fn a_full_budget_reports_the_wait() {
        let budget = ThreadBudget::new(threads(1));
        let limit = SimulationLimit::from(budget.clone());
        let held = limit.acquire(1, NonZeroU32::MAX, |_| {}).await.unwrap();
        let (sender, receiver) = mpsc::channel();
        let waiting = tokio::spawn(async move {
            limit
                .acquire(1, NonZeroU32::MAX, move |size| {
                    sender.send(size).ok();
                })
                .await
                .map(|permit| permit.threads())
        });
        let size = loop {
            if let Ok(size) = receiver.try_recv() {
                break size;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(size, threads(1));
        // A grow wakes the waiting test.
        budget.resize(threads(2));
        assert_eq!(waiting.await.unwrap().unwrap(), 1);
        drop(held);
    }
}
