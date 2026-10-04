// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Discovery of testbenches and their tests, and the testcases to run.
//!
//! A port of `test/bench.py`, `test/bench_list.py` and the naming in `test/suites.py`.
//!
//! The VHDL parser records the test markers of every file in a [`TestScan`], so discovery works
//! on parse results and doesn't read files. Discovery then:
//!
//! - finds testbenches: entities with a `runner_cfg` generic (`tb_filter`);
//! - checks that each testbench has exactly one architecture, and scans that architecture's file
//!   for tests and attributes;
//! - creates the configurations of every test from the project's
//!   [`TestConfigSpec`]s and names the resulting testcases.
//!
//! Differences from `VUnit`:
//!
//! - Problems are reported as diagnostics. A testbench with errors (duplicate tests, invalid
//!   attributes, no or several architectures) is skipped instead of aborting the run.
//! - Every test runs in its own simulation; `run_all_in_same_sim` is accepted and ignored.
//! - Testcase names keep the case of the entity declaration (`VUnit` lowercases them).
//! - Test names that differ only in case produce a warning, since patterns can't tell them apart.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use regex::bytes::Regex;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use serde::Deserialize;
use serde::Serialize;

use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::configuration::ConfigurationSet;
use crate::configuration::FAIL_ON_WARNING;
use crate::configuration::RUN_ALL_IN_SAME_SIM;
use crate::configuration::TestbenchInfo;
use crate::configuration::is_user_attribute;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::LineIndex;
use crate::diagnostics::Position;
use crate::diagnostics::Range;
use crate::diagnostics::Severity;
use crate::project::FileId;
use crate::project::LibraryId;
use crate::project::Project;
use crate::spec::AssertLevel;
use crate::spec::SimOptions;
use crate::spec::TestConfigSpec;
use crate::vhdl_parser::NO_FLAGS;
use crate::vhdl_parser::VhdlEntity;
use crate::vhdl_parser::latin1;
use crate::vhdl_parser::python_regex;
use crate::vhdl_standard::VhdlStandard;

#[cfg(test)]
mod tests;

/// A test that can be run, as shown to clients.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Testcase {
    /// The full name: `lib.tb[.config].test`, or `lib.tb[.config]` / `lib.tb.all` for a
    /// testbench without explicit tests.
    pub name: String,
    /// The absolute path of the file with the test.
    pub file: Utf8PathBuf,
    /// The location of the test name in `run("…")`, or of `test_runner_setup` for a testbench
    /// without explicit tests.
    pub range: Range,
    /// User attribute names of the test and its configuration, sorted.
    pub attributes: Vec<String>,
}

// -------------------------------------------------------------------------------------------------
// Scanning
// -------------------------------------------------------------------------------------------------

/// The test markers found in a VHDL file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestScan {
    /// Explicit tests (`run("…")`), in file order.
    pub tests: Vec<ScannedTest>,
    /// The first `test_runner_setup(` call, the location of an implicit test.
    pub suite: Option<ScannedTest>,
    /// Attributes: first all legacy `vunit_pragma`s, then all `vunit:` attributes.
    pub attributes: Vec<ScannedAttribute>,
}

/// A test name or the `test_runner_setup` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedTest {
    /// The test name, or `test_runner_setup` as written.
    pub name: String,
    /// The byte offset of the name.
    pub offset: usize,
    /// The location of the name.
    pub range: Range,
}

/// An attribute in a comment: `-- vunit: name` or the legacy `vunit_pragma name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedAttribute {
    /// The attribute name; user attributes start with `.`.
    pub name: String,
    /// Whether this is a legacy `vunit_pragma`, which always applies to the whole file.
    pub legacy: bool,
    /// The byte offset of the name.
    pub offset: usize,
    /// The location of the name.
    pub range: Range,
}

/// The first character of a file.
const FILE_START: Range = Range {
    start: Position::new(1, 1),
    end: Position::new(1, 1),
};

static TEST_CASE_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r#"(\s|\()+run\s*\(\s*"(?P<name>.*?)"\s*\)"#, NO_FLAGS));

static TEST_SUITE_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"(?P<name>test_runner_setup)\s*\(", NO_FLAGS));

static ATTRIBUTE_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"vunit:\s*(?P<name>\.?[a-zA-Z0-9_\-]+)", NO_FLAGS));

