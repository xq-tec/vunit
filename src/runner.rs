// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Running testcases: one simulation per test.
//!
//! A port of `test/suites.py` (`TestRun`, `encode_dict`) and the output paths of
//! `test/runner.py`, for a single test per simulation:
//!
//! - **Resolution:** [`SimulationPlan::new`] matches the requested patterns against the
//!   testcases and prepares a [`PlannedTest`] per match.
//! - **Run:** each test first takes its testcase lock, so the same testcase never runs twice at
//!   once, and then a permit of the simulation semaphore. Tests whose lock is free get permits
//!   in name order; a test whose lock is held by another simulation waits for it without
//!   holding up the others. Each test with a permit recreates its output directory, gets a seed
//!   and its `runner_cfg` generic, and runs `risim-ghdl --elab-run` with stdout and stderr
//!   redirected to `output.txt`.
//! - **Outcome:** an explicit test passed if `vunit_results` records its start and the end of
//!   the test suite; a testbench without explicit tests passed if the test suite ended. A
//!   non-zero exit code fails a passed test from VHDL-2008 on (`has_valid_exit_code`).
//! - **Results:** every finished test is recorded in the [`ResultStore`]. A test cancelled
//!   before it started keeps its previous result, since its output directory is unchanged.
//!
//! Differences from `VUnit`:
//!
//! - A test that never started counts as failed (`VUnit`: skipped).
//! - No `pre_config`/`post_check` hooks, no seed "repeat", no elaborate-only mode.
//! - Problems that prevent a simulation (unsupported standard, spawn failure, output directory
//!   errors) fail the test, are written to its `output.txt`, and are reported as diagnostics.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use tokio::sync::OwnedMutexGuard;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::compile;
use crate::configuration::directory_generic;
use crate::diagnostics::Diagnostic;
use crate::discovery::Discovery;
use crate::pattern;
use crate::process;
use crate::project::Project;
use crate::simulator::SimulateArgs;
use crate::simulator::Simulator;
use crate::simulator::Top;
use crate::spec::AssertLevel;
use crate::spec::SimOptions;
use crate::store::OutputLayout;
use crate::store::ResultStore;
use crate::store::TestOutcome;
use crate::store::TestOutputPaths;
use crate::store::TestResult;
use crate::store::Timestamp;
use crate::vhdl_parser::latin1;
use crate::vhdl_standard::VhdlStandard;

#[cfg(test)]
mod tests;

// -------------------------------------------------------------------------------------------------
// runner_cfg
// -------------------------------------------------------------------------------------------------

/// Escapes commas in a test name, so that the VHDL runner doesn't split it
/// (`encode_test_case`).
pub fn encode_test_case(name: &str) -> String {
    name.replace(',', ",,")
}

/// Encodes a dictionary for `VUnit`'s VHDL dictionary parser (`encode_dict`): entries sorted by
/// key, `key : value`, separated by commas, with `:` and `,` doubled in keys and values.
pub fn encode_dict<'entry>(
    entries: impl IntoIterator<Item = (&'entry str, &'entry str)>,
) -> String {
    let escape = |text: &str| text.replace(':', "::").replace(',', ",,");
    let mut entries: Vec<(&str, &str)> = entries.into_iter().collect();
    entries.sort_unstable();
    entries
        .iter()
        .map(|(key, value)| format!("{} : {}", escape(key), escape(value)))
        .collect::<Vec<_>>()
        .join(",")
}

/// The `runner_cfg` generic of a test run.
#[derive(Debug, Clone, Copy)]
pub struct RunnerCfg<'a> {
    /// The test to run, or `None` for a testbench without explicit tests.
    pub test: Option<&'a str>,
    /// The test output directory.
    pub output_path: &'a Utf8Path,
    /// The seed.
    pub seed: &'a str,
    /// The directory of the testbench file.
    pub tb_path: &'a Utf8Path,
}

impl RunnerCfg<'_> {
    /// Encodes the configuration as `TestRun._simulate` does.
    pub fn encode(&self) -> String {
        let enabled = self.test.map(encode_test_case).unwrap_or_default();
        let output_path = directory_generic(self.output_path);
        let tb_path = directory_generic(self.tb_path);
        encode_dict([
            ("enabled_test_cases", enabled.as_str()),
            ("use_color", "false"),
            ("output path", output_path.as_str()),
            ("active python runner", "true"),
            ("tb path", tb_path.as_str()),
            ("seed", self.seed),
        ])
    }
}

