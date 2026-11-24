// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests of compile and simulation operations.
//!
//! The binary doubles as a fake risim-runner and analyzer (see `fake_risim.rs`), so the tests run
//! everywhere without a simulator. The trial `real_risim_compiles_uart` uses the risim-runner
//! named by `RISIM_RUNNER` and the analyzer named by `RISIM_ANALYZER`, and is ignored without
//! either; the end-to-end tests with risim-runner are in the `acceptance` target.
//!
//! AI NOTICE: Generated, minimally reviewed.

mod fake_risim;
#[path = "../common/trial.rs"]
mod trial;
mod workspace;

use std::env;
use std::fs;
use std::num::NonZeroU32;
use std::num::NonZeroUsize;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use libtest_mimic::Arguments;
use libtest_mimic::Trial;
use risim_vunit_frontend::builtins;
use risim_vunit_frontend::compile;
use risim_vunit_frontend::compile::CompileContext;
use risim_vunit_frontend::compile::CompileEvent;
use risim_vunit_frontend::compile::CompileReport;
use risim_vunit_frontend::compile::CompileStatus;
use risim_vunit_frontend::compile::FileStatus;
use risim_vunit_frontend::compile::PlanInput;
use risim_vunit_frontend::diagnostics::Severity;
use risim_vunit_frontend::discovery;
use risim_vunit_frontend::runner;
use risim_vunit_frontend::runner::SimulationContext;
use risim_vunit_frontend::runner::SimulationEvent;
use risim_vunit_frontend::runner::SimulationInput;
use risim_vunit_frontend::runner::SimulationPlan;
use risim_vunit_frontend::runner::SimulationReport;
use risim_vunit_frontend::runner::SimulationRequest;
use risim_vunit_frontend::runtime::ProcessLimit;
use risim_vunit_frontend::simulator::RunOptions;
use risim_vunit_frontend::simulator::Simulator;
use risim_vunit_frontend::sources;
use risim_vunit_frontend::sources::SourceCache;
use risim_vunit_frontend::spec::CompilationUnit;
use risim_vunit_frontend::spec::Define;
use risim_vunit_frontend::spec::Feature;
use risim_vunit_frontend::spec::FilePattern;
use risim_vunit_frontend::spec::IncludeDir;
use risim_vunit_frontend::spec::LibrarySpec;
use risim_vunit_frontend::spec::Options;
use risim_vunit_frontend::spec::ProjectSpec;
use risim_vunit_frontend::store::CompileState;
use risim_vunit_frontend::store::FileKey;
use risim_vunit_frontend::store::OutputLayout;
use risim_vunit_frontend::store::ResultStore;
use risim_vunit_frontend::store::TestCounts;
use risim_vunit_frontend::store::TestOutcome;
use risim_vunit_frontend::store::TestOutputPaths;
use risim_vunit_frontend::store::TestResults;
use risim_vunit_frontend::threads::HeldRunner;
use risim_vunit_frontend::threads::Lease;
use risim_vunit_frontend::threads::LeaseEvents;
use risim_vunit_frontend::threads::LeaseFuture;
use risim_vunit_frontend::threads::LeaseProvider;
use risim_vunit_frontend::threads::NoLeases;
use risim_vunit_frontend::threads::ReleaseFuture;
use risim_vunit_frontend::threads::SimulationLimit;
use risim_vunit_frontend::threads::ThreadBudget;
use risim_vunit_frontend::threads::ThreadState;
use risim_vunit_frontend::threads::WaitReason;
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use self::trial::trial;

fn main() -> ExitCode {
    if env::var_os(fake_risim::ENV).is_some() {
        return fake_risim::main();
    }
    // SAFETY: No other threads exist yet. Child processes inherit the variable and act as the
    // fake simulator.
    unsafe {
        env::set_var(fake_risim::ENV, "1");
    }

    let args = Arguments::from_args();
    let trials = vec![
        trial(
            "compiles_in_order_and_incrementally",
            compiles_in_order_and_incrementally,
        ),
        trial("failure_skips_dependents", failure_skips_dependents),
        trial(
            "files_of_a_library_compile_in_one_batch",
            files_of_a_library_compile_in_one_batch,
        ),
        trial(
            "obsolete_reports_recompile_their_files",
            obsolete_reports_recompile_their_files,
        ),
        trial(
            "crash_fails_the_files_without_errors",
            crash_fails_the_files_without_errors,
        ),
        trial("independent_libraries_compile_in_parallel", || {
            parallelism(4, true)
        }),
        trial("semaphore_limits_parallelism", || parallelism(1, false)),
        trial(
            "diagnostics_are_live_and_persisted",
            diagnostics_are_live_and_persisted,
        ),
        trial(
            "unparsable_failure_output_becomes_a_diagnostic",
            unparsable_failure_output,
        ),
        trial(
            "cancel_terminates_the_process_tree",
            cancel_terminates_the_process_tree,
        ),
        trial(
            "simulator_change_recompiles_everything",
            simulator_change_recompiles_everything,
        ),
        trial("spawn_failure_fails_the_file", spawn_failure_fails_the_file),
        trial(
            "dependency_cycle_fails_the_compile",
            dependency_cycle_fails_the_compile,
        ),
        trial(
            "detect_rejects_other_programs",
            detect_rejects_other_programs,
        ),
        trial(
            "simulates_and_records_results",
            simulates_and_records_results,
        ),
        trial(
            "non_zero_exit_code_fails_a_passed_test",
            non_zero_exit_code_fails_a_passed_test,
        ),
        trial("semaphore_limits_simulations", semaphore_limits_simulations),
        trial("tests_share_the_threads", tests_share_the_threads),
        trial(
            "many_tests_get_one_thread_each",
            many_tests_get_one_thread_each,
        ),
        trial(
            "a_test_takes_at_most_its_threads",
            a_test_takes_at_most_its_threads,
        ),
        trial("a_full_budget_reports_waits", a_full_budget_reports_waits),
        trial("the_budget_resizes", the_budget_resizes),
        trial(
            "a_refused_lease_fails_the_test",
            a_refused_lease_fails_the_test,
        ),
        trial(
            "cancel_terminates_and_skips_simulations",
            cancel_terminates_and_skips_simulations,
        ),
        trial(
            "simulation_spawn_failure_fails_the_test",
            simulation_spawn_failure_fails_the_test,
        ),
        trial(
            "run_options_that_set_a_simulation_key_fail_the_test",
            run_options_that_set_a_simulation_key_fail_the_test,
        ),
        trial("real_risim_compiles_uart", real_risim_compiles_uart).with_ignored_flag(
            env::var_os("RISIM_RUNNER").is_none() || env::var_os("RISIM_ANALYZER").is_none(),
        ),
    ];
    let trials = [trials, sv_trials(), held_trials(), workspace::trials()]
        .into_iter()
        .flatten()
        .collect();
    libtest_mimic::run(&args, trials).exit_code()
}

/// The trials of SystemVerilog and Verilog files.
fn sv_trials() -> Vec<Trial> {
    vec![
        trial(
            "sv_files_compile_with_their_library_options",
            sv_files_compile_with_their_library_options,
        ),
        trial(
            "editing_an_include_recompiles_its_includers",
            editing_an_include_recompiles_its_includers,
        ),
        trial(
            "library_mode_compiles_every_sv_file_of_the_library_in_one_call",
            library_mode_compiles_every_sv_file_of_the_library_in_one_call,
        ),
        trial(
            "a_report_in_an_include_goes_to_its_includer",
            a_report_in_an_include_goes_to_its_includer,
        ),
        trial(
            "up_to_date_sv_files_are_not_compiled",
            up_to_date_sv_files_are_not_compiled,
        ),
        trial(
            "editing_an_sv_module_recompiles_it_before_its_vhdl_testbench",
            editing_an_sv_module_recompiles_it_before_its_vhdl_testbench,
        ),
    ]
}

/// The trials of simulators held at the end.
fn held_trials() -> Vec<Trial> {
    vec![
        trial(
            "a_held_end_hands_over_the_simulator",
            a_held_end_hands_over_the_simulator,
        ),
        trial(
            "a_held_simulator_frees_the_process_permit",
            a_held_simulator_frees_the_process_permit,
        ),
        trial(
            "release_held_comes_before_prepare",
            release_held_comes_before_prepare,
        ),
        trial(
            "a_cancel_interrupts_release_held",
            a_cancel_interrupts_release_held,
        ),
        trial(
            "a_provider_without_adoption_terminates_the_simulator",
            a_provider_without_adoption_terminates_the_simulator,
        ),
    ]
}

// -------------------------------------------------------------------------------------------------
// Fixture
// -------------------------------------------------------------------------------------------------

/// The builtins, extracted and parsed once for all trials.
static BUILTINS: LazyLock<Utf8PathBuf> = LazyLock::new(|| {
    let root = Utf8PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("operations-builtins");
    builtins::extract(&root).expect("extract the builtins")
});

static SOURCE_CACHE: LazyLock<Mutex<SourceCache>> =
    LazyLock::new(|| Mutex::new(SourceCache::new()));

struct Workspace {
    _temp: tempfile::TempDir,
    root: Utf8PathBuf,
    layout: OutputLayout,
    spec: ProjectSpec,
    simulator: Simulator,
    /// Limits the compile processes.
    semaphore: Arc<Semaphore>,
    /// The simulation threads.
    threads: ThreadBudget,
    /// The most threads that one test takes.
    max_threads_per_test: NonZeroU32,
    /// Grants the simulations' leases.
    leases: Arc<dyn LeaseProvider>,
    /// The number of processes of a simulation operation, if limited.
    max_parallel: Option<usize>,
}

/// The result of a compile, with the events and when they arrived.
struct Run {
    report: CompileReport,
    events: Vec<(Instant, CompileEvent)>,
}

impl Run {
    fn statuses(&self) -> Vec<(&str, FileStatus)> {
        self.report
            .files
            .iter()
            .map(|(key, status)| (file_name(key.as_str()), *status))
            .collect()
    }

    fn with_status(&self, status: FileStatus) -> Vec<&str> {
        self.statuses()
            .into_iter()
            .filter(|&(_, file_status)| file_status == status)
            .map(|(name, _)| name)
            .collect()
    }
}

/// The result of a simulation, with its events.
struct SimRun {
    report: SimulationReport,
    events: Vec<SimulationEvent>,
}

impl SimRun {
    fn outcomes(&self) -> Vec<(&str, TestOutcome)> {
        self.report
            .outcomes
            .iter()
            .map(|(name, outcome)| (name.as_str(), *outcome))
            .collect()
    }