static PRAGMA_LEGACY_RE: LazyLock<Regex> =
    LazyLock::new(|| python_regex(r"vunit_pragma\s+(?P<name>[a-zA-Z0-9_\-]+)", NO_FLAGS));

/// Scans a file for tests (`_find_tests`) and attributes (`_find_attributes`).
///
/// `code` is `source` with comments blanked out. Tests are searched in `code`, attributes in
/// `source`, since they are written in comments.
pub(crate) fn scan_tests(source: &[u8], code: &[u8], lines: &LineIndex<'_>) -> TestScan {
    let scanned = |captures: &regex::bytes::Captures<'_>| {
        captures.name("name").map(|name| ScannedTest {
            name: latin1(name.as_bytes()),
            offset: name.start(),
            range: lines.range(name.range()),
        })
    };
    let tests = TEST_CASE_RE
        .captures_iter(code)
        .filter_map(|captures| scanned(&captures))
        .collect();
    let suite = TEST_SUITE_RE
        .captures(code)
        .and_then(|captures| scanned(&captures));
    let mut attributes = Vec::new();
    for (regex, legacy) in [(&*PRAGMA_LEGACY_RE, true), (&*ATTRIBUTE_RE, false)] {
        attributes.extend(regex.captures_iter(source).filter_map(|captures| {
            scanned(&captures).map(|found| ScannedAttribute {
                name: found.name,
                legacy,
                offset: found.offset,
                range: found.range,
            })
        }));
    }
    TestScan {
        tests,
        suite,
        attributes,
    }
}

// -------------------------------------------------------------------------------------------------
// Tests of a testbench
// -------------------------------------------------------------------------------------------------

/// A test of a testbench after attributes have been associated with it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchTest {
    /// `None` for the implicit test.
    name: Option<String>,
    range: Range,
    /// User attribute names.
    attributes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchTests {
    /// The explicit tests, or the implicit one.
    tests: Vec<BenchTest>,
    fail_on_warning: bool,
}

/// Associates attributes with tests and checks them (`_find_tests_and_attributes` and
/// `TestBench.scan_tests_from_file`).
///
/// Returns `None` if there are errors; they are added to `diagnostics`.
fn bench_tests(
    scan: &TestScan,
    file: &Utf8Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<BenchTests> {
    let errors_before = count_errors(diagnostics);
    let report = |sink: &mut Vec<Diagnostic>, diagnostic: Diagnostic, range| {
        sink.push(diagnostic.in_file(file).at(Some(range)));
    };

    for attribute in &scan.attributes {
        if !is_user_attribute(&attribute.name) && !is_builtin_attribute(&attribute.name) {
            report(
                diagnostics,
                Diagnostic::error(format!("invalid attribute '{}'", attribute.name)),
                attribute.range,
            );
        }
    }

    check_duplicate_tests(&scan.tests, file, diagnostics);

    // In file order.
    let mut tests: Vec<LocatedTest<'_>> = if scan.tests.is_empty() {
        let (offset, range) = scan.suite.as_ref().map_or_else(
            || {
                report(
                    diagnostics,
                    Diagnostic::warning("found no tests or test suite (test_runner_setup)"),
                    FILE_START,
                );
                (0, FILE_START)
            },
            |suite| (suite.offset, suite.range),
        );
        vec![LocatedTest {
            offset,
            test: BenchTest {
                name: None,
                range,
                attributes: Vec::new(),
            },
            attributes: Vec::new(),
        }]
    } else {
        scan.tests
            .iter()
            .map(|test| LocatedTest {
                offset: test.offset,
                test: BenchTest {
                    name: Some(test.name.clone()),
                    range: test.range,
                    attributes: Vec::new(),
                },
                attributes: Vec::new(),
            })
            .collect()
    };

    // An attribute belongs to the closest preceding test; legacy pragmas are always global.
    let mut global = Vec::new();
    for attribute in &scan.attributes {
        let index = tests.partition_point(|test| test.offset <= attribute.offset);
        match index.checked_sub(1) {
            Some(test_index) if !attribute.legacy => tests[test_index].attributes.push(attribute),
            _ => global.push(attribute),
        }
    }

    for located in &tests {
        check_duplicate_attributes(&located.attributes, Some(&located.test), file, diagnostics);
    }
    check_duplicate_attributes(&global, None, file, diagnostics);

    let mut fail_on_warning = false;
    for attribute in &global {
        if is_user_attribute(&attribute.name) {
            report(
                diagnostics,
                Diagnostic::error(format!(
                    "file global attributes are not yet supported: {}",
                    attribute.name
                )),
                attribute.range,
            );
        } else if attribute.name == FAIL_ON_WARNING {
            fail_on_warning = true;
        }
    }
    for LocatedTest {
        test, attributes, ..
    } in &mut tests
    {
        for attribute in attributes.iter() {
            if is_builtin_attribute(&attribute.name) {
                report(
                    diagnostics,
                    Diagnostic::error(format!(
                        "attribute {} is global and can't be associated with {}",
                        attribute.name,
                        describe_test(test)
                    )),
                    attribute.range,
                );
            } else if is_user_attribute(&attribute.name) {
                test.attributes.push(attribute.name.clone());
            }
        }
    }

    (count_errors(diagnostics) == errors_before).then(|| BenchTests {
        tests: tests.into_iter().map(|located| located.test).collect(),
        fail_on_warning,
    })
}