/// A new random seed: 16 hex digits.
pub fn generate_seed() -> String {
    format!("{:016x}", rand::random::<u64>())
}

// -------------------------------------------------------------------------------------------------
// Results
// -------------------------------------------------------------------------------------------------

/// Whether `vunit_results` shows that the test passed, ignoring the exit code
/// (`_read_test_results` for a single enabled test).
///
/// An explicit test passed if it started and the test suite completed; with a single enabled
/// test, no later test can start. A testbench without explicit tests passed if the test suite
/// completed.
pub fn results_show_pass(results: &str, test: Option<&str>) -> bool {
    let mut started = false;
    let mut suite_done = false;
    for line in results.lines() {
        if let Some(name) = line.strip_prefix("test_start:") {
            started |= test == Some(name);
        } else if line.starts_with("test_suite_done") {
            suite_done = true;
        }
    }
    suite_done && (started || test.is_none())
}

/// The outcome of a finished simulation (`_check_results`): a non-zero exit code fails a passed
/// test if the testbench is VHDL-2008 or later.
pub fn outcome(results_pass: bool, exit_success: bool, standard: VhdlStandard) -> TestOutcome {
    let valid_exit_code = standard >= VhdlStandard::Vhdl2008;
    if results_pass && (exit_success || !valid_exit_code) {
        TestOutcome::Passed
    } else {
        TestOutcome::Failed
    }
}

// -------------------------------------------------------------------------------------------------
// Planning
// -------------------------------------------------------------------------------------------------

/// A request to run the testcases matching `pattern`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimulationRequest {
    /// A testcase pattern (`fnmatch`, case-insensitive).
    pub pattern: String,
    /// Whether to start the simulations paused, for a waveform viewer.
    pub gui: bool,
}

/// Everything a simulation plan is made from.
#[derive(Debug, Clone, Copy)]
pub struct SimulationInput<'a> {
    /// The output directory.
    pub layout: &'a OutputLayout,
    /// The project.
    pub project: &'a Project,
    /// The testcases of the project.
    pub discovery: &'a Discovery,
    /// The project's simulation options; configurations override them.
    pub sim_options: &'a SimOptions,
}

/// The top-level unit of a planned simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlannedTop {
    Configuration(String),
    Entity {
        entity: String,
        architecture: String,
    },
}

/// A testcase ready to run.
#[derive(Debug, Clone)]
pub struct PlannedTest {
    /// The testcase name.
    pub name: String,
    /// Whether to start the simulation paused.
    pub gui: bool,
    /// The output directory and its files.
    pub paths: TestOutputPaths,
    /// The test name, or `None` for a testbench without explicit tests.
    test: Option<String>,
    vhdl_standard: VhdlStandard,
    library: String,
    library_dir: Utf8PathBuf,
    library_dirs: Arc<[Utf8PathBuf]>,
    top: PlannedTop,
    /// The configuration's generics, without `runner_cfg`.
    generics: BTreeMap<String, String>,
    /// Whether the entity declares an `output_path` generic that the configuration leaves unset.
    fill_output_path: bool,
    tb_path: Utf8PathBuf,
    elab_flags: Vec<String>,
    sim_flags: Vec<String>,
    assert_level: AssertLevel,
    disable_ieee_asserts: bool,
    seed: Option<String>,
}

impl PlannedTest {
    /// The seed: the `seed` simulation option, or a new random one.
    pub fn seed(&self) -> String {
        self.seed.clone().unwrap_or_else(generate_seed)
    }

    /// The generics of a run with `seed`, including `runner_cfg` and `output_path`.
    pub fn generics(&self, seed: &str) -> BTreeMap<String, String> {
        let mut generics = self.generics.clone();
        if self.fill_output_path {
            generics.insert("output_path".to_owned(), directory_generic(&self.paths.dir));
        }
        let runner_cfg = RunnerCfg {
            test: self.test.as_deref(),
            output_path: &self.paths.dir,
            seed,
            tb_path: &self.tb_path,
        };
        generics.insert("runner_cfg".to_owned(), runner_cfg.encode());
        generics
    }

