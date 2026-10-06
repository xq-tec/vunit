// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests of workspaces: opening, request merging, cancellation, testcase locks,
//! watching, the lock and crash recovery. They use the fake simulator like the other trials.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fs;
use std::num::NonZeroUsize;
use std::time::Duration;
use std::time::Instant;

use camino::Utf8PathBuf;
use libtest_mimic::Trial;
use risim_vunit_frontend::DiagnosticSource;
use risim_vunit_frontend::OpenError;
use risim_vunit_frontend::ProjectSource;
use risim_vunit_frontend::RequestTag;
use risim_vunit_frontend::Runtime;
use risim_vunit_frontend::RuntimeOptions;
use risim_vunit_frontend::SimulationRequest;
use risim_vunit_frontend::TestCounts;
use risim_vunit_frontend::TestOutcome;
use risim_vunit_frontend::Workspace as Handle;
use risim_vunit_frontend::WorkspaceEvent;
use risim_vunit_frontend::WorkspaceEventKind;
use risim_vunit_frontend::diagnostics::Severity;
use risim_vunit_frontend::store::CompileState;
use tokio::sync::mpsc;

use crate::Workspace;
use crate::simulation_project;
use crate::testbench;
use crate::trial::trial;

pub fn trials() -> Vec<Trial> {
    vec![
        trial(
            "workspace_compiles_and_simulates_with_events",
            workspace_compiles_and_simulates_with_events,
        ),
        trial("workspace_lock_excludes_a_second_open", workspace_lock),
        trial(
            "workspace_merges_requests_while_compiling",
            workspace_merges_requests_while_compiling,
        ),
        trial("workspace_cancel_all", workspace_cancel_all),
        trial(
            "workspace_cancels_one_request",
            workspace_cancels_one_request,
        ),
        trial(
            "workspace_simulates_the_compiled_project",
            workspace_simulates_the_compiled_project,
        ),
        trial(
            "workspace_keeps_diagnostics_of_files_not_compiled",
            workspace_keeps_diagnostics_of_files_not_compiled,
        ),
        trial(
            "workspace_runs_a_testcase_once_at_a_time",
            workspace_runs_a_testcase_once_at_a_time,
        ),
        trial(
            "workspace_watches_config_and_sources",
            workspace_watches_config_and_sources,
        ),
        trial(
            "workspace_rewatches_a_recreated_directory",
            workspace_rewatches_a_recreated_directory,
        ),
        trial(
            "workspace_recovers_from_a_crash",
            workspace_recovers_from_a_crash,
        ),
        trial(
            "workspace_reports_config_errors",
            workspace_reports_config_errors,
        ),
        trial(
            "workspace_close_cancels_simulations",
            workspace_close_cancels_simulations,
        ),
        trial(
            "workspace_sees_edits_right_before_a_compile",
            workspace_sees_edits_right_before_a_compile,
        ),
        trial(
            "workspace_without_handles_finishes_and_closes",
            workspace_without_handles_finishes_and_closes,
        ),
    ]
}

// -------------------------------------------------------------------------------------------------
// Fixture
// -------------------------------------------------------------------------------------------------

/// How long to wait for an event before failing.
const TIMEOUT: Duration = Duration::from_secs(20);

const CONFIG: &str = "risim-config.toml";

async fn runtime() -> Runtime {
    let exe = Utf8PathBuf::from_path_buf(std::env::current_exe().expect("current exe"))
        .expect("UTF-8 path");
    let four = NonZeroUsize::new(4).expect("non-zero");
    Runtime::new(RuntimeOptions {
        risim_ghdl: exe,
        max_parallel_simulations: four,
        max_parallel_compiles: four,
    })
    .await
    .expect("create the runtime")
}

/// An open workspace with its events.
struct Session {
    handle: Handle,
    events: mpsc::UnboundedReceiver<WorkspaceEvent>,
}