/// A test of a testbench file, with its position and the attributes that belong to it.
struct LocatedTest<'scan> {
    /// The byte offset of the test in the file.
    offset: usize,
    test: BenchTest,
    attributes: Vec<&'scan ScannedAttribute>,
}

fn is_builtin_attribute(name: &str) -> bool {
    name == FAIL_ON_WARNING || name == RUN_ALL_IN_SAME_SIM
}

fn count_errors(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .count()
}

fn describe_test(test: &BenchTest) -> String {
    test.name.as_ref().map_or_else(
        || "the implicit test".to_owned(),
        |name| format!("test '{name}'"),
    )
}

/// `_check_duplicate_tests`, plus a warning for names that differ only in case.
fn check_duplicate_tests(
    tests: &[ScannedTest],
    file: &Utf8Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut known: FxHashMap<&str, &ScannedTest> = FxHashMap::default();
    let mut known_lowercase: FxHashMap<String, &ScannedTest> = FxHashMap::default();
    for test in tests {
        if let Some(previous) = known.get(test.name.as_str()) {
            diagnostics.push(
                Diagnostic::error(format!(
                    "duplicate test \"{}\", previously defined on line {}",
                    test.name, previous.range.start.line
                ))
                .in_file(file)
                .at(Some(test.range)),
            );
            continue;
        }
        known.insert(&test.name, test);
        let lowercase = test.name.to_ascii_lowercase();
        if let Some(previous) = known_lowercase.get(&lowercase) {
            diagnostics.push(
                Diagnostic::warning(format!(
                    "test \"{}\" differs from test \"{}\" on line {} only in case; testcase \
                     patterns can't tell them apart",
                    test.name, previous.name, previous.range.start.line
                ))
                .in_file(file)
                .at(Some(test.range)),
            );
        } else {
            known_lowercase.insert(lowercase, test);
        }
    }
}

/// `_check_duplicates`.
fn check_duplicate_attributes(
    attributes: &[&ScannedAttribute],
    test: Option<&BenchTest>,
    file: &Utf8Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut previous: FxHashMap<&str, &ScannedAttribute> = FxHashMap::default();
    for &attribute in attributes {
        if let Some(earlier) = previous.get(attribute.name.as_str()) {
            let of = test.map_or_else(String::new, |test| format!(" of {}", describe_test(test)));
            diagnostics.push(
                Diagnostic::error(format!(
                    "duplicate attribute {}{of}, previously defined on line {}",
                    attribute.name, earlier.range.start.line
                ))
                .in_file(file)
                .at(Some(attribute.range)),
            );
        } else {
            previous.insert(&attribute.name, attribute);
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Testbenches and testcases
// -------------------------------------------------------------------------------------------------

/// A testbench: an entity with a `runner_cfg` generic and exactly one architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Testbench {
    /// The library of the entity.
    pub library: LibraryId,
    /// The library name, in the case it was defined with.
    pub library_name: String,
    /// The entity name, in the case it is declared with.
    pub entity: String,
    /// The file with the entity declaration.
    pub entity_file: FileId,
    /// The lowercase architecture name.
    pub architecture: String,
    /// The file with the architecture, which contains the tests.
    pub architecture_file: FileId,
    /// The lowercase generic names of the entity.
    pub generic_names: Vec<String>,
    /// The VHDL standard of the entity's file.
    pub vhdl_standard: VhdlStandard,
}

/// One simulation run: a test of a testbench in one configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    /// The testcase as shown to clients.
    pub testcase: Testcase,
    /// The index of the testbench in [`Discovery::testbenches`].
    pub testbench: usize,
    /// The test name, or `None` for a testbench without explicit tests.
    pub test: Option<String>,
    /// The configuration to run the test in.
    pub configuration: Configuration,
}

/// The testbenches and testcases of a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// The testbenches, grouped by library in project order.
    pub testbenches: Vec<Testbench>,
    /// All runs; per testbench, test-major and then by configuration.
    pub runs: Vec<TestRun>,
    /// Problems with testbenches, tests and attributes.
    pub project_diagnostics: Vec<Diagnostic>,
    /// Problems with test configurations.
    pub config_diagnostics: Vec<Diagnostic>,
}

