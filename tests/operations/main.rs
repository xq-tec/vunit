// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests of compile and simulation operations.
//!
//! The binary doubles as a fake risim-ghdl (see `fake_ghdl.rs`), so the tests run everywhere
//! without a simulator. The trial `real_risim_ghdl_compiles_uart` uses the risim-ghdl named
//! by `RISIM_GHDL` and is ignored without it; the end-to-end tests with risim-ghdl are in the
//! `acceptance` target.
//!
//! AI NOTICE: Generated, minimally reviewed.

mod fake_ghdl;
#[path = "../common/trial.rs"]
mod trial;
mod workspace;

use std::env;
use std::fs;
use std::num::NonZeroUsize;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use libtest_mimic::Arguments;
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
use risim_vunit_frontend::simulator::Simulator;
use risim_vunit_frontend::sources;
use risim_vunit_frontend::sources::SourceCache;
use risim_vunit_frontend::spec::Feature;
use risim_vunit_frontend::spec::ProjectSpec;
use risim_vunit_frontend::store::CompileState;
use risim_vunit_frontend::store::OutputLayout;
use risim_vunit_frontend::store::ResultStore;
use risim_vunit_frontend::store::TestCounts;
use risim_vunit_frontend::store::TestOutcome;
use risim_vunit_frontend::store::TestOutputPaths;
use risim_vunit_frontend::store::TestResults;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use self::trial::trial;