async fn try_open(
    runtime: &Runtime,
    fixture: &Workspace,
    source: ProjectSource,
) -> Result<Session, OpenError> {
    let (sender, events) = mpsc::unbounded_channel();
    let handle = runtime
        .open_workspace(&fixture.root, source, sender)
        .await?;
    Ok(Session { handle, events })
}

async fn open(runtime: &Runtime, fixture: &Workspace, source: ProjectSource) -> Session {
    try_open(runtime, fixture, source)
        .await
        .expect("open the workspace")
}

fn config_source() -> ProjectSource {
    ProjectSource::ConfigFile(CONFIG.into())
}

fn spec_source(fixture: &Workspace) -> ProjectSource {
    ProjectSource::Spec(fixture.spec.clone())
}

#[expect(clippy::unnecessary_wraps, reason = "requests take optional tags")]
fn tag(name: &str) -> Option<RequestTag> {
    Some(RequestTag(name.to_owned()))
}

fn requests(patterns: &[&str]) -> Vec<SimulationRequest> {
    patterns
        .iter()
        .map(|pattern| SimulationRequest {
            pattern: (*pattern).to_owned(),
            gui: false,
        })
        .collect()
}

fn has_tag(tags: &[RequestTag], name: &str) -> bool {
    tags.iter().any(|tag| tag.0 == name)
}

