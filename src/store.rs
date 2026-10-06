// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The `risim-out/` directory: its layout, the JSON state files, and the lock.
//!
//! Replaces `database.py` and `hashing.py`. All state is JSON with a top-level `"version"`;
//! a file with another version, or one that can't be parsed, is treated as missing.
//!
//! ```text
//! <workspace>/risim-out/
//! ├── .lock                     locked while the workspace is open, holds the owner's PID
//! ├── state.json                compile state and simulator identity
//! ├── parse_cache.json          per-file parse results
//! ├── builtins/<hash>/…         extracted VUnit/OSVVM sources
//! ├── libraries/<lib>/          risim-ghdl work directories
//! ├── compile_output/<lib>/<file-stem>_<hash>.txt
//! ├── test_output/<safe-name>_<hash>/{output.txt, vunit_results}
//! └── results.json              last result per testcase
//! ```
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::fs::File;
use std::fs::TryLockError;
use std::io;
use std::io::Read as _;
use std::io::Seek as _;
use std::io::Write;
use std::process;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::diagnostics::Diagnostic;
use crate::simulator::SimulatorIdentity;
pub use crate::sources::OUTPUT_DIR;
use crate::sync::lock_unpoisoned;

/// The paths inside `risim-out/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLayout {
    root: Utf8PathBuf,
}

impl OutputLayout {
    /// The layout of `<workspace_root>/risim-out`.
    pub fn new(workspace_root: &Utf8Path) -> Self {
        Self {
            root: workspace_root.join(OUTPUT_DIR),
        }
    }

    /// `risim-out/` itself.
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// The lock file.
    pub fn lock_file(&self) -> Utf8PathBuf {
        self.root.join(".lock")
    }

    /// The compile state.
    pub fn state_file(&self) -> Utf8PathBuf {
        self.root.join("state.json")
    }

    /// The persistent parse cache.
    pub fn parse_cache_file(&self) -> Utf8PathBuf {
        self.root.join("parse_cache.json")
    }

    /// The parent directory of the extracted builtins.
    pub fn builtins_root(&self) -> Utf8PathBuf {
        self.root.join("builtins")
    }

    /// The parent directory of all library work directories.
    pub fn libraries_dir(&self) -> Utf8PathBuf {
        self.root.join("libraries")
    }

    /// The work directory of `library`.
    ///
    /// Library names are case-insensitive, so the directory name is lowercase.
    pub fn library_dir(&self, library: &str) -> Utf8PathBuf {
        self.libraries_dir().join(library.to_ascii_lowercase())
    }

    /// The file receiving the output of compiling `source` into `library`.
    pub fn compile_output_file(&self, library: &str, source: &Utf8Path) -> Utf8PathBuf {
        let stem = source.file_stem().unwrap_or("file");
        self.root
            .join("compile_output")
            .join(library.to_ascii_lowercase())
            .join(format!("{stem}_{}.txt", short_hash(source.as_str())))
    }

    /// The last test results.
    pub fn results_file(&self) -> Utf8PathBuf {
        self.root.join("results.json")
    }

    /// The parent directory of all test output directories.
    pub fn test_output_root(&self) -> Utf8PathBuf {
        self.root.join("test_output")
    }

    /// The output directory of `testcase`: `<safe-name>_<hash>`.
    ///
    /// The safe name replaces every character except `[A-Za-z0-9._]` with `_`. On Windows, it is
    /// shortened like VUnit does, so that paths stay below 260 characters with a margin of 100
    /// characters for the files inside.
    pub fn test_output_dir(&self, testcase: &str) -> Utf8PathBuf {
        let root = self.test_output_root();
        let max_safe_len = if cfg!(windows) {
            const MAX_PATH: usize = 260;
            const MARGIN: usize = 100;
            const HASH_LEN: usize = 16;
            // VUnit measures the root without the separator before the directory name.
            Some(MAX_PATH.saturating_sub(MARGIN + root.as_str().len() + HASH_LEN))
        } else {
            None
        };
        root.join(test_output_name(testcase, max_safe_len))
    }
}

