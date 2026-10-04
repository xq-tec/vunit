// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Running a project with the real risim-ghdl through the public workspace API.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::num::NonZeroUsize;
use std::time::Duration;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use risim_vunit_frontend::DiagnosticSource;
use risim_vunit_frontend::ProjectSource;
use risim_vunit_frontend::RequestTag;
use risim_vunit_frontend::Runtime;
use risim_vunit_frontend::RuntimeOptions;
use risim_vunit_frontend::SimulationRequest;
use risim_vunit_frontend::TestOutcome;
use risim_vunit_frontend::Workspace;
use risim_vunit_frontend::WorkspaceEvent;
use risim_vunit_frontend::WorkspaceEventKind;
use risim_vunit_frontend::diagnostics::Severity;
use risim_vunit_frontend::spec::ConfigurationSpec;
use risim_vunit_frontend::spec::ProjectSpec;
use risim_vunit_frontend::spec::TestConfigSpec;
use tokio::sync::mpsc;

/// The environment variable naming the risim-ghdl executable.
pub const RISIM_GHDL: &str = "RISIM_GHDL";

/// How long a whole simulate operation may take.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(900);

const TAG: &str = "acceptance";

/// The directory of the upstream VHDL libraries.
pub fn vhdl_dir() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vunit/vhdl")
}

/// The directory of the acceptance fixtures.
pub fn fixtures_dir() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/acceptance")
}

/// A temporary workspace root. The name contains a space, to check that paths with spaces work,
/// as `test_artificial.py` does.
pub struct Root {
    _temp: tempfile::TempDir,
    pub path: Utf8PathBuf,
}

impl Root {
    pub fn new() -> Self {
        let temp = tempfile::Builder::new()
            .disable_cleanup(env::var_os("ACCEPTANCE_KEEP").is_some())
            .prefix("risim acceptance ")
            .tempdir()
            .expect("create a temporary directory");
        let path = Utf8Path::from_path(temp.path())
            .expect("UTF-8 path")
            .to_owned();
        Self { _temp: temp, path }
    }
}

/// A runtime with the risim-ghdl named by [`RISIM_GHDL`].
pub async fn runtime() -> Runtime {
    let ghdl = Utf8PathBuf::from(env::var(RISIM_GHDL).expect("RISIM_GHDL"));
    let parallelism =
        std::thread::available_parallelism().unwrap_or(NonZeroUsize::new(4).expect("non-zero"));
    Runtime::new(RuntimeOptions {
        risim_ghdl: ghdl,
        max_parallel_simulations: parallelism,
        max_parallel_compiles: parallelism,
    })
    .await
    .expect("create the runtime")
}

async fn open(
    runtime: &Runtime,
    root: &Utf8Path,
    spec: &ProjectSpec,
) -> (Workspace, mpsc::UnboundedReceiver<WorkspaceEvent>) {
    let (sender, events) = mpsc::unbounded_channel();
    let workspace = runtime
        .open_workspace(root, ProjectSource::Spec(spec.clone()), sender)
        .await
        .expect("open the workspace");
    (workspace, events)
}

/// The names of the testcases of `spec`, like `VUnit`'s `get_tests()` before any configuration
/// is added.
pub async fn testcase_names(runtime: &Runtime, root: &Utf8Path, spec: &ProjectSpec) -> Vec<String> {
    let (workspace, _events) = open(runtime, root, spec).await;
    let names = workspace
        .snapshot()
        .testcases
        .iter()
        .map(|testcase| testcase.name.clone())
        .collect();
    workspace.close().await;
    names
}

/// The tests of testbench `testbench` (`lib.tb`) among `names`, without the testbench prefix.
pub fn tests_of<'name>(names: &'name [String], testbench: &str) -> Vec<&'name str> {
    let prefix = format!("{testbench}.");
    names
        .iter()
        .filter_map(|name| name.strip_prefix(&prefix))
        .collect()
}

/// What a simulate operation of all testcases produced.
pub struct Outcome {
    /// The outcome of every testcase that ran.
    pub outcomes: BTreeMap<String, TestOutcome>,
    /// The simulator output file of every testcase that ran.
    pub output_paths: BTreeMap<String, Utf8PathBuf>,
    /// Errors of all sources, and warnings of the configuration and simulation sets.
    pub problems: Vec<String>,
}