impl Session {
    async fn next(&mut self) -> WorkspaceEventKind {
        tokio::time::timeout(TIMEOUT, self.events.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("the event channel is closed")
            .kind
    }

    /// Collects events up to and including the first one satisfying `done`.
    async fn until(
        &mut self,
        done: impl Fn(&WorkspaceEventKind) -> bool,
    ) -> Vec<WorkspaceEventKind> {
        let mut events = Vec::new();
        loop {
            let event = self.next().await;
            let stop = done(&event);
            events.push(event);
            if stop {
                return events;
            }
        }
    }

    /// Collects events up to the end of the operation tagged `name` (see `WorkspaceEventKind`).
    async fn operation(&mut self, name: &str) -> Vec<WorkspaceEventKind> {
        self.until(|event| match event {
            WorkspaceEventKind::SimulationFinished { tags, .. } => has_tag(tags, name),
            WorkspaceEventKind::CompileFinished {
                tags,
                success,
                simulation_patterns,
            } => has_tag(tags, name) && (!success || simulation_patterns.is_none()),
            _ => false,
        })
        .await
    }
}

fn tags_text(tags: &[RequestTag]) -> String {
    tags.iter()
        .map(|tag| tag.0.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// The operation events as text, without testcase and diagnostic changes.
fn summary(events: &[WorkspaceEventKind]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            WorkspaceEventKind::TestcasesChanged(_)
            | WorkspaceEventKind::DiagnosticsChanged { .. } => None,
            WorkspaceEventKind::CompileStarted { tags, total } => {
                Some(format!("compile_started {} {total}", tags_text(tags)))
            },
            WorkspaceEventKind::FileCompiling { file, .. } => Some(format!(
                "compiling {}",
                file.file_name().unwrap_or_default()
            )),
            WorkspaceEventKind::CompileFinished {
                tags,
                success,
                simulation_patterns,
            } => Some(format!(
                "compile_finished {} {success} {simulation_patterns:?}",
                tags_text(tags)
            )),
            WorkspaceEventKind::SimulationStarted { tags, testcases } => Some(format!(
                "simulation_started {} [{}]",
                tags_text(tags),
                testcases.join("; ")
            )),
            WorkspaceEventKind::TestStarted { name, .. } => Some(format!("test_started {name}")),
            WorkspaceEventKind::TestFinished { name, outcome, .. } => {
                Some(format!("test_finished {name} {outcome:?}"))
            },
            WorkspaceEventKind::SimulationFinished {
                tags,
                counts:
                    TestCounts {
                        passed,
                        failed,
                        cancelled,
                    },
            } => Some(format!(
                "simulation_finished {} {passed}/{failed}/{cancelled}",
                tags_text(tags)
            )),
        })
        .collect()
}

fn testcase_names(event: &WorkspaceEventKind) -> Option<Vec<&str>> {
    match event {
        WorkspaceEventKind::TestcasesChanged(testcases) => Some(
            testcases
                .iter()
                .map(|testcase| testcase.name.as_str())
                .collect(),
        ),
        _ => None,
    }
}

fn simulation_log(fixture: &Workspace) -> Vec<String> {
    fixture
        .take_log()
        .into_iter()
        .filter(|line| line.starts_with("sim"))
        .collect()
}

fn lock_contents(fixture: &Workspace) -> String {
    fs::read_to_string(fixture.layout.lock_file()).expect("read the lock file")
}

/// Waits until `line` appears in the fake simulator log.
async fn wait_for_log_line(fixture: &Workspace, line: &str) {
    let path = fixture.root.join(crate::fake_ghdl::LOG);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = fs::read_to_string(&path).unwrap_or_default();
        if log.lines().any(|entry| entry == line) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {line:?} in {log:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// -------------------------------------------------------------------------------------------------
// Trials
// -------------------------------------------------------------------------------------------------

async fn workspace_compiles_and_simulates_with_events() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.write(CONFIG, "[libraries.lib]\nfiles = [\"tb/*.vhd\"]\n");
    fixture.sim_directive("lib.tb_tests.fail, really", "fail");
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, config_source()).await;

    let first = session.next().await;
    assert_eq!(
        testcase_names(&first).expect("the testcases come first"),
        [
            "lib.tb_implicit.all",
            "lib.tb_tests.pass",
            "lib.tb_tests.fail, really",
            "lib.tb_tests.slow",
        ]
    );
    assert_eq!(session.handle.snapshot().testcases.len(), 4);

    session
        .handle
        .simulate(requests(&["lib.tb_tests.*", "nothing*"]), tag("s"));
    let events = summary(&session.operation("s").await);
    assert_eq!(
        events[..5],
        [
            "compile_started s 2",
            "compiling tb_implicit.vhd",
            "compiling tb_tests.vhd",
            "compile_finished s true Some([\"lib.tb_tests.*\", \"nothing*\"])",
            "simulation_started s [lib.tb_tests.fail, really; lib.tb_tests.pass; lib.tb_tests.slow]",
        ]
    );
    // Tests run concurrently, so their events interleave.
    let mut tests = events[5..events.len() - 1].to_vec();
    tests.sort();
    assert_eq!(
        tests,
        [
            "test_finished lib.tb_tests.fail, really Failed",
            "test_finished lib.tb_tests.pass Passed",
            "test_finished lib.tb_tests.slow Passed",
            "test_started lib.tb_tests.fail, really",
            "test_started lib.tb_tests.pass",
            "test_started lib.tb_tests.slow",
        ]
    );
    assert_eq!(events[events.len() - 1], "simulation_finished s 2/1/0");

    let snapshot = session.handle.snapshot();
    let outcomes: Vec<(&str, TestOutcome)> = snapshot
        .results
        .iter()
        .map(|(name, result)| (name.as_str(), result.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("lib.tb_tests.fail, really", TestOutcome::Failed),
            ("lib.tb_tests.pass", TestOutcome::Passed),
            ("lib.tb_tests.slow", TestOutcome::Passed),
        ]
    );
    let simulation = snapshot.diagnostics.get(DiagnosticSource::Simulation);
    assert_eq!(simulation.len(), 1, "{simulation:?}");
    assert_eq!(simulation[0].message, "no testcase matches 'nothing*'");

    session.handle.close().await;
    assert_eq!(lock_contents(&fixture), "");
    assert!(fixture.layout.results_file().is_file());
    assert!(fixture.layout.parse_cache_file().is_file());

    // The results survive a restart.
    let reopened = open(&runtime, &fixture, config_source()).await;
    assert_eq!(reopened.handle.snapshot().results.len(), 3);
    reopened.handle.close().await;
}

async fn workspace_lock() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    let runtime = runtime().await;
    let first = open(&runtime, &fixture, spec_source(&fixture)).await;
    let pid = lock_contents(&fixture);
    assert_eq!(pid.trim(), std::process::id().to_string());
    let second = try_open(&runtime, &fixture, spec_source(&fixture)).await;
    assert!(
        matches!(second, Err(OpenError::Locked { .. })),
        "{:?}",
        second.err()
    );
    // The failed open changed nothing.
    assert_eq!(lock_contents(&fixture), pid);
    first.handle.close().await;
    let third = open(&runtime, &fixture, spec_source(&fixture)).await;
    third.handle.close().await;
}