    /// The events without diagnostics and thread states, as `(kind, testcase)`.
    fn event_names(&self) -> Vec<(&'static str, String)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                SimulationEvent::Started { .. } => Some(("started", String::new())),
                SimulationEvent::TestStarted { name, .. } => Some(("test_started", name.clone())),
                SimulationEvent::TestFinished { name, .. } => Some(("test_finished", name.clone())),
                SimulationEvent::Finished { .. } => Some(("finished", String::new())),
                SimulationEvent::Diagnostic { .. } | SimulationEvent::TestThreads { .. } => None,
            })
            .collect()
    }

    /// The thread states of the testcases, in the order they were reported.
    fn thread_states(&self) -> Vec<(&str, &ThreadState)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                SimulationEvent::TestThreads { name, state } => Some((name.as_str(), state)),
                _ => None,
            })
            .collect()
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl Workspace {
    async fn new() -> Self {
        let temp = tempfile::tempdir().expect("create a temporary directory");
        let root = Utf8Path::from_path(temp.path())
            .expect("UTF-8 path")
            .to_owned();
        let exe = Utf8PathBuf::from_path_buf(env::current_exe().expect("current exe"))
            .expect("UTF-8 path");
        let simulator = Simulator::detect(&exe, &exe)
            .await
            .expect("detect the fake");
        Self {
            _temp: temp,
            root: root.clone(),
            layout: OutputLayout::new(&root),
            spec: ProjectSpec::new(),
            simulator,
            semaphore: Arc::new(Semaphore::new(4)),
            threads: ThreadBudget::new(threads(4)),
            max_threads_per_test: NonZeroU32::MIN,
            leases: Arc::new(NoLeases),
            max_parallel: None,
        }
    }

    fn write(&self, rel_path: &str, contents: &str) -> Utf8PathBuf {
        let path = self.root.join(rel_path);
        fs::create_dir_all(path.parent().expect("parent")).expect("create directories");
        fs::write(&path, contents).expect("write the file");
        path
    }

    /// `lib`: a package, an entity using it, an unused entity, and a testbench.
    fn standard_project(&mut self) {
        self.write("src/pkg.vhd", "package pkg is end package;\n");
        self.write(
            "src/ent.vhd",
            "use work.pkg.all;\nentity ent is end entity;\n\
             architecture rtl of ent is begin end architecture;\n",
        );
        self.write("src/unused.vhd", "entity unused is end entity;\n");
        self.write(
            "src/tb_top.vhd",
            &testbench("tb_top", "dut: entity work.ent;"),
        );
        self.spec.add_library("lib", ["src/*.vhd"]);
    }

    /// Libraries `a` and `b` with one package each, and a testbench in `tb` using both.
    fn diamond_project(&mut self, a_code: &str, b_code: &str) {
        self.write(
            "a/a_pkg.vhd",
            &format!("{a_code}\npackage a_pkg is end package;\n"),
        );
        self.write(
            "b/b_pkg.vhd",
            &format!("{b_code}\npackage b_pkg is end package;\n"),
        );
        self.write(
            "tb/tb_ab.vhd",
            &format!(
                "library a, b;\nuse a.a_pkg.all;\nuse b.b_pkg.all;\n{}",
                testbench("tb_ab", "")
            ),
        );
        self.spec
            .add_library("a", ["a/*.vhd"])
            .add_library("b", ["b/*.vhd"])
            .add_library("tb", ["tb/*.vhd"]);
    }

    /// Adds the library `name` with the files of `patterns` and the options `options`.
    fn add_library_with_options(&mut self, name: &str, patterns: &[&str], options: Options) {
        self.spec.libraries.push(LibrarySpec::Sources {
            name: name.to_owned(),
            files: patterns
                .iter()
                .map(|&pattern| FilePattern::new(pattern))
                .collect(),
            vhdl_standard: None,
            options,
        });
    }

    /// Returns and clears the arguments of the `analyze` calls of the fake simulator, by
    /// library.
    fn take_args(&self) -> Vec<(String, Vec<String>)> {
        let path = self.root.join(fake_risim::ARGS);
        let log = fs::read_to_string(&path).unwrap_or_default();
        let _ignored = fs::remove_file(&path);
        log.lines()
            .map(|line| {
                let args: Vec<String> = serde_json::from_str(line).expect("the arguments");
                let work = args
                    .iter()
                    .find_map(|arg| arg.strip_prefix("--work="))
                    .expect("--work")
                    .to_owned();
                (work, args)
            })
            .collect()
    }

    async fn compile(&self, state: &mut CompileState) -> Run {
        self.compile_until(state, |_| false).await
    }

    /// Compiles, cancelling as soon as an event satisfies `cancel_on`.
    async fn compile_until(
        &self,
        state: &mut CompileState,
        cancel_on: impl Fn(&CompileEvent) -> bool + Send + 'static,
    ) -> Run {
        let loaded = {
            let mut cache = SOURCE_CACHE.lock().expect("cache lock");
            sources::build_project(&self.root, &self.spec, &BUILTINS, &mut cache)
        };
        assert_eq!(loaded.config_diagnostics, [], "config diagnostics");
        let found = discovery::discover(&loaded.project, &self.spec.test_configs);
        let targets = compile::targets(&loaded.project, &found);

        let (events, mut receiver) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let collector = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut collected = Vec::new();
                while let Some(event) = receiver.recv().await {
                    if cancel_on(&event) {
                        cancel.cancel();
                    }
                    collected.push((Instant::now(), event));
                }
                collected
            })
        };
        let context = CompileContext {
            workspace_root: self.root.clone(),
            limit: ProcessLimit::from(Arc::clone(&self.semaphore)),
            events,
            cancel,
        };
        let input = PlanInput {
            layout: &self.layout,
            project: &loaded.project,
            targets: &targets,
            simulator: &self.simulator,
        };
        let report = compile::compile(&input, state, &context).await;
        drop(context);
        Run {
            report,
            events: collector.await.expect("collector"),
        }
    }

    /// Discovers the testcases and simulates those matching `requests` (`(pattern, gui)`),
    /// cancelling as soon as an event satisfies `cancel_on`.
    async fn simulate_until(
        &self,
        requests: &[(&str, bool)],
        results: &Arc<ResultStore>,
        cancel_on: impl Fn(&SimulationEvent) -> bool + Send + 'static,
    ) -> SimRun {
        let loaded = {
            let mut cache = SOURCE_CACHE.lock().expect("cache lock");
            sources::build_project(&self.root, &self.spec, &BUILTINS, &mut cache)
        };
        assert_eq!(loaded.config_diagnostics, [], "config diagnostics");
        let found = discovery::discover(&loaded.project, &self.spec.test_configs);
        let requests: Vec<SimulationRequest> = requests
            .iter()
            .map(|&(pattern, gui)| SimulationRequest {
                pattern: pattern.to_owned(),
                gui,
                ..SimulationRequest::default()
            })
            .collect();
        let plan = SimulationPlan::new(
            &SimulationInput {
                layout: &self.layout,
                project: &loaded.project,
                discovery: &found,
                sim_options: &self.spec.sim_options,
            },
            &requests,
        );

        let (events, mut receiver) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let collector = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut collected = Vec::new();
                while let Some(event) = receiver.recv().await {
                    if cancel_on(&event) {
                        cancel.cancel();
                    }
                    collected.push(event);
                }
                collected
            })
        };
        let context = SimulationContext {
            workspace_root: self.root.clone(),
            simulator: self.simulator.clone(),
            run_defaults: Arc::default(),
            limit: SimulationLimit::new(
                self.threads.clone(),
                self.max_parallel
                    .map(|processes| Arc::new(Semaphore::new(processes))),
                None,
            ),
            max_threads_per_test: self.max_threads_per_test,
            leases: Arc::clone(&self.leases),
            results: Arc::clone(results),
            testcase_locks: Arc::default(),
            events: events.into(),
            cancel,
        };
        let report = runner::simulate(plan, &context).await;
        drop(context);
        SimRun {
            report,
            events: collector.await.expect("collector"),
        }
    }

    async fn simulate(&self, requests: &[(&str, bool)], results: &Arc<ResultStore>) -> SimRun {
        self.simulate_until(requests, results, |_| false).await
    }

    /// Adds a directive for `testcase` to the fake simulator.
    fn sim_directive(&self, testcase: &str, directive: &str) {
        let path = self.root.join(fake_risim::SIM_DIRECTIVES);
        let mut directives = fs::read_to_string(&path).unwrap_or_default();
        for part in [testcase, "\t", directive, "\n"] {
            directives.push_str(part);
        }
        fs::write(&path, directives).expect("write the directives");
    }

    /// Returns and clears the log of the `analyze` calls of the fake simulator.
    fn take_calls(&self) -> Vec<String> {
        let path = self.root.join(fake_risim::CALLS);
        let log = fs::read_to_string(&path).unwrap_or_default();
        let _ignored = fs::remove_file(&path);
        log.lines().map(str::to_owned).collect()
    }

    /// Returns and clears the log of the fake simulator.
    fn take_log(&self) -> Vec<String> {
        let path = self.root.join(fake_risim::LOG);
        let log = fs::read_to_string(&path).unwrap_or_default();
        let _ignored = fs::remove_file(&path);
        log.lines().map(str::to_owned).collect()
    }
}

/// A thread count.
const fn threads(count: u32) -> NonZeroU32 {
    NonZeroU32::new(count).expect("non-zero")
}

/// The run options of the last simulation of `testcase` by the fake simulator, from its
/// `options.json`.
fn run_options(layout: &OutputLayout, testcase: &str) -> Value {
    let paths = TestOutputPaths::new(layout, testcase);
    let options = fs::read_to_string(paths.dir.join("options.json")).expect("read options.json");
    serde_json::from_str(&options).expect("the run options")
}

fn testbench(name: &str, body: &str) -> String {
    format!(
        "entity {name} is generic (runner_cfg : string); end entity;\n\
         architecture tb of {name} is\nbegin\n  {body}\n  main : process\n  begin\n    \
         test_runner_setup(runner, runner_cfg);\n    wait;\n  end process;\nend architecture;\n"
    )
}

// -------------------------------------------------------------------------------------------------
// Trials
// -------------------------------------------------------------------------------------------------