/// The name of a test output directory: the safe name, `_`, and the short hash of `testcase`.
/// The safe name and its `_` are cut to `max_safe_len` characters.
fn test_output_name(testcase: &str, max_safe_len: Option<usize>) -> String {
    let mut name: String = testcase
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    name.push('_');
    if let Some(max) = max_safe_len {
        // The safe name is ASCII, so any length is a character boundary.
        name.truncate(max);
    }
    name.push_str(&short_hash(testcase));
    name
}

/// The files of a test output directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutputPaths {
    /// The directory, passed to the testbench as `output path`.
    pub dir: Utf8PathBuf,
    /// The simulator output (stdout and stderr).
    pub output_file: Utf8PathBuf,
    /// The file the VUnit runner writes its progress to.
    pub results_file: Utf8PathBuf,
}

impl TestOutputPaths {
    /// The paths for `testcase`.
    pub fn new(layout: &OutputLayout, testcase: &str) -> Self {
        let dir = layout.test_output_dir(testcase);
        let output_file = dir.join("output.txt");
        let results_file = dir.join("vunit_results");
        Self {
            dir,
            output_file,
            results_file,
        }
    }

    /// Recreates the directory empty, with an empty results file, as VUnit's
    /// `_prepare_test_suite_output_path` and `TestRun.run` do.
    ///
    /// # Errors
    ///
    /// Fails if the directory can't be deleted or created.
    pub fn prepare(&self) -> io::Result<()> {
        remove_path(&self.dir)?;
        fs::create_dir_all(&self.dir)?;
        File::create(&self.results_file)?;
        Ok(())
    }
}

/// The first 16 hex digits of the blake3 hash of `text`.
pub fn short_hash(text: &str) -> String {
    let hash = blake3::hash(text.as_bytes()).to_hex();
    hash.as_str().get(..16).unwrap_or_default().to_owned()
}

// -------------------------------------------------------------------------------------------------
// JSON files
// -------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

/// Reads a JSON file written by [`write_json`] with the same `version`.
///
/// Returns `None`, and logs why, if the file is missing, unreadable, unparsable, or has
/// another version.
pub fn read_json<T: DeserializeOwned>(path: &Utf8Path, version: u32) -> Option<T> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%path, %error, "failed to read state file");
            return None;
        },
    };
    match serde_json::from_slice::<VersionOnly>(&contents) {
        Ok(found) if found.version == version => {},
        Ok(found) => {
            tracing::info!(%path, found = found.version, expected = version, "ignoring state file with another version");
            return None;
        },
        Err(error) => {
            tracing::warn!(%path, %error, "ignoring unparsable state file");
            return None;
        },
    }
    match serde_json::from_slice(&contents) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%path, %error, "ignoring unparsable state file");
            None
        },
    }
}

#[derive(Serialize)]
struct Versioned<'value, T> {
    version: u32,
    #[serde(flatten)]
    value: &'value T,
}

/// Writes `value` as JSON with a top-level `"version"`, atomically: the data goes to a
/// temporary file, which is synced and then renamed over `path`.
///
/// `T` must serialize to a JSON object.
///
/// # Errors
///
/// Fails if the file can't be written.
pub fn write_json<T: Serialize>(path: &Utf8Path, version: u32, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec(&Versioned { version, value }).map_err(io::Error::other)?;
    write_atomic(path, &json)
}

/// Distinguishes the temporary files of concurrent writes in this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `contents` to `path` atomically, creating the parent directory.
///
/// # Errors
///
/// Fails if the file can't be written.
pub fn write_atomic(path: &Utf8Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Utf8Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().unwrap_or("file");
    let temp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = write_synced_and_rename(&temp, path, contents);
    if result.is_err() {
        let _ignored = fs::remove_file(&temp);
    }
    result
}

/// Writes `contents` to `temp`, flushes it to disk and renames it to `path`.
fn write_synced_and_rename(temp: &Utf8Path, path: &Utf8Path, contents: &[u8]) -> io::Result<()> {
    let mut file = File::create(temp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)
}