    /// The command line of a run with `seed`.
    ///
    /// # Errors
    ///
    /// Fails if the simulator doesn't support the testbench's standard.
    pub fn command(
        &self,
        simulator: &Simulator,
        seed: &str,
    ) -> Result<Vec<String>, crate::simulator::CommandError> {
        let generics = self.generics(seed);
        let top = match &self.top {
            PlannedTop::Configuration(name) => Top::Configuration(name),
            PlannedTop::Entity {
                entity,
                architecture,
            } => Top::Entity {
                entity,
                architecture,
            },
        };
        simulator.simulate_command(&SimulateArgs {
            vhdl_standard: self.vhdl_standard,
            library: &self.library,
            library_dir: &self.library_dir,
            library_dirs: &self.library_dirs,
            elab_flags: &self.elab_flags,
            top,
            sim_flags: &self.sim_flags,
            generics: &generics,
            assert_level: self.assert_level,
            disable_ieee_asserts: self.disable_ieee_asserts,
            wait: self.gui,
            name: &self.name,
        })
    }

    /// The outcome of a finished simulation given the contents of `vunit_results`.
    pub fn outcome(&self, results: &str, exit_success: bool) -> TestOutcome {
        outcome(
            results_show_pass(results, self.test.as_deref()),
            exit_success,
            self.vhdl_standard,
        )
    }
}

/// The testcases a simulation runs.
#[derive(Debug, Clone, Default)]
pub struct SimulationPlan {
    /// The tests, sorted by name.
    pub tests: Vec<PlannedTest>,
    /// Warnings about patterns that match no testcase.
    pub diagnostics: Vec<Diagnostic>,
}

impl SimulationPlan {
    /// Matches `requests` against the testcases of `input.discovery`.
    ///
    /// A testcase matched by several requests runs once, paused if any of them asks for it.
    pub fn new(input: &SimulationInput<'_>, requests: &[SimulationRequest]) -> Self {
        let discovery = input.discovery;
        let names: Vec<&str> = discovery
            .runs
            .iter()
            .map(|run| run.testcase.name.as_str())
            .collect();
        let resolution = pattern::resolve(
            requests
                .iter()
                .map(|request| (request.pattern.as_str(), request.gui)),
            &names,
        );
        let runs: FxHashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(index, &name)| (name, index))
            .collect();
        let library_dirs: Arc<[Utf8PathBuf]> =
            compile::library_dirs(input.project, input.layout).into();

        let tests = resolution
            .entries
            .into_iter()
            .filter_map(|(name, gui)| {
                let run = &discovery.runs[*runs.get(name.as_str())?];
                let testbench = discovery.testbenches.get(run.testbench)?;
                let configuration = &run.configuration;
                let sim_options = input.sim_options.overridden_by(&configuration.sim_options);
                let top = configuration.vhdl_configuration_name.clone().map_or_else(
                    || PlannedTop::Entity {
                        entity: testbench.entity.clone(),
                        architecture: testbench.architecture.clone(),
                    },
                    PlannedTop::Configuration,
                );
                let fill_output_path = testbench
                    .generic_names
                    .iter()
                    .any(|generic| generic == "output_path")
                    && !configuration
                        .generics
                        .keys()
                        .any(|generic| generic.eq_ignore_ascii_case("output_path"));
                let paths = TestOutputPaths::new(input.layout, &name);
                Some(PlannedTest {
                    name,
                    gui,
                    paths,
                    test: run.test.clone(),
                    vhdl_standard: testbench.vhdl_standard,
                    library: testbench.library_name.clone(),
                    library_dir: input.layout.library_dir(&testbench.library_name),
                    library_dirs: Arc::clone(&library_dirs),
                    top,
                    generics: configuration.generics.clone(),
                    fill_output_path,
                    tb_path: configuration.tb_path.clone(),
                    elab_flags: sim_options.elab_flags.unwrap_or_default(),
                    sim_flags: sim_options.sim_flags.unwrap_or_default(),
                    assert_level: sim_options
                        .vhdl_assert_stop_level
                        .unwrap_or(AssertLevel::Error),
                    disable_ieee_asserts: sim_options.disable_ieee_warnings.unwrap_or(false),
                    seed: sim_options.seed,
                })
            })
            .collect();
        let diagnostics = resolution
            .unmatched
            .into_iter()
            .map(|pattern| Diagnostic::warning(format!("no testcase matches '{pattern}'")))
            .collect();
        Self { tests, diagnostics }
    }

    /// The testcase names, sorted.
    pub fn testcases(&self) -> Vec<String> {
        self.tests.iter().map(|test| test.name.clone()).collect()
    }
}