async fn compiles_in_order_and_incrementally() {
    let mut workspace = Workspace::new().await;
    workspace.standard_project();
    let mut state = CompileState::load(&workspace.layout);
    let first = workspace.compile(&mut state).await;
    assert_eq!(first.report.status, CompileStatus::Succeeded);
    assert_eq!(
        first.statuses(),
        [
            ("pkg.vhd", FileStatus::Compiled),
            ("ent.vhd", FileStatus::Compiled),
            ("tb_top.vhd", FileStatus::Compiled),
        ]
    );
    assert_eq!(
        workspace.take_log(),
        [
            "start lib pkg.vhd",
            "end lib pkg.vhd",
            "start lib ent.vhd",
            "end lib ent.vhd",
            "start lib tb_top.vhd",
            "end lib tb_top.vhd",
        ]
    );
    assert_eq!(first.events[0].1, CompileEvent::Started { total: 3 });
    let indices: Vec<(usize, usize)> = first
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            CompileEvent::FileStarted { index, total, .. } => Some((*index, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(indices, [(1, 3), (2, 3), (3, 3)]);
    assert!(workspace.layout.library_dir("lib").is_dir());
    assert!(
        workspace
            .layout
            .compile_output_file("lib", &workspace.root.join("src/pkg.vhd"))
            .is_file()
    );

    // The saved state makes everything up to date.
    let mut reloaded = CompileState::load(&workspace.layout);
    assert_eq!(reloaded.files.len(), 3);
    let second = workspace.compile(&mut reloaded).await;
    assert_eq!(second.report.status, CompileStatus::Succeeded);
    assert_eq!(
        second.with_status(FileStatus::UpToDate),
        ["pkg.vhd", "ent.vhd", "tb_top.vhd"]
    );
    assert_eq!(workspace.take_log(), Vec::<String>::new());

    // Changing the entity recompiles it and the testbench, but not the package.
    workspace.write(
        "src/ent.vhd",
        "use work.pkg.all;\n-- changed\nentity ent is end entity;\n\
         architecture rtl of ent is begin end architecture;\n",
    );
    let third = workspace.compile(&mut reloaded).await;
    assert_eq!(
        third.with_status(FileStatus::Compiled),
        ["ent.vhd", "tb_top.vhd"]
    );
    assert_eq!(third.with_status(FileStatus::UpToDate), ["pkg.vhd"]);
}

async fn failure_skips_dependents() {
    let mut workspace = Workspace::new().await;
    workspace.diamond_project("-- fake: error broken package", "");
    let mut state = CompileState::default();
    let failed = workspace.compile(&mut state).await;
    assert_eq!(failed.report.status, CompileStatus::Failed);
    assert_eq!(
        failed.statuses(),
        [
            ("a_pkg.vhd", FileStatus::Failed),
            ("b_pkg.vhd", FileStatus::Compiled),
            ("tb_ab.vhd", FileStatus::Skipped),
        ]
    );
    let a_file = workspace.root.join("a/a_pkg.vhd");
    let (a_key, diagnostics) = failed
        .report
        .diagnostics
        .iter()
        .find(|(candidate, _)| candidate.as_str().ends_with("a_pkg.vhd"))
        .expect("diagnostics of a_pkg.vhd");
    assert_eq!(a_key.as_str(), format!("a:{a_file}"));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, Severity::Error);
    assert_eq!(diagnostics[0].message, "broken package");
    assert_eq!(diagnostics[0].file.as_ref(), Some(&a_file));
    assert_eq!(diagnostics[0].range.map(|range| range.start.line), Some(1));
    assert!(failed.events.iter().any(|(_, event)| matches!(
        event,
        CompileEvent::FileFinished { key, status: FileStatus::Skipped, .. }
            if key.as_str().ends_with("tb_ab.vhd")
    )));
    let stored: Vec<&str> = state
        .files
        .keys()
        .map(|key| file_name(key.as_str()))
        .collect();
    assert_eq!(stored, ["b_pkg.vhd"]);

    // After the fix, only the failed file and its dependents are compiled.
    workspace.write("a/a_pkg.vhd", "package a_pkg is end package;\n");
    let fixed = workspace.compile(&mut state).await;
    assert_eq!(fixed.report.status, CompileStatus::Succeeded);
    assert_eq!(
        fixed.with_status(FileStatus::Compiled),
        ["a_pkg.vhd", "tb_ab.vhd"]
    );
    assert_eq!(fixed.with_status(FileStatus::UpToDate), ["b_pkg.vhd"]);
    assert!(fixed.report.diagnostics.values().all(Vec::is_empty));
}

async fn files_of_a_library_compile_in_one_batch() {
    let mut workspace = Workspace::new().await;
    workspace.standard_project();
    workspace.write(
        "src/pkg.vhd",
        "-- fake: warning first\n-- fake: error broken\npackage pkg is end package;\n",
    );
    workspace.write(
        "src/ent.vhd",
        "use work.pkg.all;\n-- fake: error depends on the broken one\nentity ent is end entity;\n\
         architecture rtl of ent is begin end architecture;\n",
    );
    let mut state = CompileState::default();
    let failed = workspace.compile(&mut state).await;
    assert_eq!(
        failed.statuses(),
        [
            ("pkg.vhd", FileStatus::Failed),
            ("ent.vhd", FileStatus::Skipped),
            ("tb_top.vhd", FileStatus::Skipped),
        ]
    );
    assert_eq!(workspace.take_calls(), ["lib pkg.vhd ent.vhd tb_top.vhd"]);
    // The reports go to their files; those of skipped files are dropped.
    let messages: Vec<(&str, &str)> = failed
        .report
        .diagnostics
        .iter()
        .flat_map(|(key, diagnostics)| {
            diagnostics
                .iter()
                .map(move |diagnostic| (file_name(key.as_str()), diagnostic.message.as_str()))
        })
        .collect();
    assert_eq!(messages, [("pkg.vhd", "first"), ("pkg.vhd", "broken")]);
    let started: Vec<(&str, usize)> = failed
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            CompileEvent::FileStarted { file, index, .. } => {
                Some((file_name(file.as_str()), *index))
            },
            _ => None,
        })
        .collect();
    assert_eq!(started, [("pkg.vhd", 1), ("ent.vhd", 2), ("tb_top.vhd", 3)]);

    // Incremental compiles batch only the files that need compiling.
    workspace.write("src/pkg.vhd", "package pkg is end package;\n");
    workspace.write(
        "src/ent.vhd",
        "use work.pkg.all;\nentity ent is end entity;\n\
         architecture rtl of ent is begin end architecture;\n",
    );
    let fixed = workspace.compile(&mut state).await;
    assert_eq!(fixed.report.status, CompileStatus::Succeeded);
    assert_eq!(workspace.take_calls(), ["lib pkg.vhd ent.vhd tb_top.vhd"]);
    workspace.write("src/pkg.vhd", "package pkg is end package;\n-- changed\n");
    workspace.compile(&mut state).await;
    assert_eq!(workspace.take_calls(), ["lib pkg.vhd ent.vhd tb_top.vhd"]);
    workspace.write(
        "src/tb_top.vhd",
        &format!(
            "-- changed\n{}",
            testbench("tb_top", "dut: entity work.ent;")
        ),
    );
    workspace.compile(&mut state).await;
    assert_eq!(workspace.take_calls(), ["lib tb_top.vhd"]);
}

/// A report that a batch made a design unit of a file outside the batch obsolete is dropped,
/// and that file loses its stored key, so that it is compiled again.
async fn obsolete_reports_recompile_their_files() {
    let mut workspace = Workspace::new().await;
    workspace.standard_project();
    let mut state = CompileState::default();
    workspace.compile(&mut state).await;
    // `unused.vhd` is outside the compile set; it was compiled by an earlier compile set.
    let unused = FileKey::new("lib", &workspace.root.join("src/unused.vhd"));
    let stored = state.files.values().next().expect("a stored file").clone();
    state.files.insert(unused.clone(), stored);

    workspace.write(
        "src/pkg.vhd",
        "-- fake: obsolete src/unused.vhd
package pkg is end package;
",
    );
    let compiled = workspace.compile(&mut state).await;
    assert_eq!(compiled.report.status, CompileStatus::Succeeded);
    assert!(
        compiled.report.diagnostics.values().all(Vec::is_empty),
        "{:?}",
        compiled.report.diagnostics
    );
    assert!(!state.files.contains_key(&unused));
    assert_eq!(state.files.len(), 3);
}

/// A batch whose process ends with another status than that of reported errors, such as a crash,
/// fails its files without errors too.
async fn crash_fails_the_files_without_errors() {
    let mut workspace = Workspace::new().await;
    workspace.write(
        "src/a.vhd",
        "-- fake: error broken
package a is end package;
",
    );
    workspace.write(
        "src/b.vhd",
        "-- fake: exit 101
package b is end package;
",
    );
    workspace.write(
        "src/tb_ab.vhd",
        &format!(
            "use work.a.all;
use work.b.all;
{}",
            testbench("tb_ab", "")
        ),
    );
    workspace.spec.add_library("lib", ["src/*.vhd"]);
    let mut state = CompileState::default();
    let failed = workspace.compile(&mut state).await;
    assert_eq!(workspace.take_calls(), ["lib a.vhd b.vhd tb_ab.vhd"]);
    assert_eq!(
        failed.statuses(),
        [
            ("a.vhd", FileStatus::Failed),
            ("b.vhd", FileStatus::Failed),
            ("tb_ab.vhd", FileStatus::Skipped),
        ]
    );
    let b_messages: Vec<&str> = failed
        .report
        .diagnostics
        .iter()
        .filter(|(key, _)| key.as_str().ends_with("b.vhd"))
        .flat_map(|(_, diagnostics)| {
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
        })
        .collect();
    assert_eq!(b_messages.len(), 1, "{b_messages:?}");
    assert!(
        b_messages[0].contains("risim analyze failed"),
        "{b_messages:?}"
    );
    assert!(state.files.is_empty());
}

async fn parallelism(permits: usize, expect_overlap: bool) {
    let mut workspace = Workspace::new().await;
    workspace.semaphore = Arc::new(Semaphore::new(permits));
    workspace.diamond_project("-- fake: sleep 400", "-- fake: sleep 400");
    let run = workspace.compile(&mut CompileState::default()).await;
    assert_eq!(run.report.status, CompileStatus::Succeeded);
    let log = workspace.take_log();
    assert_eq!(log.len(), 6, "{log:?}");
    let overlapping = log[0].starts_with("start") && log[1].starts_with("start");
    assert_eq!(overlapping, expect_overlap, "{log:?}");
    assert_eq!(log[4], "start tb tb_ab.vhd", "{log:?}");
}