/// Removes a file or directory tree; a missing path isn't an error.
///
/// # Errors
///
/// Fails if the path exists but can't be removed.
pub fn remove_path(path: &Utf8Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// A modification time, as stored in state files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileTime {
    /// Seconds since the Unix epoch.
    pub secs: u64,
    /// Nanoseconds within the second.
    pub nanos: u32,
}

impl FileTime {
    /// Converts `time`; times before the Unix epoch can't be represented.
    pub fn from_system_time(time: SystemTime) -> Option<Self> {
        let since_epoch = time.duration_since(SystemTime::UNIX_EPOCH).ok()?;
        Some(Self {
            secs: since_epoch.as_secs(),
            nanos: since_epoch.subsec_nanos(),
        })
    }

    /// Converts back to a [`SystemTime`].
    pub fn to_system_time(self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::new(self.secs, self.nanos)
    }
}

// -------------------------------------------------------------------------------------------------
// Lock
// -------------------------------------------------------------------------------------------------

/// `risim-out/` can't be locked.
#[derive(Debug, Error)]
pub enum LockError {
    /// Another process holds the lock.
    #[error("{path} is locked by another process")]
    Locked {
        /// The lock file.
        path: Utf8PathBuf,
    },
    /// The lock file can't be created or locked.
    #[error("failed to lock {path}")]
    Io {
        /// The lock file.
        path: Utf8PathBuf,
        /// The cause.
        source: io::Error,
    },
}

/// The exclusive lock on `risim-out/`.
///
/// The lock file holds the PID of its owner while it is locked, and is empty after
/// [`release`](Self::release). Dropping the lock without releasing it leaves the PID behind, so
/// the next owner treats the state as left behind by a crash. The file is never deleted:
/// deleting it would let a process lock the deleted file while another locks a new one.
#[derive(Debug)]
pub struct OutputLock {
    path: Utf8PathBuf,
    file: File,
}

/// Whether the previous owner of `risim-out/` released it properly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousOwner {
    /// The lock file was missing or empty.
    Released,
    /// The lock file still held a PID: the previous owner crashed, and the compile state, the
    /// parse cache and the libraries have been deleted.
    Crashed,
}

impl OutputLock {
    /// Locks `risim-out/`, creating it if needed.
    ///
    /// If the previous owner crashed, the compile state, the parse cache and the compiled
    /// libraries are deleted, so everything is recompiled. Test results are kept.
    ///
    /// # Errors
    ///
    /// Fails if another process holds the lock, or on I/O errors.
    pub fn acquire(layout: &OutputLayout) -> Result<(Self, PreviousOwner), LockError> {
        let path = layout.lock_file();
        let io_error = |source| LockError::Io {
            path: path.clone(),
            source,
        };
        fs::create_dir_all(layout.root()).map_err(io_error)?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io_error)?;
        match file.try_lock() {
            Ok(()) => {},
            Err(TryLockError::WouldBlock) => return Err(LockError::Locked { path }),
            Err(TryLockError::Error(error)) => return Err(io_error(error)),
        }