// -------------------------------------------------------------------------------------------------
// Execution
// -------------------------------------------------------------------------------------------------

/// Progress of a simulation, for live status reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationEvent {
    /// The testcases to run are known.
    Started {
        /// The testcase names, sorted.
        testcases: Vec<String>,
    },
    /// The simulator of a testcase is about to be spawned.
    TestStarted {
        /// The testcase name.
        name: String,
        /// The simulator output file (`output.txt`).
        output_path: Utf8PathBuf,
    },
    /// A testcase finished, failed to start, or was cancelled.
    TestFinished {
        /// The testcase name.
        name: String,
        /// How it ended.
        outcome: TestOutcome,
        /// The simulator output file (`output.txt`). For a test cancelled before it started,
        /// the file belongs to an earlier run, if any.
        output_path: Utf8PathBuf,
        /// How long the simulator ran; zero if it didn't start.
        duration: Duration,
    },
    /// A problem with a simulation: a spawn failure, an unreadable results file, or a pattern
    /// that matches nothing.
    Diagnostic {
        /// The testcase the problem belongs to; `None` for a pattern that matches nothing.
        testcase: Option<String>,
        /// The problem.
        diagnostic: Diagnostic,
    },
    /// All testcases are done.
    Finished {
        /// The number of passed testcases.
        passed: usize,
        /// The number of failed testcases.
        failed: usize,
        /// The number of cancelled testcases.
        cancelled: usize,
    },
}

/// The result of a simulation.
#[derive(Debug, Clone, Default)]
pub struct SimulationReport {
    /// The outcome of every testcase, sorted by name.
    pub outcomes: Vec<(String, TestOutcome)>,
    /// The problems reported during the simulation.
    pub diagnostics: Vec<Diagnostic>,
}

impl SimulationReport {
    /// The number of testcases with `outcome`.
    pub fn count(&self, outcome: TestOutcome) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, test_outcome)| *test_outcome == outcome)
            .count()
    }
}

/// One lock per testcase, shared by all simulations of a workspace, so that the same testcase
/// never runs twice at once.
#[derive(Debug, Default)]
pub struct TestcaseLocks {
    locks: Mutex<FxHashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl TestcaseLocks {
    /// The lock of `testcase`.
    fn get(&self, testcase: &str) -> Arc<tokio::sync::Mutex<()>> {
        // The map stays consistent even if a holder panicked: every change is one insert.
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lock) = locks.get(testcase) {
            return Arc::clone(lock);
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(testcase.to_owned(), Arc::clone(&lock));
        lock
    }
}

/// Receives the events of a simulation, synchronously and in emission order.
///
/// A callback rather than a channel, so that a listener of several simulations can merge their
/// events into one ordered stream: with a channel per simulation, the second run of a testcase
/// could be reported as started before the first run is reported as finished.
#[derive(Clone)]
pub struct SimulationEvents(Arc<dyn Fn(SimulationEvent) + Send + Sync>);

impl SimulationEvents {
    /// Calls `listener` for every event.
    pub fn new(listener: impl Fn(SimulationEvent) + Send + Sync + 'static) -> Self {
        Self(Arc::new(listener))
    }

    fn send(&self, event: SimulationEvent) {
        (self.0)(event);
    }
}

impl From<mpsc::UnboundedSender<SimulationEvent>> for SimulationEvents {
    fn from(sender: mpsc::UnboundedSender<SimulationEvent>) -> Self {
        Self::new(move |event| {
            if sender.send(event).is_err() {
                tracing::trace!("nobody listens to simulation events");
            }
        })
    }
}

impl std::fmt::Debug for SimulationEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SimulationEvents")
    }
}