async fn diagnostics_are_live_and_persisted() {
    let mut workspace = Workspace::new().await;
    workspace.write(
        "src/pkg.vhd",
        "-- fake: warning look here\n-- fake: sleep 600\npackage pkg is end package;\n",
    );
    workspace.write(
        "src/tb_pkg.vhd",
        &format!("use work.pkg.all;\n{}", testbench("tb_pkg", "")),
    );
    workspace.spec.add_library("lib", ["src/*.vhd"]);
    let mut state = CompileState::default();
    let first = workspace.compile(&mut state).await;
    assert_eq!(first.report.status, CompileStatus::Succeeded);

    let diagnostic_time = first
        .events
        .iter()
        .find_map(|(time, event)| {
            matches!(event, CompileEvent::Diagnostic { diagnostic, .. }
                if diagnostic.message == "look here")
            .then_some(*time)
        })
        .expect("live diagnostic");
    let finished_time = first
        .events
        .iter()
        .find_map(|(time, event)| {
            matches!(event, CompileEvent::FileFinished { key, .. }
                if key.as_str().ends_with("pkg.vhd"))
            .then_some(*time)
        })
        .expect("finished event");
    assert!(
        finished_time.duration_since(diagnostic_time) >= Duration::from_millis(300),
        "the diagnostic arrived only {:?} before the end",
        finished_time.duration_since(diagnostic_time)
    );

    // Warnings of compiled files survive in the state, also across a reload.
    let mut reloaded = CompileState::load(&workspace.layout);
    let second = workspace.compile(&mut reloaded).await;
    assert_eq!(
        second.with_status(FileStatus::UpToDate),
        ["pkg.vhd", "tb_pkg.vhd"]
    );
    let warnings: Vec<&str> = second
        .report
        .diagnostics
        .values()
        .flatten()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(warnings, ["look here"]);
}

async fn unparsable_failure_output() {
    let mut workspace = Workspace::new().await;
    workspace.write(
        "src/pkg.vhd",
        "-- fake: warning only a warning\n-- fake: output something odd happened\n\
         -- fake: exit 3\npackage pkg is end package;\n",
    );
    workspace.write(
        "src/tb_pkg.vhd",
        &format!("use work.pkg.all;\n{}", testbench("tb_pkg", "")),
    );
    workspace.spec.add_library("lib", ["src/*.vhd"]);
    let run = workspace.compile(&mut CompileState::default()).await;
    assert_eq!(run.report.status, CompileStatus::Failed);
    let pkg = workspace.root.join("src/pkg.vhd");
    let diagnostics = run.report.diagnostics.values().next().expect("diagnostics");
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(diagnostics[1].severity, Severity::Error);
    assert!(diagnostics[1].message.contains("something odd happened"));
    assert_eq!(diagnostics[1].file.as_ref(), Some(&pkg));
    assert_eq!(diagnostics[1].range, None);
}