async fn workspace_merges_requests_while_compiling() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.write(
        "tb/tb_implicit.vhd",
        &format!("-- fake: sleep 600\n{}", testbench("tb_implicit", "")),
    );
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;

    session.handle.compile(tag("a"));
    session
        .handle
        .simulate(requests(&["lib.tb_tests.pass"]), tag("b"));
    session.handle.compile(tag("c"));
    session.handle.compile(None);
    session.handle.simulate(
        requests(&["lib.tb_implicit.all", "lib.tb_tests.pass"]),
        tag("d"),
    );
    let events = summary(&session.operation("d").await);
    assert_eq!(
        events[..7],
        [
            "compile_started a 2",
            "compiling tb_implicit.vhd",
            "compiling tb_tests.vhd",
            "compile_finished a true None",
            "compile_started b,c,d 0",
            "compile_finished b,c,d true Some([\"lib.tb_tests.pass\", \"lib.tb_implicit.all\"])",
            "simulation_started b,c,d [lib.tb_implicit.all; lib.tb_tests.pass]",
        ]
    );
    assert_eq!(events.len(), 12, "{events:#?}");
    assert_eq!(events[11], "simulation_finished b,c,d 2/0/0");
    session.handle.close().await;
}

async fn workspace_cancel_all() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.write(
        "tb/tb_implicit.vhd",
        &format!("-- fake: hang\n{}", testbench("tb_implicit", "")),
    );
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;

    // A hanging compile with a queued simulation.
    let started = Instant::now();
    session.handle.compile(tag("a"));
    session.handle.simulate(requests(&["*"]), tag("b"));
    session
        .until(|event| matches!(event, WorkspaceEventKind::FileCompiling { .. }))
        .await;
    session.handle.cancel_all();
    let compile_events = summary(&session.operation("a").await);
    assert_eq!(
        compile_events,
        [
            "compile_finished b false Some([\"*\"])",
            "compile_finished a false None",
        ]
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    // Hanging simulations; the other tests may finish before the cancel.
    fixture.write("tb/tb_implicit.vhd", &testbench("tb_implicit", ""));
    fixture.sim_directive("lib.tb_tests.slow", "hang");
    session
        .handle
        .simulate(requests(&["lib.tb_tests.*"]), tag("c"));
    session
        .until(|event| {
            matches!(event, WorkspaceEventKind::TestStarted { name, .. } if name == "lib.tb_tests.slow")
        })
        .await;
    session.handle.cancel_all();
    let simulation_events = summary(&session.operation("c").await);
    assert!(
        simulation_events.contains(&"test_finished lib.tb_tests.slow Cancelled".to_owned()),
        "{simulation_events:#?}"
    );
    let finished = simulation_events.last().expect("events");
    assert!(
        finished.starts_with("simulation_finished c ") && !finished.ends_with("/0"),
        "{finished}"
    );

    // The workspace works after a cancel.
    fixture.write(crate::fake_ghdl::SIM_DIRECTIVES, "");
    session
        .handle
        .simulate(requests(&["lib.tb_tests.slow"]), tag("d"));
    let rerun_events = summary(&session.operation("d").await);
    assert_eq!(
        rerun_events.last().map(String::as_str),
        Some("simulation_finished d 1/0/0")
    );
    session.handle.close().await;
}