impl Discovery {
    /// The testcases, in run order.
    pub fn testcases(&self) -> Vec<Testcase> {
        self.runs.iter().map(|run| run.testcase.clone()).collect()
    }

    /// The run with the testcase name `name`.
    pub fn run(&self, name: &str) -> Option<&TestRun> {
        self.runs.iter().find(|run| run.testcase.name == name)
    }
}

const TB_PATTERN: &str = "^(tb_.*)|(.*_tb)$";

/// Whether `name` matches [`TB_PATTERN`].
fn has_tb_name(name: &str) -> bool {
    let lowercase = name.to_ascii_lowercase();
    lowercase.starts_with("tb_") || lowercase.ends_with("_tb")
}

/// `tb_filter`: whether the entity is a testbench, with warnings for suspicious names.
fn tb_filter(entity: &VhdlEntity, file: &Utf8Path, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let has_runner_cfg = entity
        .generics
        .iter()
        .any(|generic| generic.identifier == "runner_cfg");
    let has_tb_name = has_tb_name(&entity.identifier);
    let message = match (has_runner_cfg, has_tb_name) {
        (false, true) => Some(format!(
            "entity {} matches the testbench name pattern {TB_PATTERN} but has no generic \
             runner_cfg and will therefore not be run",
            entity.declared_name
        )),
        (true, false) => Some(format!(
            "entity {} has a generic runner_cfg, but its name doesn't match the testbench name \
             pattern {TB_PATTERN}",
            entity.declared_name
        )),
        _ => None,
    };
    if let Some(message) = message {
        diagnostics.push(
            Diagnostic::warning(message)
                .in_file(file)
                .at(Some(entity.range)),
        );
    }
    has_runner_cfg
}

fn testbench_info<'a>(testbench: &'a Testbench, entity_path: &'a Utf8Path) -> TestbenchInfo<'a> {
    TestbenchInfo {
        library: &testbench.library_name,
        entity: &testbench.entity,
        generic_names: &testbench.generic_names,
        file: entity_path,
    }
}

/// A testbench while its configurations are built.
struct BenchState {
    testbench: Testbench,
    entity_path: Utf8PathBuf,
    architecture_path: Utf8PathBuf,
    tests: Vec<BenchTest>,
    /// One set per test.
    configurations: Vec<ConfigurationSet>,
}

impl BenchState {
    fn info(&self) -> TestbenchInfo<'_> {
        testbench_info(&self.testbench, &self.entity_path)
    }

    fn is_implicit(&self) -> bool {
        self.tests.first().is_some_and(|test| test.name.is_none())
    }
}