async fn cancel_terminates_the_process_tree() {
    let mut workspace = Workspace::new().await;
    let pid_file = workspace.root.join("sleeper.pid");
    workspace.standard_project();
    workspace.write(
        "src/pkg.vhd",
        &format!("-- fake: spawn-sleeper {pid_file}\n-- fake: hang\npackage pkg is end package;\n"),
    );
    let mut state = CompileState::default();
    // Times the cancellation alone, not the loading and planning before it, which can be slow
    // when many tests run at once.
    let cancelled_at = Arc::new(OnceLock::new());
    let run = workspace
        .compile_until(&mut state, {
            let cancelled_at = Arc::clone(&cancelled_at);
            move |event| {
                let cancel = matches!(event, CompileEvent::FileStarted { file, .. } if file.ends_with("pkg.vhd"));
                if cancel {
                    cancelled_at.get_or_init(Instant::now);
                }
                cancel
            }
        })
        .await;
    let cancelled_at = cancelled_at.get().expect("the hanging compile started");
    assert!(cancelled_at.elapsed() < Duration::from_secs(10));
    assert_eq!(run.report.status, CompileStatus::Cancelled);
    assert_eq!(
        run.statuses(),
        [
            ("pkg.vhd", FileStatus::Cancelled),
            ("ent.vhd", FileStatus::NotStarted),
            ("tb_top.vhd", FileStatus::NotStarted),
        ]
    );
    assert!(state.files.is_empty());
    assert!(CompileState::load(&workspace.layout).files.is_empty());

    #[cfg(unix)]
    {
        // The hanging process may be killed before it spawns the sleeper.
        if let Ok(pid) = fs::read_to_string(&pid_file) {
            let alive = || {
                std::process::Command::new("kill")
                    .args(["-0", pid.trim()])
                    .status()
                    .is_ok_and(|status| status.success())
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            while alive() {
                assert!(Instant::now() < deadline, "the sleeper {pid} survived");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

async fn simulator_change_recompiles_everything() {
    let mut workspace = Workspace::new().await;
    workspace.standard_project();
    let mut state = CompileState::default();
    workspace.compile(&mut state).await;
    let marker = workspace.layout.library_dir("lib").join("marker");
    fs::write(&marker, "").expect("write marker");

    let mut identity = workspace.simulator.identity().clone();
    identity.version_output.push_str("rebuilt\n");
    workspace.simulator = Simulator::from_identity(identity).expect("valid identity");
    let run = workspace.compile(&mut state).await;
    assert_eq!(
        run.with_status(FileStatus::Compiled),
        ["pkg.vhd", "ent.vhd", "tb_top.vhd"]
    );
    assert!(!marker.exists());
    assert_eq!(
        state.simulator.as_ref(),
        Some(workspace.simulator.identity())
    );
}

async fn spawn_failure_fails_the_file() {
    let mut workspace = Workspace::new().await;
    workspace.standard_project();
    let mut identity = workspace.simulator.identity().clone();
    identity.analyzer = workspace.root.join("missing-risim");
    workspace.simulator = Simulator::from_identity(identity).expect("valid identity");
    let run = workspace.compile(&mut CompileState::default()).await;
    assert_eq!(run.report.status, CompileStatus::Failed);
    assert_eq!(
        run.statuses(),
        [
            ("pkg.vhd", FileStatus::Failed),
            ("ent.vhd", FileStatus::Skipped),
            ("tb_top.vhd", FileStatus::Skipped),
        ]
    );
    let message = &run.report.diagnostics.values().next().expect("diagnostics")[0].message;
    assert!(
        message.starts_with("failed to run risim analyze"),
        "{message}"
    );
}

async fn dependency_cycle_fails_the_compile() {
    let mut workspace = Workspace::new().await;
    workspace.write("src/a.vhd", "use work.b.all;\npackage a is end package;\n");
    workspace.write("src/b.vhd", "use work.a.all;\npackage b is end package;\n");
    workspace.write(
        "src/tb_a.vhd",
        &format!("use work.a.all;\n{}", testbench("tb_a", "")),
    );
    workspace.spec.add_library("lib", ["src/*.vhd"]);
    let run = workspace.compile(&mut CompileState::default()).await;
    assert_eq!(run.report.status, CompileStatus::Failed);
    assert!(run.report.files.is_empty());
    assert_eq!(run.report.project_diagnostics.len(), 1);
    assert!(
        run.report.project_diagnostics[0]
            .message
            .contains("circular dependency")
    );
    assert_eq!(workspace.take_log(), Vec::<String>::new());
}

async fn detect_rejects_other_programs() {
    let missing = Simulator::detect(
        Utf8Path::new("/nonexistent/risim-runner"),
        Utf8Path::new("/nonexistent/risim"),
    )
    .await;
    missing.expect_err("the executable doesn't exist");
    #[cfg(unix)]
    {
        let error = Simulator::detect(Utf8Path::new("/bin/sh"), Utf8Path::new("/bin/sh"))
            .await
            .expect_err("sh isn't risim-runner");
        assert!(
            error.to_string().contains("risim-runner version"),
            "{error}"
        );
    }
}

/// The SystemVerilog files compile with the arguments of their library, after `vunit_lib`,
/// whose `vunit_pkg.sv` the project gets; a VHDL-only library gets no SystemVerilog arguments.
async fn sv_files_compile_with_their_library_options() {
    let mut workspace = Workspace::new().await;
    workspace.write("inc/defs.svh", "");
    workspace.write("rtl/counter.sv", "module counter;\nendmodule\n");
    workspace.write("tb/tb_top.vhd", &testbench("tb_top", ""));
    workspace.add_library_with_options(
        "rtl",
        &["rtl/*.sv"],
        Options {
            include_dirs: Some(vec![IncludeDir::new("inc")]),
            defines: [("W".to_owned(), Define::text("8"))].into(),
            ..Options::default()
        },
    );
    workspace.spec.add_library("tb", ["tb/*.vhd"]);
    let mut state = CompileState::default();
    let run = workspace.compile(&mut state).await;
    assert_eq!(run.report.status, CompileStatus::Succeeded);
    assert_eq!(
        run.statuses(),
        [
            ("vunit_pkg.sv", FileStatus::Compiled),
            ("counter.sv", FileStatus::Compiled),
            ("tb_top.vhd", FileStatus::Compiled),
        ]
    );
    let calls = workspace.take_args();
    let libraries: Vec<&str> = calls.iter().map(|(work, _)| work.as_str()).collect();
    let position = |library: &str| libraries.iter().position(|&work| work == library);
    assert!(position("vunit_lib") < position("rtl"), "{libraries:?}");
    let sv_args = |library: &str| -> Vec<&str> {
        let (_, args) = &calls[position(library).expect("a call")];
        args.iter()
            .map(String::as_str)
            .filter(|arg| {
                ["--include-dir", "--define", "--sv-library"]
                    .iter()
                    .any(|prefix| arg.starts_with(prefix))
            })
            .collect()
    };
    assert_eq!(
        sv_args("rtl"),
        [
            format!("--include-dir={}", BUILTINS.join("verilog/include")).as_str(),
            &format!("--include-dir={}", workspace.root.join("inc")),
            "--define=W=8",
            "--sv-library=vunit_lib",
        ]
    );
    assert_eq!(sv_args("tb"), Vec::<&str>::new());
    assert_eq!(sv_args("vunit_lib"), Vec::<&str>::new());
}

/// Editing an include file recompiles the files that include it, and no other.
async fn editing_an_include_recompiles_its_includers() {
    let mut workspace = Workspace::new().await;
    workspace.write("rtl/defs.svh", "`define W 8\n");
    workspace.write("rtl/a.sv", "`include \"defs.svh\"\nmodule a;\nendmodule\n");
    workspace.write("rtl/b.sv", "module b;\nendmodule\n");
    workspace.spec.add_library("rtl", ["rtl/*.sv"]);
    let mut state = CompileState::default();
    let first = workspace.compile(&mut state).await;
    assert_eq!(first.report.status, CompileStatus::Succeeded);
    assert_eq!(
        workspace.take_calls(),
        ["vunit_lib vunit_pkg.sv", "rtl a.sv b.sv"]
    );

    workspace.write("rtl/defs.svh", "`define W 16\n");
    let edited = workspace.compile(&mut state).await;
    assert_eq!(edited.with_status(FileStatus::Compiled), ["a.sv"]);
    assert_eq!(workspace.take_calls(), ["rtl a.sv"]);
}

/// In `library` mode, a change of one file compiles all SystemVerilog files of the library in
/// one call, and a failed dependency of one of them skips all of them.
async fn library_mode_compiles_every_sv_file_of_the_library_in_one_call() {
    let mut workspace = Workspace::new().await;
    workspace.write("pkgs/p.sv", "package p;\nendpackage\n");
    workspace.write("rtl/a.sv", "module a;\n  import p::*;\nendmodule\n");
    workspace.write("rtl/b.sv", "module b;\nendmodule\n");
    workspace.write("rtl/c.sv", "module c;\nendmodule\n");
    workspace.spec.add_library("pkgs", ["pkgs/*.sv"]);
    workspace.add_library_with_options(
        "rtl",
        &["rtl/*.sv"],
        Options {
            compilation_unit: Some(CompilationUnit::Library),
            ..Options::default()
        },
    );
    let mut state = CompileState::default();
    let first = workspace.compile(&mut state).await;
    assert_eq!(first.report.status, CompileStatus::Succeeded);
    assert_eq!(
        workspace.take_calls(),
        ["vunit_lib vunit_pkg.sv", "pkgs p.sv", "rtl a.sv b.sv c.sv"]
    );

    workspace.write("rtl/b.sv", "module b;\n  // changed\nendmodule\n");
    let edited = workspace.compile(&mut state).await;
    assert_eq!(
        edited.with_status(FileStatus::Compiled),
        ["a.sv", "b.sv", "c.sv"]
    );
    assert_eq!(workspace.take_calls(), ["rtl a.sv b.sv c.sv"]);

    workspace.write(
        "pkgs/p.sv",
        "// -- fake: error broken\npackage p;\nendpackage\n",
    );
    let failed = workspace.compile(&mut state).await;
    assert_eq!(failed.with_status(FileStatus::Failed), ["p.sv"]);
    assert_eq!(
        failed.with_status(FileStatus::Skipped),
        ["a.sv", "b.sv", "c.sv"]
    );
    assert_eq!(workspace.take_calls(), ["pkgs p.sv"]);
}

/// A report in an include file goes to the batch member that includes it, also when that isn't
/// the first one, and keeps its location in the include file.
async fn a_report_in_an_include_goes_to_its_includer() {
    let mut workspace = Workspace::new().await;
    workspace.write("rtl/defs.svh", "`define W 8\n");
    workspace.write("rtl/a.sv", "module a;\nendmodule\n");
    workspace.write(
        "rtl/b.sv",
        "`include \"defs.svh\"\n// -- fake: include-error rtl/defs.svh broken\nmodule b;\nendmodule\n",
    );
    workspace.spec.add_library("rtl", ["rtl/*.sv"]);
    let mut state = CompileState::default();
    let run = workspace.compile(&mut state).await;
    assert_eq!(
        workspace.take_calls(),
        ["vunit_lib vunit_pkg.sv", "rtl a.sv b.sv"]
    );
    assert_eq!(
        run.with_status(FileStatus::Compiled),
        ["vunit_pkg.sv", "a.sv"]
    );
    assert_eq!(run.with_status(FileStatus::Failed), ["b.sv"]);
    let diagnostics = |name: &str| {
        let key = FileKey::new("rtl", &workspace.root.join("rtl").join(name));
        run.report
            .diagnostics
            .get(&key)
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(diagnostics("a.sv"), []);
    let reported = diagnostics("b.sv");
    let located: Vec<(&str, Severity, Option<&str>)> = reported
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.message.as_str(),
                diagnostic.severity,
                diagnostic
                    .file
                    .as_deref()
                    .map(|file| file_name(file.as_str())),
            )
        })
        .collect();
    assert_eq!(
        located,
        [
            ("broken", Severity::Error, Some("defs.svh")),
            ("included from here", Severity::Note, Some("b.sv")),
        ]
    );
}

/// SystemVerilog files whose compile keys are unchanged aren't compiled again.
async fn up_to_date_sv_files_are_not_compiled() {
    let mut workspace = Workspace::new().await;
    workspace.write("rtl/a.sv", "module a;\nendmodule\n");
    workspace.spec.add_library("rtl", ["rtl/*.sv"]);
    let mut state = CompileState::load(&workspace.layout);
    workspace.compile(&mut state).await;
    assert_eq!(workspace.take_calls().len(), 2);
    let mut reloaded = CompileState::load(&workspace.layout);
    let second = workspace.compile(&mut reloaded).await;
    assert_eq!(second.report.status, CompileStatus::Succeeded);
    assert_eq!(
        second.with_status(FileStatus::UpToDate),
        ["vunit_pkg.sv", "a.sv"]
    );
    assert_eq!(workspace.take_calls(), Vec::<String>::new());
}

/// A VHDL testbench whose component binds to an SV module of another library, which
/// instantiates a module of a third library: `rtl` searches `ip`, and editing either SV file
/// compiles it again before the testbench runs.
async fn editing_an_sv_module_recompiles_it_before_its_vhdl_testbench() {
    let mut workspace = Workspace::new().await;
    workspace.write("ip/leaf.sv", "module leaf;\nendmodule\n");
    workspace.write(
        "rtl/counter.sv",
        "module counter;\n  leaf u_leaf();\nendmodule\n",
    );
    workspace.write(
        "tb/tb_counter.vhd",
        &format!(
            "library rtl;\nuse rtl.all;\n{}",
            testbench("tb_counter", "dut : component counter;").replacen(
                "begin\n",
                "  component counter is end component;\nbegin\n",
                1
            )
        ),
    );
    workspace.spec.add_library("ip", ["ip/*.sv"]);
    workspace.spec.add_library("rtl", ["rtl/*.sv"]);
    workspace.spec.add_library("tb", ["tb/*.vhd"]);
    let mut state = CompileState::default();
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let first = workspace.compile(&mut state).await;
    assert_eq!(first.report.status, CompileStatus::Succeeded);
    let calls = workspace.take_args();
    let (_, rtl_args) = calls
        .iter()
        .find(|(work, _)| work == "rtl")
        .expect("a call of rtl");
    let sv_libraries: Vec<&str> = rtl_args
        .iter()
        .map(String::as_str)
        .filter(|arg| arg.starts_with("--sv-library"))
        .collect();
    assert_eq!(sv_libraries, ["--sv-library=ip", "--sv-library=vunit_lib"]);
    let run = workspace
        .simulate(&[("tb.tb_counter.all", false)], &results)
        .await;
    assert_eq!(run.report.counts().passed, 1);
    workspace.take_calls();
    workspace.take_log();

    for (edited, library) in [("ip/leaf.sv", "ip"), ("rtl/counter.sv", "rtl")] {
        let contents = fs::read_to_string(workspace.root.join(edited)).expect("read the file");
        workspace.write(edited, &format!("// edited\n{contents}"));
        let compiled = workspace.compile(&mut state).await;
        assert_eq!(compiled.report.status, CompileStatus::Succeeded);
        let name = file_name(edited);
        assert!(
            compiled.with_status(FileStatus::Compiled).contains(&name),
            "{edited}: {:?}",
            compiled.statuses()
        );
        let edit_calls = workspace.take_calls();
        assert!(
            edit_calls.contains(&format!("{library} {name}")),
            "{edited}: {edit_calls:?}"
        );
        // The VHDL testbench isn't compiled again.
        assert!(
            edit_calls.iter().all(|call| !call.starts_with("tb ")),
            "{edited}: {edit_calls:?}"
        );
        let rerun = workspace
            .simulate(&[("tb.tb_counter.all", false)], &results)
            .await;
        assert_eq!(rerun.report.counts().passed, 1);
        let log = workspace.take_log();
        let compiled_at = log
            .iter()
            .position(|line| *line == format!("end {library} {name}"));
        let started_at = log.iter().position(|line| line.starts_with("simstart"));
        assert!(
            compiled_at.is_some() && compiled_at < started_at,
            "{edited}: {log:?}"
        );
    }
}

/// `lib` with a testbench with three explicit tests and one without explicit tests.
fn simulation_project(workspace: &mut Workspace) {
    workspace.write(
        "tb/tb_tests.vhd",
        "entity tb_tests is generic (runner_cfg : string; output_path : string); end entity;\n\
         architecture tb of tb_tests is\nbegin\n  main : process\n  begin\n    \
         test_runner_setup(runner, runner_cfg);\n    \
         if run(\"pass\") then\n    elsif run(\"fail, really\") then\n    \
         elsif run(\"slow\") then\n    end if;\n    test_runner_cleanup(runner);\n  \
         end process;\nend architecture;\n",
    );
    workspace.write("tb/tb_implicit.vhd", &testbench("tb_implicit", ""));
    workspace.spec.add_library("lib", ["tb/*.vhd"]);
}

#[expect(clippy::too_many_lines, reason = "one scenario with many checks")]
async fn simulates_and_records_results() {
    let mut workspace = Workspace::new().await;
    simulation_project(&mut workspace);
    workspace.spec.sim_options.seed = Some("0123456789abcdef".to_owned());
    workspace.sim_directive("lib.tb_tests.fail, really", "print it broke");
    workspace.sim_directive("lib.tb_tests.fail, really", "fail");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(
            &[
                ("lib.tb_tests.*", false),
                ("*.ALL", true),
                ("missing", false),
            ],
            &results,
        )
        .await;
    assert_eq!(
        run.outcomes(),
        [
            ("lib.tb_implicit.all", TestOutcome::Passed),
            ("lib.tb_tests.fail, really", TestOutcome::Failed),
            ("lib.tb_tests.pass", TestOutcome::Passed),
            ("lib.tb_tests.slow", TestOutcome::Passed),
        ]
    );
    assert_eq!(run.report.diagnostics.len(), 1);
    assert_eq!(
        run.report.diagnostics[0].message,
        "no testcase matches 'missing'"
    );

    // Every test starts before it finishes; the whole run is bracketed.
    let names = run.event_names();
    assert_eq!(names.first().map(|(kind, _)| *kind), Some("started"));
    assert_eq!(names.last().map(|(kind, _)| *kind), Some("finished"));
    for (name, _) in run.outcomes() {
        let position = |kind| {
            names
                .iter()
                .position(|(event_kind, event_name)| *event_kind == kind && event_name == name)
                .expect("event")
        };
        assert!(
            position("test_started") < position("test_finished"),
            "{name}"
        );
    }
    assert!(run.events.iter().any(|event| matches!(
        event,
        SimulationEvent::Finished {
            counts: TestCounts {
                passed: 3,
                failed: 1,
                cancelled: 0
            }
        }
    )));
    assert!(matches!(
        &run.events[0],
        SimulationEvent::Started { testcases } if testcases.len() == 4
    ));

    // The output file starts with the seed and holds the simulator output.
    let fail_paths = TestOutputPaths::new(&workspace.layout, "lib.tb_tests.fail, really");
    let output = fs::read_to_string(&fail_paths.output_file).expect("output.txt");
    assert!(
        output.starts_with("Seed for lib.tb_tests.fail, really: 0123456789abcdef\n"),
        "{output}"
    );
    assert!(output.contains("it broke"), "{output}");
    assert_eq!(
        fs::read_to_string(&fail_paths.results_file).expect("vunit_results"),
        "test_start:fail, really\n"
    );

    // The run options: `runner_cfg`, the filled `output_path` generic, and GUI mode.
    let options = |testcase: &str| run_options(&workspace.layout, testcase);
    let generics = |run_options: &Value| run_options["generics"].to_string();
    let pass_options = options("lib.tb_tests.pass");
    let pass_generics = generics(&pass_options);
    let pass_dir = TestOutputPaths::new(&workspace.layout, "lib.tb_tests.pass").dir;
    assert!(
        pass_generics.contains(&format!("\"output_path={pass_dir}/\"")),
        "{pass_generics}"
    );
    assert!(
        pass_generics.contains("enabled_test_cases : pass,"),
        "{pass_generics}"
    );
    assert!(
        pass_generics.contains("seed : 0123456789abcdef"),
        "{pass_generics}"
    );
    assert_eq!(pass_options["name"], "lib.tb_tests.pass");
    assert_eq!(pass_options["paused"], false);
    assert!(
        generics(&options("lib.tb_tests.fail, really"))
            .contains("enabled_test_cases : fail,,,, really,")
    );
    let implicit_options = options("lib.tb_implicit.all");
    assert_eq!(implicit_options["paused"], true);
    assert!(
        generics(&implicit_options).contains("enabled_test_cases : ,"),
        "{implicit_options}"
    );

    // The results are persisted.
    let saved = TestResults::load(&workspace.layout).results;
    assert_eq!(saved.len(), 4);
    let failed = &saved["lib.tb_tests.fail, really"];
    assert_eq!(failed.outcome, TestOutcome::Failed);
    assert_eq!(failed.output_path, fail_paths.output_file);
    assert!(failed.started_at <= failed.finished_at);
    // A failed run completed, so it measured the duration.
    assert!(failed.duration_ms.is_some());

    // A rerun replaces the output directory.
    let stale = fail_paths.dir.join("stale.txt");
    fs::write(&stale, "").expect("write");
    let rerun = workspace.simulate(&[("*fail*", false)], &results).await;
    assert_eq!(
        rerun.outcomes(),
        [("lib.tb_tests.fail, really", TestOutcome::Failed)]
    );
    assert!(!stale.exists());
}

async fn non_zero_exit_code_fails_a_passed_test() {
    let mut workspace = Workspace::new().await;
    simulation_project(&mut workspace);
    workspace.sim_directive("lib.tb_tests.pass", "exit 3");
    workspace.sim_directive("lib.tb_implicit.all", "no-results");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(&[("*.pass", false), ("*.all", false)], &results)
        .await;
    assert_eq!(
        run.outcomes(),
        [
            ("lib.tb_implicit.all", TestOutcome::Failed),
            ("lib.tb_tests.pass", TestOutcome::Failed),
        ]
    );
    assert_eq!(run.report.diagnostics, []);
}

async fn semaphore_limits_simulations() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(2));
    simulation_project(&mut workspace);
    for test in ["pass", "fail, really", "slow"] {
        workspace.sim_directive(&format!("lib.tb_tests.{test}"), "sleep 300");
    }
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(&[("lib.tb_tests.*", false)], &results)
        .await;
    assert_eq!(run.report.counts().passed, 3);
    let mut running: i32 = 0;
    let mut max_running = 0;
    for line in workspace.take_log() {
        if line.starts_with("simstart") {
            running += 1;
        } else if line.starts_with("simend") {
            running -= 1;
        }
        max_running = max_running.max(running);
    }
    assert_eq!(max_running, 2);
}

/// Grants leases with the run option `{"lease": <threads>}`, or fails them with `fail`,
/// records the threads of every test, and reports them as running.
///
/// With `hold`, the leases hold their simulators at the end, which the fake simulator reports
/// in its log (its directive `hold`); the provider adopts them.
/// It records the calls of `lease` and `release_held`, and the tests whose lease guards were
/// dropped.
#[derive(Debug, Default)]
struct RecordingLeases {
    granted: Mutex<Vec<(String, u32)>>,
    fail: Option<String>,
    hold: bool,
    /// `lease <test>` and `release <test> <whether the output directory has options.json>`.
    calls: Mutex<Vec<String>>,
    adopted: Mutex<Vec<HeldRunner>>,
    dropped: Arc<Mutex<Vec<String>>>,
}

impl RecordingLeases {
    /// The threads of the granted leases, in the order they were asked for.
    fn threads(&self) -> Vec<u32> {
        let granted = self.granted.lock().expect("lock");
        granted.iter().map(|&(_, threads)| threads).collect()
    }

    /// The calls of `lease` and `release_held`, in order.
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }

    /// Takes the adopted simulators, sorted by test.
    fn take_adopted(&self) -> Vec<HeldRunner> {
        let mut adopted = std::mem::take(&mut *self.adopted.lock().expect("lock"));
        adopted.sort_by(|left, right| left.test.cmp(&right.test));
        adopted
    }

    /// The tests whose lease guards were dropped.
    fn dropped(&self) -> Vec<String> {
        self.dropped.lock().expect("lock").clone()
    }
}