async fn workspace_cancels_one_request() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.write(
        "tb/tb_implicit.vhd",
        &format!("-- fake: hang\n{}", testbench("tb_implicit", "")),
    );
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;

    // A hanging compile, and three requests merged into the queued operation.
    session.handle.compile(tag("a"));
    session.handle.simulate(requests(&["*"]), tag("b"));
    session
        .handle
        .simulate(requests(&["lib.tb_tests.slow"]), tag("e"));
    session.handle.compile(tag("c"));
    session
        .until(|event| matches!(event, WorkspaceEventKind::FileCompiling { .. }))
        .await;

    // Cancelling a request of the queued operation ends only that request.
    session.handle.cancel(RequestTag("c".to_owned()));
    assert_eq!(
        summary(&session.operation("c").await),
        ["compile_finished c false Some([\"*\", \"lib.tb_tests.slow\"])"]
    );

    // Cancelling the only request of the running compile cancels it; the queued operation
    // starts. `FileCompiling` is sent before the process reads the file, so wait until the
    // hang directive has been taken: replacing the source earlier lets this compile continue
    // into the next file.
    wait_for_log_line(&fixture, "hang").await;
    fixture.write("tb/tb_implicit.vhd", &testbench("tb_implicit", ""));
    fixture.sim_directive("lib.tb_tests.slow", "hang");
    session.handle.cancel(RequestTag("a".to_owned()));
    assert_eq!(
        summary(&session.operation("a").await),
        ["compile_finished a false None"]
    );
    let started = session
        .until(|event| {
            matches!(event, WorkspaceEventKind::TestStarted { name, .. } if name == "lib.tb_tests.slow")
        })
        .await;
    assert!(
        matches!(
            started.last(),
            Some(WorkspaceEventKind::TestStarted { tags, .. }) if tags_text(tags) == "b,e"
        ),
        "{started:#?}"
    );

    // A request that shares the simulation with another one ends, and the simulation goes on.
    session.handle.cancel(RequestTag("e".to_owned()));
    let detached = session.operation("e").await;
    assert!(
        matches!(
            detached.last(),
            Some(WorkspaceEventKind::SimulationFinished { tags, counts })
                if tags_text(tags) == "e"
                    && counts.cancelled >= 1
                    && counts.passed + counts.failed + counts.cancelled == 4
        ),
        "{detached:#?}"
    );

    // Cancelling the last request cancels the simulation.
    session.handle.cancel(RequestTag("b".to_owned()));
    let cancelled = session.operation("b").await;
    for event in &cancelled {
        if let WorkspaceEventKind::TestFinished {
            tags: test_tags, ..
        } = event
        {
            assert_eq!(tags_text(test_tags), "b");
        }
    }
    let cancelled = summary(&cancelled);
    assert!(
        cancelled.contains(&"test_finished lib.tb_tests.slow Cancelled".to_owned()),
        "{cancelled:#?}"
    );
    let finished = cancelled.last().expect("events");
    assert!(
        finished.starts_with("simulation_finished b ") && !finished.ends_with("/0"),
        "{finished}"
    );

    // A request of another client isn't affected.
    fixture.write(crate::fake_ghdl::SIM_DIRECTIVES, "");
    fixture.sim_directive("lib.tb_tests.slow", "sleep 300");
    session
        .handle
        .simulate(requests(&["lib.tb_tests.slow"]), tag("f"));
    session.handle.cancel(RequestTag("other".to_owned()));
    assert_eq!(
        summary(&session.operation("f").await)
            .last()
            .map(String::as_str),
        Some("simulation_finished f 1/0/0")
    );
    session.handle.close().await;
}