        let mut previous_owner = Vec::new();
        #[expect(
            clippy::verbose_file_reads,
            reason = "on Windows, the lock blocks reading through another handle"
        )]
        let _length = file.read_to_end(&mut previous_owner).map_err(io_error)?;
        let previous = if previous_owner.is_empty() {
            PreviousOwner::Released
        } else {
            tracing::warn!(
                %path,
                pid = %String::from_utf8_lossy(&previous_owner).trim(),
                "the previous owner didn't release the lock; discarding the compile state"
            );
            for stale in [
                layout.state_file(),
                layout.parse_cache_file(),
                layout.libraries_dir(),
            ] {
                remove_path(&stale).map_err(io_error)?;
            }
            PreviousOwner::Crashed
        };

        // The PID marks the lock as taken; it must reach the disk to detect a crash.
        file.set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| writeln!(file, "{}", process::id()))
            .and_then(|()| file.sync_data())
            .map_err(io_error)?;
        Ok((Self { path, file }, previous))
    }

    /// Marks the lock file as released and unlocks it.
    pub fn release(self) {
        let Self { path, file } = self;
        if let Err(error) = file.set_len(0).and_then(|()| file.sync_data()) {
            tracing::warn!(%path, %error, "failed to clear the lock file");
        }
        if let Err(error) = file.unlock() {
            tracing::warn!(%path, %error, "failed to unlock");
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Compile state
// -------------------------------------------------------------------------------------------------

/// The hash deciding whether a file must be recompiled (see the `compile` module).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompileKey(#[serde(with = "hex32")] pub [u8; 32]);

impl fmt::Debug for CompileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CompileKey({})",
            blake3::Hash::from_bytes(self.0).to_hex()
        )
    }
}

/// Serializes a 32-byte hash as a hex string, with `#[serde(with = "hex32")]`.
pub(crate) mod hex32 {
    use serde::Deserialize as _;
    use serde::Deserializer;
    use serde::Serializer;

    pub(crate) fn serialize<S: Serializer>(
        bytes: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&blake3::Hash::from_bytes(*bytes).to_hex())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        let hex = String::deserialize(deserializer)?;
        let hash = blake3::Hash::from_hex(&hex).map_err(serde::de::Error::custom)?;
        Ok(*hash.as_bytes())
    }
}

/// Identifies a source file in a library: `<library>:<path>`, with the library in lowercase.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileKey(String);

impl FileKey {
    /// The key of `path` in `library`.
    pub fn new(library: &str, path: &Utf8Path) -> Self {
        Self(format!("{}:{path}", library.to_ascii_lowercase()))
    }

    /// The key as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The state of a successfully compiled file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    /// The compile key the file was compiled with.
    pub compile_key: CompileKey,
    /// The warnings and notes of that compile.
    pub diagnostics: Vec<Diagnostic>,
}

/// The compile state of a workspace (`state.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileState {
    /// The simulator the libraries were compiled with.
    pub simulator: Option<SimulatorIdentity>,
    /// The successfully compiled files.
    pub files: BTreeMap<FileKey, FileState>,
}

const STATE_VERSION: u32 = 1;

impl CompileState {
    /// Reads `state.json`; a missing or invalid file gives an empty state.
    pub fn load(layout: &OutputLayout) -> Self {
        read_json(&layout.state_file(), STATE_VERSION).unwrap_or_default()
    }

    /// Writes `state.json`.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be written.
    pub fn save(&self, layout: &OutputLayout) -> io::Result<()> {
        write_json(&layout.state_file(), STATE_VERSION, self)
    }

    /// Prepares the state for compiling with `simulator`.
    ///
    /// If the libraries were compiled with another simulator, or the identity is unknown, the
    /// state and all compiled libraries are discarded.
    ///
    /// # Errors
    ///
    /// Fails if the library directories can't be deleted.
    pub fn use_simulator(
        &mut self,
        simulator: &SimulatorIdentity,
        layout: &OutputLayout,
    ) -> io::Result<()> {
        if self.simulator.as_ref() == Some(simulator) {
            return Ok(());
        }
        if self.simulator.is_some() {
            tracing::info!(path = %simulator.path, "simulator changed; recompiling everything");
        }
        self.files.clear();
        remove_path(&layout.libraries_dir())?;
        self.simulator = Some(simulator.clone());
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------
// Test results
// -------------------------------------------------------------------------------------------------

/// How a test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    /// The test passed.
    Passed,
    /// The test failed, didn't start, or the simulation couldn't be run.
    Failed,
    /// The test was cancelled.
    Cancelled,
}

/// The number of tests with each [`TestOutcome`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TestCounts {
    /// The number of passed tests.
    pub passed: usize,
    /// The number of failed tests.
    pub failed: usize,
    /// The number of cancelled tests.
    pub cancelled: usize,
}