impl LeaseProvider for RecordingLeases {
    fn lease<'a>(
        &'a self,
        test: &'a str,
        threads: u32,
        events: &'a LeaseEvents,
    ) -> LeaseFuture<'a> {
        self.granted
            .lock()
            .expect("lock")
            .push((test.to_owned(), threads));
        self.calls
            .lock()
            .expect("lock")
            .push(format!("lease {test}"));
        events.report(ThreadState::Running { threads });
        let lease = if let Some(message) = &self.fail {
            Err(message.clone())
        } else {
            let lease = Lease {
                options: RunOptions::from_iter([("lease".to_owned(), threads.into())]),
                guard: Box::new(LeaseGuard {
                    test: test.to_owned(),
                    dropped: Arc::clone(&self.dropped),
                }),
            };
            if self.hold {
                let log = events
                    .workspace_root()
                    .expect("the workspace root")
                    .join(fake_risim::LOG);
                let (sender, ended) = oneshot::channel();
                tokio::spawn(report_held_end(log, test.to_owned(), sender));
                Ok(lease.with_ended(ended))
            } else {
                Ok(lease)
            }
        };
        Box::pin(std::future::ready(lease))
    }

    fn adopt(&self, held: HeldRunner) {
        self.adopted.lock().expect("lock").push(held);
    }

    fn release_held<'a>(
        &'a self,
        workspace_root: &'a Utf8Path,
        test: &'a str,
    ) -> ReleaseFuture<'a> {
        let paths = TestOutputPaths::new(&OutputLayout::new(workspace_root), test);
        let previous = paths.dir.join("options.json").exists();
        self.calls
            .lock()
            .expect("lock")
            .push(format!("release {test} {previous}"));
        Box::pin(std::future::ready(()))
    }
}

/// The guard of a lease of [`RecordingLeases`], which records its test when dropped.
#[derive(Debug)]
struct LeaseGuard {
    test: String,
    dropped: Arc<Mutex<Vec<String>>>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.dropped.lock().expect("lock").push(self.test.clone());
    }
}

/// Sends the exit code of `test` once the fake simulator logs to `log` that it holds it.
async fn report_held_end(log: Utf8PathBuf, test: String, sender: oneshot::Sender<i32>) {
    let prefix = format!("held {test} ");
    while !sender.is_closed() {
        let contents = fs::read_to_string(&log).unwrap_or_default();
        if let Some(code) = contents.lines().find_map(|line| line.strip_prefix(&prefix)) {
            sender.send(code.parse().expect("the exit code")).ok();
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A [`RecordingLeases`] without its own `adopt`, which leaves adoption to the default.
#[derive(Debug)]
struct WithoutAdoption(Arc<RecordingLeases>);

impl LeaseProvider for WithoutAdoption {
    fn lease<'a>(
        &'a self,
        test: &'a str,
        threads: u32,
        events: &'a LeaseEvents,
    ) -> LeaseFuture<'a> {
        self.0.lease(test, threads, events)
    }
}

/// A [`RecordingLeases`] whose held simulators never end, so that `release_held` waits forever.
#[derive(Debug)]
struct NeverReleased(Arc<RecordingLeases>);

impl LeaseProvider for NeverReleased {
    fn lease<'a>(
        &'a self,
        test: &'a str,
        threads: u32,
        events: &'a LeaseEvents,
    ) -> LeaseFuture<'a> {
        self.0.lease(test, threads, events)
    }

    fn release_held<'a>(
        &'a self,
        _workspace_root: &'a Utf8Path,
        _test: &'a str,
    ) -> ReleaseFuture<'a> {
        Box::pin(std::future::pending())
    }
}

/// Awaits `future`, failing if it takes longer than 30 s.
async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("in time")
}

/// Waits until `condition` holds, failing after 30 s.
async fn eventually(mut condition: impl FnMut() -> bool) {
    within(async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// `lib`: a testbench `tb_many` with the tests `t0`, `t1`, …, `count` in all.
fn many_tests_project(workspace: &mut Workspace, count: usize) {
    let runs: Vec<String> = (0..count)
        .map(|index| format!("run(\"t{index}\")"))
        .collect();
    workspace.write(
        "tb/tb_many.vhd",
        &format!(
            "entity tb_many is generic (runner_cfg : string); end entity;\n\
             architecture tb of tb_many is\nbegin\n  main : process\n  begin\n    \
             test_runner_setup(runner, runner_cfg);\n    if {} then\n    end if;\n    \
             test_runner_cleanup(runner);\n  end process;\nend architecture;\n",
            runs.join(" then\n    elsif ")
        ),
    );
    workspace.spec.add_library("lib", ["tb/*.vhd"]);
}

/// The tests of an operation share the free threads, if they can use them: 5 tests on 10
/// threads get 2 each, which their leases get, and the simulator their run options.
async fn tests_share_the_threads() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(10));
    workspace.max_threads_per_test = threads(8);
    let leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    many_tests_project(&mut workspace, 5);
    for index in 0..5 {
        workspace.sim_directive(&format!("lib.tb_many.t{index}"), "sleep 300");
    }
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(&[("lib.tb_many.*", false)], &results)
        .await;
    assert_eq!(run.report.counts().passed, 5);
    assert_eq!(leases.threads(), [2; 5]);
    // The provider's reports reach the listeners.
    let states = run.thread_states();
    assert_eq!(states.len(), 5, "{states:?}");
    assert!(
        states
            .iter()
            .all(|(_, state)| **state == ThreadState::Running { threads: 2 }),
        "{states:?}"
    );
    let options = run_options(&workspace.layout, "lib.tb_many.t0");
    assert_eq!(options["lease"], 2, "{options}");
}