/// Finds the testbenches and testcases of `project` and applies `test_configs` in order.
pub fn discover(project: &Project, test_configs: &[TestConfigSpec]) -> Discovery {
    let mut discovery = Discovery::default();
    let candidates = find_testbench_entities(project, &mut discovery.project_diagnostics);

    let mut benches: Vec<BenchState> = Vec::new();
    // Testbenches skipped because of errors; configurations targeting them are ignored.
    let mut skipped: FxHashSet<(LibraryId, String)> = FxHashSet::default();
    for (file_id, entity) in candidates {
        match bench_state(project, file_id, entity, &mut discovery.project_diagnostics) {
            Some(bench) => benches.push(bench),
            None => {
                skipped.insert((project.file(file_id).library, entity.identifier.clone()));
            },
        }
    }

    for spec in test_configs {
        apply_test_config(
            project,
            &mut benches,
            &skipped,
            spec,
            &mut discovery.config_diagnostics,
        );
    }

    for (index, bench) in benches.into_iter().enumerate() {
        let prefix = format!(
            "{}.{}",
            bench.testbench.library_name, bench.testbench.entity
        );
        for (test, configurations) in bench.tests.iter().zip(&bench.configurations) {
            for configuration in configurations.to_run() {
                let mut name = prefix.clone();
                if let Some(configuration_name) = &configuration.name {
                    name.push('.');
                    name.push_str(configuration_name);
                }
                match &test.name {
                    Some(test_name) => {
                        name.push('.');
                        name.push_str(test_name);
                    },
                    None if configuration.is_default() => name.push_str(".all"),
                    None => {},
                }
                let attributes: BTreeSet<String> = test
                    .attributes
                    .iter()
                    .chain(configuration.attributes.keys())
                    .cloned()
                    .collect();
                discovery.runs.push(TestRun {
                    testcase: Testcase {
                        name,
                        file: bench.architecture_path.clone(),
                        range: test.range,
                        attributes: attributes.into_iter().collect(),
                    },
                    testbench: index,
                    test: test.name.clone(),
                    configuration: configuration.clone(),
                });
            }
        }
        discovery.testbenches.push(bench.testbench);
    }
    discovery
}

/// The entities with a `runner_cfg` generic, grouped by library in project order
/// (`TestBenchList`). An entity defined again replaces the earlier one at its position.
fn find_testbench_entities<'project>(
    project: &'project Project,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<(FileId, &'project VhdlEntity)> {
    let mut files_by_library: FxHashMap<LibraryId, Vec<FileId>> = FxHashMap::default();
    for (file_id, file) in project.files() {
        files_by_library
            .entry(file.library)
            .or_default()
            .push(file_id);
    }

    let mut candidates = Vec::new();
    for (library_id, _) in project.libraries() {
        let mut positions: FxHashMap<&str, usize> = FxHashMap::default();
        for &file_id in files_by_library.get(&library_id).into_iter().flatten() {
            let file = project.file(file_id);
            let Some(design_file) = &file.design_file else {
                continue;
            };
            for entity in &design_file.entities {
                if !tb_filter(entity, &file.path, diagnostics) {
                    continue;
                }
                if let Some(&position) = positions.get(entity.identifier.as_str()) {
                    candidates[position] = (file_id, entity);
                } else {
                    positions.insert(&entity.identifier, candidates.len());
                    candidates.push((file_id, entity));
                }
            }
        }
    }
    candidates
}