/// The shared resources of a simulation.
#[derive(Debug, Clone)]
pub struct SimulationContext {
    /// The working directory of the simulator processes.
    pub workspace_root: Utf8PathBuf,
    /// The simulator.
    pub simulator: Simulator,
    /// Limits the number of concurrent simulations; a paused GUI simulation holds its permit.
    pub semaphore: Arc<Semaphore>,
    /// Receives the results.
    pub results: Arc<ResultStore>,
    /// The testcase locks of the workspace.
    pub testcase_locks: Arc<TestcaseLocks>,
    /// Receives the progress.
    pub events: SimulationEvents,
    /// Cancels the simulation.
    pub cancel: CancellationToken,
}

impl SimulationContext {
    fn emit(&self, event: SimulationEvent) {
        self.events.send(event);
    }
}

/// Runs all tests of `plan` concurrently, bounded by the semaphore of `context`.
pub async fn simulate(plan: SimulationPlan, context: &SimulationContext) -> SimulationReport {
    context.emit(SimulationEvent::Started {
        testcases: plan.testcases(),
    });
    for diagnostic in &plan.diagnostics {
        context.emit(SimulationEvent::Diagnostic {
            testcase: None,
            diagnostic: diagnostic.clone(),
        });
    }

    let mut outcomes: Vec<(String, TestOutcome)> = plan
        .tests
        .iter()
        .map(|test| (test.name.clone(), TestOutcome::Failed))
        .collect();
    let output_files: Vec<Utf8PathBuf> = plan
        .tests
        .iter()
        .map(|test| test.paths.output_file.clone())
        .collect();
    let mut diagnostics = plan.diagnostics;
    let mut tasks = JoinSet::new();
    let mut task_indices = FxHashMap::default();
    // Permits are handed out in plan order, so tests start in the order they were announced.
    // A test whose testcase runs in another simulation waits in its own task instead, so that it
    // doesn't hold up the tests after it.
    let mut pending = plan.tests.into_iter().enumerate();
    while let Some((index, test)) = pending.next() {
        let lock = context.testcase_locks.get(&test.name);
        let context = context.clone();
        let Ok(guard) = Arc::clone(&lock).try_lock_owned() else {
            let handle = tasks.spawn(async move {
                let guard = tokio::select! {
                    biased;
                    () = context.cancel.cancelled() => None,
                    guard = lock.lock_owned() => Some(guard),
                };
                let permit = match guard {
                    Some(_) => acquire_permit(&context).await,
                    None => None,
                };
                match guard.zip(permit) {
                    Some((guard, permit)) => run_locked(&test, &context, guard, permit).await,
                    None => cancelled_before_start(test, &context),
                }
            });
            task_indices.insert(handle.id(), index);
            continue;
        };
        // Cancellation wins over a free permit, so no test starts after a cancel.
        let Some(permit) = acquire_permit(&context).await else {
            drop(guard);
            for (skipped_index, skipped) in std::iter::once((index, test)).chain(pending.by_ref()) {
                outcomes[skipped_index].1 = cancelled_before_start(skipped, &context).0;
            }
            break;
        };
        let handle = tasks.spawn(async move { run_locked(&test, &context, guard, permit).await });
        task_indices.insert(handle.id(), index);
    }
    while let Some(joined) = tasks.join_next_with_id().await {
        match joined {
            Ok((id, (outcome, test_diagnostics))) => {
                if let Some(&index) = task_indices.get(&id) {
                    outcomes[index].1 = outcome;
                }
                diagnostics.extend(test_diagnostics);
            },
            // A panicked test keeps the outcome `Failed`; listeners still see it finish.
            Err(error) => {
                tracing::error!(%error, "simulation task failed");
                if let Some(&index) = task_indices.get(&error.id()) {
                    context.emit(SimulationEvent::TestFinished {
                        name: outcomes[index].0.clone(),
                        outcome: TestOutcome::Failed,
                        output_path: output_files[index].clone(),
                        duration: Duration::ZERO,
                    });
                }
            },
        }
    }

    let report = SimulationReport {
        outcomes,
        diagnostics,
    };
    context.emit(SimulationEvent::Finished {
        passed: report.count(TestOutcome::Passed),
        failed: report.count(TestOutcome::Failed),
        cancelled: report.count(TestOutcome::Cancelled),
    });
    report
}