/// 100 tests on 10 threads get one each while more tests wait than there are threads; at most
/// 10 run at once.
async fn many_tests_get_one_thread_each() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(10));
    workspace.max_threads_per_test = threads(8);
    let leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    many_tests_project(&mut workspace, 100);
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(&[("lib.tb_many.*", false)], &results)
        .await;
    assert_eq!(run.report.counts().passed, 100);
    let granted = leases.threads();
    assert_eq!(granted.len(), 100);
    // Up to the 90th test, at least 11 tests wait, which share at most 10 threads.
    assert_eq!(granted[..90], [1; 90]);
    let mut running: i32 = 0;
    let mut max_running = 0;
    for line in workspace.take_log() {
        if line.starts_with("simstart") {
            running += 1;
        } else if line.starts_with("simend") {
            running -= 1;
        }
        max_running = max_running.max(running);
    }
    assert!(max_running <= 10, "{max_running}");
}

/// A test takes no more threads than it can use, even when it is alone with a free budget.
async fn a_test_takes_at_most_its_threads() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(10));
    workspace.max_threads_per_test = threads(3);
    let leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    many_tests_project(&mut workspace, 2);
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(&[("lib.tb_many.*", false)], &results)
        .await;
    assert_eq!(run.report.counts().passed, 2);
    assert_eq!(leases.threads(), [3, 3]);

    // One thread each, as the hub's tests take until the multi-threaded kernel.
    workspace.max_threads_per_test = NonZeroU32::MIN;
    let single_leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::clone(&single_leases) as Arc<dyn LeaseProvider>;
    let single = workspace
        .simulate(&[("lib.tb_many.t0", false)], &results)
        .await;
    assert_eq!(single.report.counts().passed, 1);
    assert_eq!(single_leases.threads(), [1]);
}

/// A test that finds no free thread of the budget reports the wait, with the tests queued behind
/// it, once each; a GUI test takes one thread.
async fn a_full_budget_reports_waits() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(1));
    simulation_project(&mut workspace);
    for test in ["fail, really", "pass"] {
        workspace.sim_directive(&format!("lib.tb_tests.{test}"), "sleep 300");
    }
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace
        .simulate(&[("lib.tb_tests.*", false)], &results)
        .await;
    assert_eq!(run.report.counts().passed, 3);
    let budget = ThreadState::Waiting(WaitReason::Budget { threads: 1 });
    assert_eq!(
        run.thread_states(),
        [
            ("lib.tb_tests.pass", &budget),
            ("lib.tb_tests.slow", &budget)
        ]
    );
    // The wait comes before the start.
    let position = |wanted: fn(&SimulationEvent) -> bool| run.events.iter().position(wanted);
    let waited = position(|event| matches!(event, SimulationEvent::TestThreads { .. }));
    let started = position(
        |event| matches!(event, SimulationEvent::TestStarted { name, .. } if name.ends_with("slow")),
    );
    assert!(waited.is_some() && waited < started, "{:#?}", run.events);

    workspace.threads = ThreadBudget::new(threads(4));
    workspace.max_threads_per_test = threads(4);
    let leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    let gui = workspace
        .simulate(&[("lib.tb_tests.pass", true)], &results)
        .await;
    assert_eq!(gui.report.counts().passed, 1);
    assert_eq!(leases.threads(), [1]);
}

/// Growing the budget starts waiting tests at once; shrinking it lets running tests finish and
/// starts no test until the threads in use fit.
async fn the_budget_resizes() {
    let grown = resized_run(1, 3).await;
    let grown_starts = positions(&grown, "simstart");
    let first_end = grown.iter().position(|line| line.starts_with("simend"));
    // All three started before the first one ended.
    assert!(
        grown_starts.iter().all(|&start| Some(start) < first_end),
        "{grown:#?}"
    );

    let shrunk = resized_run(2, 1).await;
    let shrunk_starts = positions(&shrunk, "simstart");
    let ends = positions(&shrunk, "simend");
    // The third test starts only after both running ones ended.
    assert_eq!(shrunk_starts.len(), 3, "{shrunk:#?}");
    assert!(shrunk_starts[2] > ends[1], "{shrunk:#?}");
}

/// Simulates three tests of 400 ms with a budget of `initial` threads, which is resized to
/// `resized` when the first test starts; returns the log of the simulator.
async fn resized_run(initial: u32, resized: u32) -> Vec<String> {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(initial));
    many_tests_project(&mut workspace, 3);
    for index in 0..3 {
        workspace.sim_directive(&format!("lib.tb_many.t{index}"), "sleep 400");
    }
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let budget = workspace.threads.clone();
    let run = workspace
        .simulate_until(&[("lib.tb_many.*", false)], &results, move |event| {
            if matches!(event, SimulationEvent::TestStarted { .. }) {
                budget.resize(threads(resized));
            }
            false
        })
        .await;
    assert_eq!(run.report.counts().passed, 3);
    assert_eq!(workspace.threads.threads(), threads(resized));
    workspace.take_log()
}

/// The positions of the lines of `log` that start with `prefix`.
fn positions(log: &[String], prefix: &str) -> Vec<usize> {
    log.iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with(prefix))
        .map(|(index, _)| index)
        .collect()
}

/// A simulation that ends held finishes its test with the reported status and its results, and
/// hands over its simulator, the lease's guard and one thread of the budget.
async fn a_held_end_hands_over_the_simulator() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(4));
    workspace.max_threads_per_test = threads(4);
    let leases = Arc::new(RecordingLeases {
        hold: true,
        ..RecordingLeases::default()
    });
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    simulation_project(&mut workspace);
    workspace.sim_directive("lib.tb_tests.pass", "hold");
    workspace.sim_directive("lib.tb_tests.slow", "exit 3");
    workspace.sim_directive("lib.tb_tests.slow", "hold");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = within(workspace.simulate(
        &[("lib.tb_tests.pass", false), ("lib.tb_tests.slow", false)],
        &results,
    ))
    .await;
    // The results pass both; the status fails the second.
    let expected = [
        ("lib.tb_tests.pass", TestOutcome::Passed),
        ("lib.tb_tests.slow", TestOutcome::Failed),
    ];
    assert_eq!(run.outcomes(), expected);
    assert_eq!(run.report.diagnostics, []);
    let recorded = results.snapshot();
    for (name, outcome) in expected {
        assert_eq!(recorded[name].outcome, outcome, "{name}");
    }
    let log = workspace.take_log();
    assert!(
        log.iter().any(|line| line == "held lib.tb_tests.pass 0")
            && log.iter().any(|line| line == "held lib.tb_tests.slow 3")
            && !log.iter().any(|line| line.starts_with("simend")),
        "{log:#?}"
    );

    // Each test had more than one thread; its simulator keeps one.
    assert!(
        leases.threads().iter().all(|&threads| threads > 1),
        "{:?}",
        leases.threads()
    );
    let mut adopted = leases.take_adopted();
    let tests: Vec<&str> = adopted.iter().map(|held| held.test.as_str()).collect();
    assert_eq!(tests, ["lib.tb_tests.pass", "lib.tb_tests.slow"]);
    assert_eq!(workspace.threads.available(), 2);
    assert_eq!(leases.dropped(), Vec::<String>::new());
    for held in &mut adopted {
        assert_eq!(held.threads.threads(), 1);
        assert_eq!(
            held.workspace_root.as_deref(),
            Some(workspace.root.as_path())
        );
        let guard = held
            .guard
            .downcast_ref::<LeaseGuard>()
            .expect("the lease's guard");
        assert_eq!(guard.test, held.test);
        // The simulator lives on.
        tokio::time::timeout(Duration::from_millis(100), held.process.wait())
            .await
            .expect_err("the simulator lives on");
    }

    for mut held in adopted {
        held.process.terminate().await.expect("terminate");
    }
    assert_eq!(workspace.threads.available(), 4);
    assert_eq!(leases.dropped().len(), 2);
}

/// A held simulator doesn't keep the process permit of its operation, so that the operation's
/// other tests run.
async fn a_held_simulator_frees_the_process_permit() {
    let mut workspace = Workspace::new().await;
    workspace.max_parallel = Some(1);
    let leases = Arc::new(RecordingLeases {
        hold: true,
        ..RecordingLeases::default()
    });
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    simulation_project(&mut workspace);
    workspace.sim_directive("lib.tb_tests.pass", "hold");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = within(workspace.simulate(
        &[("lib.tb_tests.pass", false), ("lib.tb_tests.slow", false)],
        &results,
    ))
    .await;
    assert_eq!(
        run.outcomes(),
        [
            ("lib.tb_tests.pass", TestOutcome::Passed),
            ("lib.tb_tests.slow", TestOutcome::Passed),
        ]
    );
    let adopted = leases.take_adopted();
    let tests: Vec<&str> = adopted.iter().map(|held| held.test.as_str()).collect();
    assert_eq!(tests, ["lib.tb_tests.pass"]);
    for mut held in adopted {
        held.process.terminate().await.expect("terminate");
    }
}

/// A test has its testcase's held simulator ended before it prepares its output directory, and
/// before it asks for its lease.
async fn release_held_comes_before_prepare() {
    let mut workspace = Workspace::new().await;
    let leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::clone(&leases) as Arc<dyn LeaseProvider>;
    simulation_project(&mut workspace);
    let results = Arc::new(ResultStore::load(&workspace.layout));
    for _ in 0..2 {
        let run = workspace
            .simulate(&[("lib.tb_tests.pass", false)], &results)
            .await;
        assert_eq!(run.outcomes(), [("lib.tb_tests.pass", TestOutcome::Passed)]);
    }
    // At the second release, the output of the first run is still there.
    assert_eq!(
        leases.calls(),
        [
            "release lib.tb_tests.pass false",
            "lease lib.tb_tests.pass",
            "release lib.tb_tests.pass true",
            "lease lib.tb_tests.pass",
        ]
    );
}

/// A cancel interrupts a test that waits for the end of its testcase's held simulator, which
/// then doesn't start.
async fn a_cancel_interrupts_release_held() {
    let mut workspace = Workspace::new().await;
    let leases = Arc::new(RecordingLeases::default());
    workspace.leases = Arc::new(NeverReleased(Arc::clone(&leases)));
    simulation_project(&mut workspace);
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = within(workspace.simulate_until(
        &[("lib.tb_tests.pass", false)],
        &results,
        |event| matches!(event, SimulationEvent::Started { .. }),
    ))
    .await;
    assert_eq!(
        run.outcomes(),
        [("lib.tb_tests.pass", TestOutcome::Cancelled)]
    );
    assert_eq!(leases.calls(), Vec::<String>::new());
    assert!(workspace.take_log().is_empty());
}