impl TestCounts {
    /// Counts one more test with `outcome`.
    pub const fn record(&mut self, outcome: TestOutcome) {
        match outcome {
            TestOutcome::Passed => self.passed += 1,
            TestOutcome::Failed => self.failed += 1,
            TestOutcome::Cancelled => self.cancelled += 1,
        }
    }
}

impl FromIterator<TestOutcome> for TestCounts {
    fn from_iter<I: IntoIterator<Item = TestOutcome>>(outcomes: I) -> Self {
        let mut counts = Self::default();
        for outcome in outcomes {
            counts.record(outcome);
        }
        counts
    }
}

/// A point in time, in milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// The current time.
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// Converts `time`; times before the Unix epoch become the epoch.
    pub fn from_system_time(time: SystemTime) -> Self {
        let millis = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since_epoch| since_epoch.as_millis());
        Self(u64::try_from(millis).unwrap_or(u64::MAX))
    }
}

/// The last result of a testcase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    /// How the test ended.
    pub outcome: TestOutcome,
    /// When the simulator was started.
    pub started_at: Timestamp,
    /// When the test ended.
    pub finished_at: Timestamp,
    /// The simulator output file (`output.txt`).
    pub output_path: Utf8PathBuf,
}

/// The last result of every testcase (`results.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResults {
    /// The results by testcase name.
    pub results: BTreeMap<String, TestResult>,
}

const RESULTS_VERSION: u32 = 1;

impl TestResults {
    /// Reads `results.json`; a missing or invalid file gives no results.
    pub fn load(layout: &OutputLayout) -> Self {
        read_json(&layout.results_file(), RESULTS_VERSION).unwrap_or_default()
    }

    /// Writes `results.json`.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be written.
    pub fn save(&self, layout: &OutputLayout) -> io::Result<()> {
        write_json(&layout.results_file(), RESULTS_VERSION, self)
    }
}

/// The test results of a workspace, shared by concurrent simulations.
///
/// Every change is written to `results.json` right away.
#[derive(Debug)]
pub struct ResultStore {
    layout: OutputLayout,
    results: Mutex<TestResults>,
}