async fn workspace_simulates_the_compiled_project() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.write(
        "tb/tb_implicit.vhd",
        &format!("-- fake: sleep 3000\n{}", testbench("tb_implicit", "")),
    );
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;

    session.handle.simulate(requests(&["*"]), tag("a"));
    session
        .until(|event| matches!(event, WorkspaceEventKind::FileCompiling { .. }))
        .await;
    // A testbench added during the compile isn't compiled, so it doesn't run either.
    fixture.write("tb/tb_new.vhd", &testbench("tb_new", ""));
    let events = session.operation("a").await;
    assert!(
        events
            .iter()
            .any(|event| testcase_names(event)
                .is_some_and(|names| names.contains(&"lib.tb_new.all"))),
        "the watcher didn't report the new testbench during the compile: {events:#?}"
    );
    let events = summary(&events);
    assert!(
        events.contains(
            &"simulation_started a [lib.tb_implicit.all; lib.tb_tests.fail, really; \
              lib.tb_tests.pass; lib.tb_tests.slow]"
                .to_owned()
        ),
        "{events:#?}"
    );
    assert_eq!(
        events.last().map(String::as_str),
        Some("simulation_finished a 4/0/0")
    );

    // The next request compiles and runs it.
    session
        .handle
        .simulate(requests(&["lib.tb_new.*"]), tag("b"));
    assert_eq!(
        summary(&session.operation("b").await)
            .last()
            .map(String::as_str),
        Some("simulation_finished b 1/0/0")
    );
    session.handle.close().await;
}

async fn workspace_keeps_diagnostics_of_files_not_compiled() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    let tests_file = fixture.root.join("tb/tb_tests.vhd");
    let tests_source = fs::read_to_string(&tests_file).expect("read");
    fixture.write(
        "tb/tb_tests.vhd",
        &format!("-- fake: error broken\n{tests_source}"),
    );
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;
    let compile_messages = |current: &Session| {
        current
            .handle
            .snapshot()
            .diagnostics
            .get(DiagnosticSource::Compile)
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
    };

    session.handle.compile(tag("a"));
    session.operation("a").await;
    assert_eq!(compile_messages(&session), ["broken"]);

    // tb_implicit compiles first and hangs; the cancel keeps tb_tests from starting, and its
    // error stays.
    fixture.write(
        "tb/tb_implicit.vhd",
        &format!("-- fake: hang\n{}", testbench("tb_implicit", "")),
    );
    session.handle.compile(tag("b"));
    session
        .until(|event| matches!(event, WorkspaceEventKind::FileCompiling { .. }))
        .await;
    session.handle.cancel_all();
    session.operation("b").await;
    assert_eq!(compile_messages(&session), ["broken"]);

    // A compile that can't be planned keeps all of them.
    fixture.write("tb/tb_implicit.vhd", &testbench("tb_implicit", ""));
    fixture.write(
        "tb/tb_tests.vhd",
        &format!("-- fake: error broken\nuse work.pkg_a.all;\n{tests_source}"),
    );
    fixture.write(
        "tb/pkg_a.vhd",
        "use work.pkg_b.all;\npackage pkg_a is end package;\n",
    );
    fixture.write(
        "tb/pkg_b.vhd",
        "use work.pkg_a.all;\npackage pkg_b is end package;\n",
    );
    session.handle.compile(tag("c"));
    assert_eq!(
        summary(&session.operation("c").await),
        ["compile_started c 0", "compile_finished c false None"]
    );
    assert!(
        session
            .handle
            .snapshot()
            .diagnostics
            .get(DiagnosticSource::Project)
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "the cycle isn't reported"
    );
    assert_eq!(compile_messages(&session), ["broken"]);
    session.handle.close().await;
}

async fn workspace_runs_a_testcase_once_at_a_time() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.sim_directive("lib.tb_tests.slow", "sleep 500");
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;

    session
        .handle
        .simulate(requests(&["lib.tb_tests.slow"]), tag("first"));
    session
        .until(|event| matches!(event, WorkspaceEventKind::TestStarted { .. }))
        .await;
    session
        .handle
        .simulate(requests(&["lib.tb_tests.slow"]), tag("second"));
    let events = summary(&session.operation("second").await);
    // The lock is released when the test finishes, before its simulation reports the end.
    let first_finished = events
        .iter()
        .position(|event| event == "test_finished lib.tb_tests.slow Passed")
        .expect("the first run finishes first");
    let second_started = events
        .iter()
        .rposition(|event| event == "test_started lib.tb_tests.slow")
        .expect("the second run starts");
    assert!(first_finished < second_started, "{events:#?}");
    assert_eq!(
        events.last().map(String::as_str),
        Some("simulation_finished second 1/0/0")
    );
    assert_eq!(
        simulation_log(&fixture),
        [
            "simstart lib.tb_tests.slow",
            "simend lib.tb_tests.slow",
            "simstart lib.tb_tests.slow",
            "simend lib.tb_tests.slow",
        ]
    );
    session.handle.close().await;
}