/// A provider that doesn't adopt held simulators has them terminated, and then drops their
/// lease guards and threads.
async fn a_provider_without_adoption_terminates_the_simulator() {
    let mut workspace = Workspace::new().await;
    let leases = Arc::new(RecordingLeases {
        hold: true,
        ..RecordingLeases::default()
    });
    workspace.leases = Arc::new(WithoutAdoption(Arc::clone(&leases)));
    simulation_project(&mut workspace);
    workspace.sim_directive("lib.tb_tests.pass", "hold");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = within(workspace.simulate(&[("lib.tb_tests.pass", false)], &results)).await;
    assert_eq!(run.outcomes(), [("lib.tb_tests.pass", TestOutcome::Passed)]);
    eventually(|| leases.dropped() == ["lib.tb_tests.pass"]).await;
    assert_eq!(workspace.threads.available(), 4);
    let log = workspace.take_log();
    assert!(
        log.iter().any(|line| line == "held lib.tb_tests.pass 0"),
        "{log:#?}"
    );
    assert!(
        !log.iter().any(|line| line.starts_with("simend")),
        "{log:#?}"
    );
}

/// A lease that isn't granted fails the test with the provider's message.
async fn a_refused_lease_fails_the_test() {
    let mut workspace = Workspace::new().await;
    workspace.leases = Arc::new(RecordingLeases {
        fail: Some("no threads for you".to_owned()),
        ..RecordingLeases::default()
    });
    simulation_project(&mut workspace);
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace.simulate(&[("*.pass", false)], &results).await;
    assert_eq!(run.outcomes(), [("lib.tb_tests.pass", TestOutcome::Failed)]);
    let message = &run.report.diagnostics[0].message;
    assert!(
        message.ends_with("failed to lease the simulation threads: no threads for you"),
        "{message}"
    );
    assert!(workspace.take_log().is_empty());
}

async fn cancel_terminates_and_skips_simulations() {
    let mut workspace = Workspace::new().await;
    workspace.threads = ThreadBudget::new(threads(1));
    simulation_project(&mut workspace);
    workspace.sim_directive("lib.tb_tests.fail, really", "hang");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let first = workspace
        .simulate(&[("lib.tb_tests.slow", false)], &results)
        .await;
    assert_eq!(
        first.outcomes(),
        [("lib.tb_tests.slow", TestOutcome::Passed)]
    );
    let previous = results.snapshot()["lib.tb_tests.slow"].clone();
    workspace.take_log();

    // Times the cancellation alone, not the loading and planning before it, which can be slow
    // when many tests run at once.
    let cancelled_at = Arc::new(OnceLock::new());
    let run = workspace
        .simulate_until(
            &[("lib.tb_tests.fail*", false), ("lib.tb_tests.slow", false)],
            &results,
            {
                let cancelled_at = Arc::clone(&cancelled_at);
                move |event| {
                    let cancel = matches!(event, SimulationEvent::TestStarted { name, .. } if name.contains("fail"));
                    if cancel {
                        cancelled_at.get_or_init(Instant::now);
                    }
                    cancel
                }
            },
        )
        .await;
    let cancelled_at = cancelled_at.get().expect("the hanging test started");
    assert!(cancelled_at.elapsed() < Duration::from_secs(10));
    assert_eq!(
        run.outcomes(),
        [
            ("lib.tb_tests.fail, really", TestOutcome::Cancelled),
            ("lib.tb_tests.slow", TestOutcome::Cancelled),
        ]
    );
    assert!(run.events.iter().any(|event| matches!(
        event,
        SimulationEvent::Finished {
            counts: TestCounts {
                passed: 0,
                failed: 0,
                cancelled: 2
            }
        }
    )));
    // The test that never started has no `TestStarted` and keeps its previous result.
    assert!(
        !run.event_names()
            .contains(&("test_started", "lib.tb_tests.slow".to_owned()))
    );
    assert!(
        run.event_names()
            .contains(&("test_finished", "lib.tb_tests.slow".to_owned()))
    );
    let saved = TestResults::load(&workspace.layout).results;
    assert_eq!(saved["lib.tb_tests.slow"], previous);
    assert_eq!(
        saved["lib.tb_tests.fail, really"].outcome,
        TestOutcome::Cancelled
    );
    // A cancelled run doesn't measure the duration.
    assert_eq!(saved["lib.tb_tests.fail, really"].duration_ms, None);
    // The hanging simulation may be killed before it logs its start, but never ends by itself.
    let log = workspace.take_log();
    assert!(
        !log.iter().any(|line| line.starts_with("simend")),
        "{log:?}"
    );
}

async fn simulation_spawn_failure_fails_the_test() {
    let mut workspace = Workspace::new().await;
    simulation_project(&mut workspace);
    let mut identity = workspace.simulator.identity().clone();
    let missing = workspace.root.join("missing-risim");
    identity.path.clone_from(&missing);
    workspace.simulator = Simulator::from_identity(identity).expect("valid identity");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace.simulate(&[("*.pass", false)], &results).await;
    assert_eq!(run.outcomes(), [("lib.tb_tests.pass", TestOutcome::Failed)]);
    assert_eq!(run.report.diagnostics.len(), 1);
    let message = &run.report.diagnostics[0].message;
    let failed_to_run = format!("failed to run {missing}");
    assert!(
        message.starts_with(&format!("lib.tb_tests.pass: {failed_to_run}")),
        "{message}"
    );
    let paths = TestOutputPaths::new(&workspace.layout, "lib.tb_tests.pass");
    let output = fs::read_to_string(&paths.output_file).expect("output.txt");
    assert!(output.contains(&failed_to_run), "{output}");
    // A simulator that didn't run doesn't measure the duration.
    assert_eq!(results.snapshot()["lib.tb_tests.pass"].duration_ms, None);
    assert!(
        run.events
            .iter()
            .any(|event| matches!(event, SimulationEvent::Diagnostic { .. }))
    );
}

/// Run options of the configuration that set a key of the simulation fail the test with the
/// error of its command line, and nothing runs.
async fn run_options_that_set_a_simulation_key_fail_the_test() {
    let mut workspace = Workspace::new().await;
    simulation_project(&mut workspace);
    workspace.spec.sim_options.run_options =
        Some(RunOptions::from_iter([("paused".to_owned(), true.into())]));
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace.simulate(&[("*.pass", false)], &results).await;
    assert_eq!(run.outcomes(), [("lib.tb_tests.pass", TestOutcome::Failed)]);
    let message = &run.report.diagnostics[0].message;
    let expected = "the run options can't set `paused`, which every simulation sets itself";
    assert!(message.ends_with(expected), "{message}");
    let paths = TestOutputPaths::new(&workspace.layout, "lib.tb_tests.pass");
    let output = fs::read_to_string(&paths.output_file).expect("output.txt");
    assert!(output.contains(expected), "{output}");
    assert!(workspace.take_log().is_empty());
}

/// Compiles the UART example of VUnit (`tests/operations/uart`) with the real analyzer and
/// risim-runner, then recompiles after edits.
#[expect(clippy::print_stderr, reason = "reports timings")]
async fn real_risim_compiles_uart() {
    let fixtures = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/operations");
    let mut workspace = Workspace::new().await;
    let runner = Utf8PathBuf::from(env::var("RISIM_RUNNER").expect("RISIM_RUNNER"));
    let analyzer = Utf8PathBuf::from(env::var("RISIM_ANALYZER").expect("RISIM_ANALYZER"));
    workspace.simulator = Simulator::detect(&runner, &analyzer)
        .await
        .expect("detect risim-runner");
    let parallelism = thread::available_parallelism().map_or(4, NonZeroUsize::get);
    workspace.semaphore = Arc::new(Semaphore::new(parallelism));

    // The example is copied so that it can be edited.
    for entry in walkdir::WalkDir::new(fixtures.join("uart")) {
        let entry = entry.expect("walk");
        let path = Utf8Path::from_path(entry.path()).expect("UTF-8 path");
        if !entry.file_type().is_dir() {
            let relative = path.strip_prefix(&fixtures).expect("below the fixtures");
            workspace.write(
                relative.as_str(),
                &fs::read_to_string(path).expect("read example"),
            );
        }
    }
    workspace.spec.options.relaxed = true;
    workspace
        .spec
        .add_feature(Feature::VerificationComponents)
        .add_library("uart_lib", ["uart/*.vhd"])
        .add_library("tb_uart_lib", ["uart/test/*.vhd"]);

    let mut state = CompileState::load(&workspace.layout);
    let started = Instant::now();
    let full = workspace.compile(&mut state).await;
    eprintln!(
        "full compile: {} files in {:?}",
        full.with_status(FileStatus::Compiled).len(),
        started.elapsed()
    );
    let errors: Vec<String> = full
        .report
        .diagnostics
        .values()
        .flatten()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(ToString::to_string)
        .collect();
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(full.report.status, CompileStatus::Succeeded);
    assert!(full.with_status(FileStatus::Compiled).len() > 100);

    let unchanged = workspace.compile(&mut state).await;
    assert_eq!(
        unchanged.with_status(FileStatus::Compiled),
        Vec::<&str>::new()
    );

    let uart_tx = workspace.root.join("uart/uart_tx.vhd");
    let original = fs::read_to_string(&uart_tx).expect("read uart_tx.vhd");
    fs::write(&uart_tx, format!("{original}\n-- edited\n")).expect("edit");
    let edit_started = Instant::now();
    let edited = workspace.compile(&mut state).await;
    eprintln!(
        "incremental compile: {:?} in {:?}",
        edited.with_status(FileStatus::Compiled),
        edit_started.elapsed()
    );
    assert_eq!(edited.report.status, CompileStatus::Succeeded);
    assert_eq!(
        edited.with_status(FileStatus::Compiled),
        ["uart_tx.vhd", "tb_uart_tx.vhd"]
    );

    fs::write(&uart_tx, format!("{original}\nthis is not VHDL;\n")).expect("break");
    let broken = workspace.compile(&mut state).await;
    assert_eq!(broken.report.status, CompileStatus::Failed);
    assert_eq!(broken.with_status(FileStatus::Failed), ["uart_tx.vhd"]);
    assert_eq!(broken.with_status(FileStatus::Skipped), ["tb_uart_tx.vhd"]);
    let live = broken.events.iter().find_map(|(_, event)| match event {
        CompileEvent::Diagnostic { diagnostic, .. } => Some(diagnostic),
        _ => None,
    });
    let live = live.expect("a live diagnostic");
    eprintln!("live diagnostic: {live}");
    assert_eq!(live.severity, Severity::Error);
    assert_eq!(live.file.as_ref(), Some(&uart_tx));
    assert!(live.range.is_some());

    fs::write(&uart_tx, &original).expect("restore");
    let restored = workspace.compile(&mut state).await;
    assert_eq!(restored.report.status, CompileStatus::Succeeded);
    assert_eq!(
        restored.with_status(FileStatus::Compiled),
        ["uart_tx.vhd", "tb_uart_tx.vhd"]
    );
}