/// A permit of the simulation semaphore, or `None` if the simulation is cancelled first.
async fn acquire_permit(context: &SimulationContext) -> Option<OwnedSemaphorePermit> {
    tokio::select! {
        biased;
        () = context.cancel.cancelled() => None,
        permit = Arc::clone(&context.semaphore).acquire_owned() => permit.ok(),
    }
}

/// Runs a test that holds its testcase lock and a permit, and releases both afterwards.
async fn run_locked(
    test: &PlannedTest,
    context: &SimulationContext,
    guard: OwnedMutexGuard<()>,
    permit: OwnedSemaphorePermit,
) -> (TestOutcome, Vec<Diagnostic>) {
    let result = run_test(test, context).await;
    drop(permit);
    drop(guard);
    result
}

/// Reports a test that was cancelled before it started; its previous result is kept.
fn cancelled_before_start(
    test: PlannedTest,
    context: &SimulationContext,
) -> (TestOutcome, Vec<Diagnostic>) {
    context.emit(SimulationEvent::TestFinished {
        name: test.name,
        outcome: TestOutcome::Cancelled,
        output_path: test.paths.output_file,
        duration: Duration::ZERO,
    });
    (TestOutcome::Cancelled, Vec::new())
}

/// Runs one test that holds a simulation permit; returns its outcome and the problems found.
async fn run_test(
    test: &PlannedTest,
    context: &SimulationContext,
) -> (TestOutcome, Vec<Diagnostic>) {
    let started_at = Timestamp::now();
    context.emit(SimulationEvent::TestStarted {
        name: test.name.clone(),
        output_path: test.paths.output_file.clone(),
    });
    let started = Instant::now();
    let mut diagnostics = Vec::new();
    let outcome = match execute(test, context).await {
        Ok(outcome) => outcome,
        Err(message) => {
            append_to_output(&test.paths.output_file, &message);
            diagnostics.push(Diagnostic::error(format!("{}: {message}", test.name)));
            TestOutcome::Failed
        },
    };
    let duration = started.elapsed();

    let result = TestResult {
        outcome,
        started_at,
        finished_at: Timestamp::now(),
        output_path: test.paths.output_file.clone(),
    };
    if let Err(error) = context.results.record(&test.name, result) {
        diagnostics.push(Diagnostic::warning(format!(
            "failed to save the test results: {error}"
        )));
    }
    for diagnostic in &diagnostics {
        context.emit(SimulationEvent::Diagnostic {
            testcase: Some(test.name.clone()),
            diagnostic: diagnostic.clone(),
        });
    }
    context.emit(SimulationEvent::TestFinished {
        name: test.name.clone(),
        outcome,
        output_path: test.paths.output_file.clone(),
        duration,
    });
    (outcome, diagnostics)
}

/// Prepares the output directory, runs the simulator and reads the results.
async fn execute(test: &PlannedTest, context: &SimulationContext) -> Result<TestOutcome, String> {
    test.paths.prepare().map_err(|error| {
        format!(
            "failed to prepare the output directory {}: {error}",
            test.paths.dir
        )
    })?;
    let seed = test.seed();
    let command = test
        .command(&context.simulator, &seed)
        .map_err(|error| error.to_string())?;
    let header = format!("Seed for {}: {seed}\n", test.name);
    tracing::debug!(name = %test.name, ?command, "starting simulation");
    let outcome = process::run_to_file(
        &command,
        &context.workspace_root,
        &test.paths.output_file,
        header.as_bytes(),
        &context.cancel,
    )
    .await
    .map_err(|error| format!("failed to run risim-ghdl: {error}"))?;
    let status = match outcome {
        process::Outcome::Exited(status) => status,
        process::Outcome::Cancelled => return Ok(TestOutcome::Cancelled),
    };
    let results = match fs::read(&test.paths.results_file) {
        Ok(contents) => latin1(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read {}: {error}",
                test.paths.results_file
            ));
        },
    };
    Ok(test.outcome(&results, status.success()))
}

/// Appends a message to the output file, so that clients showing the output see it.
fn append_to_output(path: &Utf8Path, message: &str) {
    let result = fs::File::options()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{message}"));
    if let Err(error) = result {
        tracing::debug!(%path, %error, "failed to write the error to the output file");
    }
}