/// Checks the architecture of a testbench and finds its tests.
fn bench_state(
    project: &Project,
    entity_file: FileId,
    entity: &VhdlEntity,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<BenchState> {
    let file = project.file(entity_file);
    let library = project.library(file.library);
    let architectures = library.architectures(&entity.identifier);
    let error = |message: String| {
        Diagnostic::error(message)
            .in_file(&file.path)
            .at(Some(entity.range))
    };
    let (architecture, architecture_file) = match architectures {
        [single] => single.clone(),
        [] => {
            diagnostics.push(error(format!(
                "testbench '{}' has no architecture",
                entity.declared_name
            )));
            return None;
        },
        several => {
            let mut names: Vec<String> = several
                .iter()
                .map(|(name, id)| {
                    format!(
                        "{name}:{}",
                        project.file(*id).path.file_name().unwrap_or_default()
                    )
                })
                .collect();
            names.sort();
            diagnostics.push(error(format!(
                "testbench '{}' isn't allowed to have several architectures; it has {}",
                entity.declared_name,
                names.join(", ")
            )));
            return None;
        },
    };

    let architecture_path = project.file(architecture_file).path.clone();
    let scan = project
        .file(architecture_file)
        .design_file
        .as_ref()
        .map(|design_file| &design_file.tests)?;
    let bench_tests = bench_tests(scan, &architecture_path, diagnostics)?;

    let mut bench = BenchState {
        testbench: Testbench {
            library: file.library,
            library_name: library.name.clone(),
            entity: entity.declared_name.clone(),
            entity_file,
            architecture,
            architecture_file,
            generic_names: entity
                .generics
                .iter()
                .map(|generic| generic.identifier.clone())
                .collect(),
            vhdl_standard: file.vhdl_standard,
        },
        entity_path: file.path.clone(),
        architecture_path,
        tests: bench_tests.tests,
        configurations: Vec::new(),
    };
    let mut default = Configuration::default_for(&bench.info());
    if bench_tests.fail_on_warning {
        default.sim_options = default.sim_options.overridden_by(&SimOptions {
            vhdl_assert_stop_level: Some(AssertLevel::Warning),
            ..SimOptions::default()
        });
    }
    bench.configurations = vec![ConfigurationSet::new(default); bench.tests.len()];
    Some(bench)
}

/// Applies one [`TestConfigSpec`]: first its generics, simulation options and VHDL configuration
/// to all configurations of the target, then its configurations, each starting as a copy of the
/// target's default configuration.
///
/// For a testbench target, every test gets the configurations. A configuration that can't be
/// added to one of them (for example because a test already has one with that name) is added
/// to none.
fn apply_test_config(
    project: &Project,
    benches: &mut [BenchState],
    skipped: &FxHashSet<(LibraryId, String)>,
    spec: &TestConfigSpec,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target_error = |message: &str| {
        Diagnostic::warning(format!("configuration target '{}': {message}", spec.target))
    };
    let mut parts = spec.target.splitn(3, '.');
    let (Some(library_name), Some(entity_name)) = (parts.next(), parts.next()) else {
        diagnostics.push(target_error(
            "expected 'library.testbench' or 'library.testbench.test'",
        ));
        return;
    };
    let test_name = parts.next();
    let Some(library) = project.find_library(library_name) else {
        diagnostics.push(target_error("no such library"));
        return;
    };
    let entity_lowercase = entity_name.to_ascii_lowercase();
    let Some(bench) = benches.iter_mut().find(|bench| {
        bench.testbench.library == library
            && bench.testbench.entity.eq_ignore_ascii_case(entity_name)
    }) else {
        if !skipped.contains(&(library, entity_lowercase)) {
            diagnostics.push(target_error("no such testbench"));
        }
        return;
    };

    let indices: Vec<usize> = match test_name {
        None => (0..bench.tests.len()).collect(),
        Some(_) if bench.is_implicit() => {
            diagnostics.push(target_error("the testbench has no explicit tests"));
            return;
        },
        Some(test_name) => {
            let Some(index) = bench
                .tests
                .iter()
                .position(|test| test.name.as_deref() == Some(test_name))
            else {
                diagnostics.push(target_error("no such test"));
                return;
            };
            vec![index]
        },
    };

    // Not `bench.info()`: the configurations are borrowed mutably below.
    let info = testbench_info(&bench.testbench, &bench.entity_path);
    let mut warnings = Vec::new();
    for (name, value) in &spec.generics {
        for &index in &indices {
            if let Err(warning) = bench.configurations[index].set_generic(&info, name, value) {
                warnings.push(warning);
                break;
            }
        }
    }
    for &index in &indices {
        let set = &mut bench.configurations[index];
        set.set_sim_options(&spec.sim_options);
        if let Some(name) = &spec.vhdl_configuration_name {
            set.set_vhdl_configuration_name(name);
        }
    }
    for configuration in &spec.configurations {
        // A configuration is added to all tests of the target or, if it fails for one, to none.
        let mut updated = Vec::with_capacity(indices.len());
        let mut added_warnings = Vec::new();
        let result = indices.iter().try_for_each(|&index| {
            let mut set = bench.configurations[index].clone();
            added_warnings.extend(set.add(&info, configuration)?);
            updated.push((index, set));
            Ok::<_, ConfigurationError>(())
        });
        match result {
            Ok(()) => {
                for (index, set) in updated {
                    bench.configurations[index] = set;
                }
                warnings.extend(added_warnings);
            },
            Err(error) => diagnostics.push(Diagnostic::error(format!(
                "configuration target '{}': {error}",
                spec.target
            ))),
        }
    }
    // Every test of a testbench produces the same warning about a generic.
    let mut seen = FxHashSet::default();
    diagnostics.extend(
        warnings
            .into_iter()
            .filter(|warning| seen.insert(warning.message.clone())),
    );
}