impl ResultStore {
    /// Loads the results of `layout`.
    pub fn load(layout: &OutputLayout) -> Self {
        Self {
            layout: layout.clone(),
            results: Mutex::new(TestResults::load(layout)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, TestResults> {
        // The results stay consistent even if a holder panicked: every change is one insert or
        // retain.
        lock_unpoisoned(&self.results)
    }

    /// A copy of the current results.
    pub fn snapshot(&self) -> BTreeMap<String, TestResult> {
        self.lock().results.clone()
    }

    /// Records the result of `testcase` and saves the results.
    ///
    /// # Errors
    ///
    /// Fails if `results.json` can't be written; the result is recorded in memory anyway.
    pub fn record(&self, testcase: &str, result: TestResult) -> io::Result<()> {
        let mut results = self.lock();
        results.results.insert(testcase.to_owned(), result);
        // Saving under the lock keeps concurrent writes in order.
        results.save(&self.layout)
    }

    /// Drops the results of testcases that don't exist anymore, and saves the results if
    /// that changed them.
    ///
    /// # Errors
    ///
    /// Fails if `results.json` can't be written.
    pub fn retain_testcases<'name>(
        &self,
        testcases: impl IntoIterator<Item = &'name str>,
    ) -> io::Result<()> {
        let names: BTreeSet<&str> = testcases.into_iter().collect();
        let mut results = self.lock();
        let before = results.results.len();
        results
            .results
            .retain(|name, _| names.contains(name.as_str()));
        if results.results.len() == before {
            return Ok(());
        }
        results.save(&self.layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempRoot;
    use crate::test_support::simulator_identity;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Data {
        items: Vec<u32>,
    }

    #[test]
    fn json_round_trip_checks_version() {
        let temp = TempRoot::new();
        let path = temp.root.join("sub/data.json");
        let data = Data { items: vec![1, 2] };
        write_json(&path, 3, &data).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, r#"{"version":3,"items":[1,2]}"#);
        assert_eq!(read_json::<Data>(&path, 3), Some(data));
        assert_eq!(read_json::<Data>(&path, 4), None);

        fs::write(&path, "{ not json").unwrap();
        assert_eq!(read_json::<Data>(&path, 3), None);
        fs::write(&path, r#"{"version":3,"items":"x"}"#).unwrap();
        assert_eq!(read_json::<Data>(&path, 3), None);
        assert_eq!(read_json::<Data>(&temp.root.join("missing.json"), 3), None);
    }

    #[test]
    fn atomic_write_leaves_no_temporary_files() {
        let temp = TempRoot::new();
        let path = temp.root.join("a.json");
        write_atomic(&path, b"1").unwrap();
        write_atomic(&path, b"2").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"2");
        assert_eq!(fs::read_dir(&temp.root).unwrap().count(), 1);
    }

    #[test]
    fn layout_paths() {
        let layout = OutputLayout::new(Utf8Path::new("/ws"));
        assert_eq!(
            layout.library_dir("My_Lib"),
            "/ws/risim-out/libraries/my_lib"
        );
        let output = layout.compile_output_file("Lib", Utf8Path::new("/ws/src/a.vhd"));
        assert!(output.starts_with("/ws/risim-out/compile_output/lib/"));
        assert_eq!(output.extension(), Some("txt"));
        let stem = output.file_stem().unwrap();
        assert!(stem.starts_with("a_"));
        assert_eq!(stem.len(), "a_".len() + 16);
        assert_ne!(
            output,
            layout.compile_output_file("Lib", Utf8Path::new("/ws/tb/a.vhd"))
        );
    }

    #[test]
    fn test_output_names_are_safe_and_unique() {
        let name = test_output_name("lib.tb.Test 1: a/b", None);
        assert_eq!(
            name,
            format!("lib.tb.Test_1__a_b_{}", short_hash("lib.tb.Test 1: a/b"))
        );
        // Different names with the same safe name get different directories.
        assert_ne!(name, test_output_name("lib.tb.Test_1__a_b", None));
        // Non-ASCII characters become one `_` each.
        assert!(test_output_name("lib.tb.ä", None).starts_with("lib.tb.__"));

        let shortened = test_output_name("lib.tb.long test name", Some(4));
        assert_eq!(
            shortened,
            format!("lib.{}", short_hash("lib.tb.long test name"))
        );
        assert_eq!(test_output_name("x", Some(0)), short_hash("x"));

        let layout = OutputLayout::new(Utf8Path::new("/ws"));
        let dir = layout.test_output_dir("lib.tb.t");
        assert_eq!(dir.parent(), Some(layout.test_output_root().as_path()));
        let paths = TestOutputPaths::new(&layout, "lib.tb.t");
        assert_eq!(paths.output_file, dir.join("output.txt"));
        assert_eq!(paths.results_file, dir.join("vunit_results"));
    }

    #[test]
    fn prepare_recreates_the_output_directory() {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        let paths = TestOutputPaths::new(&layout, "lib.tb.t");
        fs::create_dir_all(&paths.dir).unwrap();
        fs::write(paths.dir.join("stale.txt"), "old").unwrap();
        fs::write(&paths.results_file, "test_suite_done\n").unwrap();
        paths.prepare().unwrap();
        assert!(!paths.dir.join("stale.txt").exists());
        assert_eq!(fs::read(&paths.results_file).unwrap(), b"");
    }

    fn result(outcome: TestOutcome) -> TestResult {
        TestResult {
            outcome,
            started_at: Timestamp(1_000),
            finished_at: Timestamp(2_500),
            output_path: "/ws/risim-out/test_output/x/output.txt".into(),
        }
    }

    #[test]
    fn result_store_saves_every_change() {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        let store = ResultStore::load(&layout);
        assert!(store.snapshot().is_empty());
        store
            .record("lib.tb.a", result(TestOutcome::Passed))
            .unwrap();
        store
            .record("lib.tb.b", result(TestOutcome::Failed))
            .unwrap();
        store
            .record("lib.tb.a", result(TestOutcome::Cancelled))
            .unwrap();

        let text = fs::read_to_string(layout.results_file()).unwrap();
        assert!(
            text.starts_with(
                r#"{"version":1,"results":{"lib.tb.a":{"outcome":"cancelled","started_at":1000,"#
            ),
            "{text}"
        );
        let reloaded = ResultStore::load(&layout);
        assert_eq!(reloaded.snapshot(), store.snapshot());
        assert_eq!(reloaded.snapshot()["lib.tb.b"], result(TestOutcome::Failed));

        reloaded
            .retain_testcases(["lib.tb.b", "lib.tb.new"])
            .unwrap();
        assert_eq!(reloaded.snapshot().keys().collect::<Vec<_>>(), ["lib.tb.b"]);
        assert_eq!(TestResults::load(&layout).results.len(), 1);
    }

    #[test]
    fn lock_excludes_and_detects_crashes() {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        let (lock, previous) = OutputLock::acquire(&layout).unwrap();
        assert_eq!(previous, PreviousOwner::Released);
        assert!(matches!(
            OutputLock::acquire(&layout),
            Err(LockError::Locked { .. })
        ));
        lock.release();
        assert_eq!(fs::read(layout.lock_file()).unwrap(), b"");
        let (released_lock, after_release) = OutputLock::acquire(&layout).unwrap();
        assert_eq!(after_release, PreviousOwner::Released);
        released_lock.release();

        // A dropped lock leaves its PID behind, like a crash.
        let (crashed_lock, _) = OutputLock::acquire(&layout).unwrap();
        write_atomic(&layout.state_file(), b"{}").unwrap();
        fs::create_dir_all(layout.library_dir("lib")).unwrap();
        write_atomic(&layout.results_file(), b"{}").unwrap();
        drop(crashed_lock);

        let (recovered_lock, recovered) = OutputLock::acquire(&layout).unwrap();
        assert_eq!(recovered, PreviousOwner::Crashed);
        assert!(!layout.state_file().exists());
        assert!(!layout.libraries_dir().exists());
        assert!(layout.results_file().exists());
        recovered_lock.release();
    }

    #[test]
    fn compile_state_round_trip() {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        let mut state = CompileState {
            simulator: Some(simulator_identity("1")),
            ..CompileState::default()
        };
        state.files.insert(
            FileKey::new("Lib", Utf8Path::new("/a.vhd")),
            FileState {
                compile_key: CompileKey([7; 32]),
                diagnostics: vec![Diagnostic::warning("w")],
            },
        );
        state.save(&layout).unwrap();
        assert_eq!(CompileState::load(&layout), state);
        let text = fs::read_to_string(layout.state_file()).unwrap();
        assert!(text.contains(r#""lib:/a.vhd""#), "{text}");
    }

    #[test]
    fn simulator_change_discards_state() {
        let temp = TempRoot::new();
        let layout = OutputLayout::new(&temp.root);
        let mut state = CompileState::default();
        state
            .use_simulator(&simulator_identity("1"), &layout)
            .unwrap();
        state.files.insert(
            FileKey::new("lib", Utf8Path::new("/a.vhd")),
            FileState {
                compile_key: CompileKey([0; 32]),
                diagnostics: Vec::new(),
            },
        );
        fs::create_dir_all(layout.library_dir("lib")).unwrap();

        state
            .use_simulator(&simulator_identity("1"), &layout)
            .unwrap();
        assert_eq!(state.files.len(), 1);
        assert!(layout.library_dir("lib").exists());

        state
            .use_simulator(&simulator_identity("2"), &layout)
            .unwrap();
        assert!(state.files.is_empty());
        assert!(!layout.libraries_dir().exists());
        assert_eq!(state.simulator, Some(simulator_identity("2")));
    }
}