/// Opens a workspace at `root` with `spec`, compiles and simulates all testcases, and closes it.
pub async fn simulate_all(runtime: &Runtime, root: &Utf8Path, spec: &ProjectSpec) -> Outcome {
    let (workspace, mut events) = open(runtime, root, spec).await;
    workspace.simulate(
        vec![SimulationRequest {
            pattern: "*".to_owned(),
            gui: false,
        }],
        Some(RequestTag(TAG.to_owned())),
    );

    let mut outcome = Outcome {
        outcomes: BTreeMap::new(),
        output_paths: BTreeMap::new(),
        problems: Vec::new(),
    };
    let deadline = tokio::time::Instant::now() + OPERATION_TIMEOUT;
    let mut running = BTreeMap::new();
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv()).await;
        if event.is_err() {
            workspace.cancel_all();
        }
        assert!(
            event.is_ok(),
            "timed out; still running: {:?}",
            running.keys().collect::<Vec<_>>()
        );
        let event = event
            .expect("checked above")
            .expect("the workspace closed early");
        match event.kind {
            WorkspaceEventKind::TestStarted { name, output_path } => {
                running.insert(name, output_path);
            },
            WorkspaceEventKind::TestFinished {
                name,
                outcome: test_outcome,
                output_path,
                ..
            } => {
                running.remove(&name);
                outcome.output_paths.insert(name.clone(), output_path);
                outcome.outcomes.insert(name, test_outcome);
            },
            WorkspaceEventKind::CompileFinished { tags, success, .. }
                if !success && has_tag(&tags) =>
            {
                break;
            },
            WorkspaceEventKind::SimulationFinished { tags, .. } if has_tag(&tags) => break,
            _ => {},
        }
    }

    let snapshot = workspace.snapshot();
    for (source, diagnostics) in snapshot.diagnostics.iter() {
        let warnings_count = matches!(
            source,
            DiagnosticSource::Config | DiagnosticSource::Simulation
        );
        for diagnostic in diagnostics {
            if diagnostic.severity == Severity::Error
                || (warnings_count && diagnostic.severity == Severity::Warning)
            {
                outcome.problems.push(format!("{source:?}: {diagnostic}"));
            }
        }
    }
    workspace.close().await;
    outcome
}

fn has_tag(tags: &[RequestTag]) -> bool {
    tags.iter().any(|tag| tag.0 == TAG)
}

impl Outcome {
    /// Checks that there are no problems, that the testcases in `failing` failed, and that all
    /// others passed. Prints the end of the output of every unexpected outcome.
    pub fn assert_all_pass_except(&self, failing: &[&str]) {
        let expected: BTreeMap<String, TestOutcome> = self
            .outcomes
            .keys()
            .map(|name| {
                let outcome = if failing.contains(&name.as_str()) {
                    TestOutcome::Failed
                } else {
                    TestOutcome::Passed
                };
                (name.clone(), outcome)
            })
            .collect();
        for name in failing {
            assert!(
                self.outcomes.contains_key(*name),
                "expected failure {name} didn't run"
            );
        }
        self.assert_outcomes(&expected);
    }

    /// Checks that there are no problems and that exactly the testcases of `expected` ran with
    /// these outcomes.
    pub fn assert_outcomes(&self, expected: &BTreeMap<String, TestOutcome>) {
        assert_eq!(self.problems, Vec::<String>::new(), "diagnostics");
        assert!(!self.outcomes.is_empty(), "no testcase ran");
        let mut report = String::new();
        let mut names: Vec<&String> = self.outcomes.keys().chain(expected.keys()).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            let (actual, wanted) = (self.outcomes.get(name), expected.get(name));
            if actual != wanted {
                let tail = self
                    .output_paths
                    .get(name)
                    .map_or_else(String::new, |path| {
                        let contents = fs::read_to_string(path).unwrap_or_default();
                        let lines: Vec<&str> = contents.lines().collect();
                        lines[lines.len().saturating_sub(8)..].join("\n    ")
                    });
                writeln!(
                    report,
                    "{name}: expected {wanted:?}, got {actual:?}\n    {tail}"
                )
                .expect("write to a string");
            }
        }
        assert!(report.is_empty(), "unexpected outcomes:\n{report}");
    }
}

/// A map from testcase names to outcomes.
pub fn outcomes(pairs: &[(&str, TestOutcome)]) -> BTreeMap<String, TestOutcome> {
    pairs
        .iter()
        .map(|&(name, outcome)| (name.to_owned(), outcome))
        .collect()
}

/// Generic values from `(name, value)` pairs.
pub fn generics(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// A test configuration of `target` (`lib.tb` or `lib.tb.test`) that changes nothing yet.
pub fn target(target: &str) -> TestConfigSpec {
    TestConfigSpec {
        target: target.to_owned(),
        ..TestConfigSpec::default()
    }
}

/// A configuration named `name` that changes nothing yet.
pub fn config(name: &str) -> ConfigurationSpec {
    ConfigurationSpec {
        name: name.to_owned(),
        ..ConfigurationSpec::default()
    }
}

/// The paths of the `.vhd` files in `dir`, sorted, without those for which `skip` is true.
pub fn vhd_files(dir: &Utf8Path, skip: impl Fn(&str) -> bool) -> Vec<String> {
    let mut files: Vec<String> = fs::read_dir(dir)
        .expect("read the directory")
        .map(|entry| Utf8PathBuf::from_path_buf(entry.expect("entry").path()).expect("UTF-8"))
        .filter(|path| path.extension() == Some("vhd"))
        .filter(|path| !skip(path.file_name().unwrap_or_default()))
        .map(String::from)
        .collect();
    files.sort();
    files
}