fn main() -> ExitCode {
    if env::var_os(fake_ghdl::ENV).is_some() {
        return fake_ghdl::main();
    }
    // SAFETY: No other threads exist yet. Child processes inherit the variable and act as the
    // fake simulator.
    unsafe {
        env::set_var(fake_ghdl::ENV, "1");
    }

    let args = Arguments::from_args();
    let trials = vec![
        trial(
            "compiles_in_order_and_incrementally",
            compiles_in_order_and_incrementally,
        ),
        trial("failure_skips_dependents", failure_skips_dependents),
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
        trial(
            "cancel_terminates_and_skips_simulations",
            cancel_terminates_and_skips_simulations,
        ),
        trial(
            "simulation_spawn_failure_fails_the_test",
            simulation_spawn_failure_fails_the_test,
        ),
        trial(
            "real_risim_ghdl_compiles_uart",
            real_risim_ghdl_compiles_uart,
        )
        .with_ignored_flag(env::var_os("RISIM_GHDL").is_none()),
    ];
    let trials = trials.into_iter().chain(workspace::trials()).collect();
    libtest_mimic::run(&args, trials).exit_code()
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
    semaphore: Arc<Semaphore>,
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

    /// The events without diagnostics, as `(kind, testcase)`.
    fn event_names(&self) -> Vec<(&'static str, String)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                SimulationEvent::Started { .. } => Some(("started", String::new())),
                SimulationEvent::TestStarted { name, .. } => Some(("test_started", name.clone())),
                SimulationEvent::TestFinished { name, .. } => Some(("test_finished", name.clone())),
                SimulationEvent::Finished { .. } => Some(("finished", String::new())),
                SimulationEvent::Diagnostic { .. } => None,
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
        let simulator = Simulator::detect(&exe).await.expect("detect the fake");
        Self {
            _temp: temp,
            root: root.clone(),
            layout: OutputLayout::new(&root),
            spec: ProjectSpec::new(),
            simulator,
            semaphore: Arc::new(Semaphore::new(4)),
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
            semaphore: Arc::clone(&self.semaphore),
            events,
            cancel,
        };
        let input = PlanInput {
            layout: &self.layout,
            project: &loaded.project,
            targets: &targets,
            compile_options: &self.spec.compile_options,
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
            semaphore: Arc::clone(&self.semaphore),
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
        let path = self.root.join(fake_ghdl::SIM_DIRECTIVES);
        let mut directives = fs::read_to_string(&path).unwrap_or_default();
        for part in [testcase, "\t", directive, "\n"] {
            directives.push_str(part);
        }
        fs::write(&path, directives).expect("write the directives");
    }

    /// Returns and clears the log of the fake simulator.
    fn take_log(&self) -> Vec<String> {
        let path = self.root.join(fake_ghdl::LOG);
        let log = fs::read_to_string(&path).unwrap_or_default();
        let _ignored = fs::remove_file(&path);
        log.lines().map(str::to_owned).collect()
    }
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
    let started = Instant::now();
    let run = workspace
        .compile_until(&mut state, |event| {
            matches!(event, CompileEvent::FileStarted { file, .. } if file.ends_with("pkg.vhd"))
        })
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
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
    identity.path = workspace.root.join("missing-risim-ghdl");
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
    assert!(message.starts_with("failed to run risim-ghdl"), "{message}");
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
    let missing = Simulator::detect(Utf8Path::new("/nonexistent/risim-ghdl")).await;
    missing.expect_err("the executable doesn't exist");
    #[cfg(unix)]
    {
        let error = Simulator::detect(Utf8Path::new("/bin/sh"))
            .await
            .expect_err("sh isn't risim-ghdl");
        assert!(error.to_string().contains("risim-ghdl version"), "{error}");
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

    // The command line: `runner_cfg`, the filled `output_path` generic, and GUI mode.
    let args = |testcase: &str| {
        let paths = TestOutputPaths::new(&workspace.layout, testcase);
        fs::read_to_string(paths.dir.join("args.txt")).expect("args.txt")
    };
    let pass_args = args("lib.tb_tests.pass");
    let pass_dir = TestOutputPaths::new(&workspace.layout, "lib.tb_tests.pass").dir;
    assert!(
        pass_args.contains(&format!("-goutput_path={pass_dir}/")),
        "{pass_args}"
    );
    assert!(
        pass_args.contains("enabled_test_cases : pass,"),
        "{pass_args}"
    );
    assert!(pass_args.contains("seed : 0123456789abcdef"), "{pass_args}");
    assert!(
        pass_args.contains("--name=lib.tb_tests.pass"),
        "{pass_args}"
    );
    assert!(!pass_args.contains("--wait"), "{pass_args}");
    assert!(args("lib.tb_tests.fail, really").contains("enabled_test_cases : fail,,,, really,"));
    let implicit_args = args("lib.tb_implicit.all");
    assert!(implicit_args.contains("--wait"), "{implicit_args}");
    assert!(
        implicit_args.contains("enabled_test_cases : ,"),
        "{implicit_args}"
    );

    // The results are persisted.
    let saved = TestResults::load(&workspace.layout).results;
    assert_eq!(saved.len(), 4);
    let failed = &saved["lib.tb_tests.fail, really"];
    assert_eq!(failed.outcome, TestOutcome::Failed);
    assert_eq!(failed.output_path, fail_paths.output_file);
    assert!(failed.started_at <= failed.finished_at);

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
    workspace.semaphore = Arc::new(Semaphore::new(2));
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

async fn cancel_terminates_and_skips_simulations() {
    let mut workspace = Workspace::new().await;
    workspace.semaphore = Arc::new(Semaphore::new(1));
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

    let started = Instant::now();
    let run = workspace
        .simulate_until(
            &[("lib.tb_tests.fail*", false), ("lib.tb_tests.slow", false)],
            &results,
            |event| matches!(event, SimulationEvent::TestStarted { name, .. } if name.contains("fail")),
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
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
    identity.path = workspace.root.join("missing-risim-ghdl");
    workspace.simulator = Simulator::from_identity(identity).expect("valid identity");
    let results = Arc::new(ResultStore::load(&workspace.layout));
    let run = workspace.simulate(&[("*.pass", false)], &results).await;
    assert_eq!(run.outcomes(), [("lib.tb_tests.pass", TestOutcome::Failed)]);
    assert_eq!(run.report.diagnostics.len(), 1);
    let message = &run.report.diagnostics[0].message;
    assert!(
        message.starts_with("lib.tb_tests.pass: failed to run risim-ghdl"),
        "{message}"
    );
    let paths = TestOutputPaths::new(&workspace.layout, "lib.tb_tests.pass");
    let output = fs::read_to_string(&paths.output_file).expect("output.txt");
    assert!(output.contains("failed to run risim-ghdl"), "{output}");
    assert!(
        run.events
            .iter()
            .any(|event| matches!(event, SimulationEvent::Diagnostic { .. }))
    );
}

/// Compiles the UART example of `VUnit` (`tests/operations/uart`) with the real risim-ghdl, then
/// recompiles after edits.
#[expect(clippy::print_stderr, reason = "reports timings")]
async fn real_risim_ghdl_compiles_uart() {
    let fixtures = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/operations");
    let mut workspace = Workspace::new().await;
    let ghdl = Utf8PathBuf::from(env::var("RISIM_GHDL").expect("RISIM_GHDL"));
    workspace.simulator = Simulator::detect(&ghdl).await.expect("detect risim-ghdl");
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
    workspace.spec.compile_options.a_flags = vec!["-frelaxed".to_owned()];
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