fn has_error(event: &WorkspaceEventKind, source: DiagnosticSource) -> bool {
    matches!(
        event,
        WorkspaceEventKind::DiagnosticsChanged { source: changed, diagnostics }
            if *changed == source
                && diagnostics.iter().any(|diagnostic| diagnostic.severity == Severity::Error)
    )
}

async fn workspace_watches_config_and_sources() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.write(CONFIG, "[libraries.lib]\nfiles = [\"tb/*.vhd\"]\n");
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, config_source()).await;
    let mut events = Vec::new();
    events.extend(session.until(|event| testcase_names(event).is_some()).await);

    // A new testbench.
    fixture.write("tb/tb_new.vhd", &testbench("tb_new", ""));
    events.extend(
        session
            .until(|event| {
                testcase_names(event).is_some_and(|names| names.contains(&"lib.tb_new.all"))
            })
            .await,
    );

    // A broken configuration keeps the last valid project.
    fixture.write(CONFIG, "[libraries.lib\n");
    events.extend(
        session
            .until(|event| has_error(event, DiagnosticSource::Config))
            .await,
    );
    assert_eq!(session.handle.snapshot().testcases.len(), 5);

    // A fixed configuration replaces it.
    fixture.write(CONFIG, "[libraries.lib]\nfiles = [\"tb/tb_tests.vhd\"]\n");
    events.extend(
        session
            .until(|event| {
                matches!(
                    event,
                    WorkspaceEventKind::DiagnosticsChanged { source: DiagnosticSource::Config, diagnostics }
                        if diagnostics.is_empty()
                )
            })
            .await,
    );
    assert_eq!(session.handle.snapshot().testcases.len(), 3);

    // A deleted configuration is an error.
    fs::remove_file(fixture.root.join(CONFIG)).expect("delete the configuration");
    events.extend(
        session
            .until(|event| has_error(event, DiagnosticSource::Config))
            .await,
    );
    assert_eq!(session.handle.snapshot().testcases.len(), 3);

    // Changes never compile anything.
    assert_eq!(summary(&events), Vec::<String>::new());
    session.handle.close().await;
}

async fn workspace_recovers_from_a_crash() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    let runtime = runtime().await;
    let compiled_files = async |name: &str| {
        let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;
        session.handle.compile(tag(name));
        let events = summary(&session.operation(name).await);
        session.handle.close().await;
        events
            .iter()
            .filter(|event| event.starts_with("compiling"))
            .count()
    };

    assert_eq!(compiled_files("a").await, 2);
    assert_eq!(compiled_files("b").await, 0);
    // A PID in the lock file means that its owner crashed. The state saved after its last
    // compile matches the libraries, so nothing is compiled again…
    fs::write(fixture.layout.lock_file(), "12345\n").expect("write the lock file");
    assert_eq!(compiled_files("c").await, 0);
    // …unless it crashed while compiling: then everything is compiled again.
    let mut state = CompileState::load(&fixture.layout);
    state.compiling = true;
    state.save(&fixture.layout).expect("save the compile state");
    fs::write(fixture.layout.lock_file(), "12345\n").expect("write the lock file");
    assert_eq!(compiled_files("d").await, 2);
}

async fn workspace_reports_config_errors() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    let runtime = runtime().await;

    let missing = try_open(&runtime, &fixture, config_source()).await;
    assert!(
        matches!(missing, Err(OpenError::Config(_))),
        "{:?}",
        missing.err()
    );
    assert!(!fixture.layout.lock_file().exists());

    let missing_root = fixture.root.join("missing");
    let (sender, _events) = mpsc::unbounded_channel();
    let without_root = runtime
        .open_workspace(&missing_root, spec_source(&fixture), sender)
        .await;
    assert!(
        matches!(without_root, Err(OpenError::Io { .. })),
        "{:?}",
        without_root.err()
    );
    assert!(!missing_root.exists());

    fixture.write(CONFIG, "[libraries.lib\n");
    let mut session = open(&runtime, &fixture, config_source()).await;
    let opened = session
        .until(|event| has_error(event, DiagnosticSource::Config))
        .await;
    assert_eq!(testcase_names(&opened[0]), Some(Vec::new()));
    session.handle.compile(tag("a"));
    assert_eq!(
        summary(&session.operation("a").await),
        ["compile_started a 0", "compile_finished a false None"]
    );
    session.handle.close().await;
}

async fn workspace_close_cancels_simulations() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.sim_directive("lib.tb_tests.slow", "hang");
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;
    session
        .handle
        .simulate(requests(&["lib.tb_tests.slow"]), tag("a"));
    session
        .until(|event| matches!(event, WorkspaceEventKind::TestStarted { .. }))
        .await;
    let started = Instant::now();
    let other_handle = session.handle.clone();
    let Session { handle, mut events } = session;
    handle.close().await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(lock_contents(&fixture), "");
    let mut remaining = Vec::new();
    while let Ok(event) = events.try_recv() {
        remaining.push(event.kind);
    }
    assert_eq!(
        summary(&remaining),
        [
            "test_finished lib.tb_tests.slow Cancelled",
            "simulation_finished a 0/0/1",
        ]
    );
    // Requests to a closed workspace are ignored.
    other_handle.compile(tag("b"));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), events.recv())
            .await
            .is_ok_and(|event| event.is_none())
    );
}

async fn workspace_sees_edits_right_before_a_compile() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;
    session.handle.compile(tag("a"));
    session.operation("a").await;

    // No time for the watcher to report the edit.
    fixture.write(
        "tb/tb_implicit.vhd",
        &format!("-- edited\n{}", testbench("tb_implicit", "")),
    );
    session.handle.compile(tag("b"));
    assert_eq!(
        summary(&session.operation("b").await),
        [
            "compile_started b 1",
            "compiling tb_implicit.vhd",
            "compile_finished b true None",
        ]
    );
    session.handle.close().await;
}

async fn workspace_without_handles_finishes_and_closes() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    fixture.sim_directive("lib.tb_tests.slow", "sleep 300");
    let runtime = runtime().await;
    let Session { handle, mut events } = open(&runtime, &fixture, spec_source(&fixture)).await;
    handle.simulate(requests(&["lib.tb_tests.slow"]), tag("a"));
    drop(handle);

    // The simulation isn't cancelled; the workspace closes once it is done.
    let mut remaining = Vec::new();
    while let Some(event) = tokio::time::timeout(TIMEOUT, events.recv())
        .await
        .expect("timed out waiting for the workspace to close")
    {
        remaining.push(event.kind);
    }
    assert_eq!(
        summary(&remaining).last().map(String::as_str),
        Some("simulation_finished a 1/0/0")
    );
    assert_eq!(lock_contents(&fixture), "");
}

async fn workspace_rewatches_a_recreated_directory() {
    let mut fixture = Workspace::new().await;
    simulation_project(&mut fixture);
    let runtime = runtime().await;
    let mut session = open(&runtime, &fixture, spec_source(&fixture)).await;

    // Deleted and recreated within one debounce period, like a branch switch.
    let tb = fixture.root.join("tb");
    fs::remove_dir_all(&tb).expect("delete tb");
    simulation_project(&mut fixture);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The recreated directory is watched again.
    fixture.write("tb/tb_new.vhd", &testbench("tb_new", ""));
    session
        .until(|event| testcase_names(event).is_some_and(|names| names.contains(&"lib.tb_new.all")))
        .await;
    session.handle.close().await;
}
