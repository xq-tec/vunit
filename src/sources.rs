// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Collecting, reading and parsing the source files of a project.
//!
//! Replaces the globbing of `ui/common.py` and the caching of `cached.py`:
//!
//! - [`collect`] expands the file patterns of a [`ProjectSpec`] and adds the builtin libraries.
//! - [`SourceCache`] reads, hashes and parses files, reusing earlier results while a file's
//!   modification time and size, or its contents, stay the same.
//! - [`build_project`] combines both into a [`Project`].
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::sync::Arc;
use std::time::SystemTime;

use camino::Utf8Component;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use globset::GlobBuilder;
use globset::GlobMatcher;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

use crate::builtins;
use crate::diagnostics::Diagnostic;
use crate::project::LibraryError;
use crate::project::Project;
use crate::spec::FilePattern;
use crate::spec::LibrarySpec;
use crate::spec::ProjectSpec;
use crate::store;
use crate::store::FileTime;
use crate::vhdl_parser::PARSER_VERSION;
use crate::vhdl_parser::ParseError;
use crate::vhdl_parser::VhdlDesignFile;
use crate::vhdl_standard::VhdlStandard;

/// The directory below the workspace root that holds all generated files.
pub const OUTPUT_DIR: &str = "risim-out";

/// The blake3 hash of a file's raw contents.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// Hashes `contents`.
    pub fn of(contents: &[u8]) -> Self {
        Self(*blake3::hash(contents).as_bytes())
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({self})")
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        let hash = blake3::Hash::from_hex(&hex).map_err(serde::de::Error::custom)?;
        Ok(Self(*hash.as_bytes()))
    }
}

/// The language of a source file, from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileType {
    /// `.vhd`, `.vhdl` or `.vho`.
    Vhdl,
    /// Verilog or `SystemVerilog`, which isn't supported yet.
    Verilog,
}

/// Returns the file type of `path` from its extension, compared case-insensitively.
pub fn file_type_of(path: &Utf8Path) -> Option<FileType> {
    let extension = path.extension()?.to_ascii_lowercase();
    match extension.as_str() {
        "vhd" | "vhdl" | "vho" => Some(FileType::Vhdl),
        "v" | "vp" | "vams" | "vo" | "sv" | "svp" => Some(FileType::Verilog),
        _ => None,
    }
}

/// A library with its expanded source files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedLibrary {
    /// The library name.
    pub name: String,
    /// The standard of the library's files.
    pub vhdl_standard: VhdlStandard,
    /// For a precompiled library, its directory.
    pub external_path: Option<Utf8PathBuf>,
    /// The VHDL files, without duplicates.
    pub files: Vec<Utf8PathBuf>,
}

/// The libraries of a project with their files.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    /// Builtin libraries first, then the libraries of the spec.
    pub libraries: Vec<CollectedLibrary>,
    /// Problems with the spec: invalid libraries and patterns, unsupported features.
    pub config_diagnostics: Vec<Diagnostic>,
    /// Problems with files: unsupported file types.
    pub project_diagnostics: Vec<Diagnostic>,
}

/// Expands the patterns of `spec` relative to `root`, and adds the builtin libraries, whose
/// files are expected in `builtins_dir` (see [`builtins::extract`]).
pub fn collect(root: &Utf8Path, spec: &ProjectSpec, builtins_dir: &Utf8Path) -> Collected {
    let mut collected = Collected::default();
    let excluded = root.join(OUTPUT_DIR);

    let (libraries, include_osvvm) = valid_libraries(spec, &mut collected.config_diagnostics);

    let selection = builtins::select(spec.vhdl_standard, &spec.features, include_osvvm);
    collected.config_diagnostics.extend(selection.diagnostics);
    for library in selection.libraries {
        collected.libraries.push(CollectedLibrary {
            name: library.name.to_owned(),
            vhdl_standard: spec.vhdl_standard,
            external_path: None,
            files: library
                .files
                .iter()
                .map(|file| builtins_dir.join(file.rel_path))
                .collect(),
        });
    }

    for library in libraries {
        match library {
            LibrarySpec::Sources {
                name,
                files,
                vhdl_standard,
            } => {
                let mut library_files = Vec::new();
                let mut seen = FxHashSet::default();
                for pattern in files {
                    let paths =
                        expand_pattern(root, pattern, &excluded, &mut collected.config_diagnostics);
                    for path in paths {
                        match file_type_of(&path) {
                            Some(FileType::Vhdl) => {
                                if seen.insert(path.clone()) {
                                    library_files.push(path);
                                }
                            },
                            Some(FileType::Verilog) => collected.project_diagnostics.push(
                                Diagnostic::warning("Verilog files aren't supported; skipped")
                                    .in_file(path),
                            ),
                            None => collected.project_diagnostics.push(
                                Diagnostic::warning("unknown file type; skipped").in_file(path),
                            ),
                        }
                    }
                }
                collected.libraries.push(CollectedLibrary {
                    name: name.clone(),
                    vhdl_standard: vhdl_standard.unwrap_or(spec.vhdl_standard),
                    external_path: None,
                    files: library_files,
                });
            },
            LibrarySpec::External { name, path } => {
                let path = normalize(&root.join(path));
                if !path.is_dir() {
                    collected.config_diagnostics.push(Diagnostic::error(format!(
                        "the directory {path} of external library '{name}' doesn't exist"
                    )));
                    continue;
                }
                collected.libraries.push(CollectedLibrary {
                    name: name.clone(),
                    vhdl_standard: spec.vhdl_standard,
                    external_path: Some(path),
                    files: Vec::new(),
                });
            },
        }
    }
    collected
}

/// Returns the libraries of `spec` with valid names, and whether the builtin OSVVM library is
/// needed. A user library named `osvvm` replaces the builtin one.
fn valid_libraries<'spec>(
    spec: &'spec ProjectSpec,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<&'spec LibrarySpec>, bool) {
    let mut libraries: Vec<&LibrarySpec> = Vec::new();
    let mut names = FxHashMap::default();
    let mut include_osvvm = true;
    for library in &spec.libraries {
        let name = library.name();
        if let Some(error) = library_name_error(name, &names) {
            diagnostics.push(Diagnostic::error(error));
            continue;
        }
        let lowercase = name.to_ascii_lowercase();
        if lowercase == builtins::OSVVM_LIB {
            include_osvvm = false;
            diagnostics.push(Diagnostic::warning(format!(
                "library '{name}' replaces the builtin OSVVM library"
            )));
        }
        names.insert(lowercase, name.to_owned());
        libraries.push(library);
    }
    (libraries, include_osvvm)
}

/// Returns why `name` can't be the name of a user library, if it can't.
///
/// `defined` maps the lowercase names of the libraries defined so far to their names.
pub(crate) fn library_name_error(
    name: &str,
    defined: &FxHashMap<String, String>,
) -> Option<String> {
    let lowercase = name.to_ascii_lowercase();
    if lowercase == "work" {
        Some(LibraryError::Work.to_string())
    } else if lowercase == builtins::VUNIT_LIB {
        Some(format!(
            "library name '{name}' is reserved for the VUnit library"
        ))
    } else {
        defined.get(&lowercase).map(|existing| {
            LibraryError::Duplicate {
                name: name.to_owned(),
                existing: existing.clone(),
            }
            .to_string()
        })
    }
}

/// Expands one pattern; problems are added to `diagnostics`.
fn expand_pattern(
    root: &Utf8Path,
    pattern: &FilePattern,
    excluded: &Utf8Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Utf8PathBuf> {
    let located = |diagnostic: Diagnostic| match &pattern.location {
        Some(location) => diagnostic.in_file(&location.file).at(Some(location.range)),
        None => diagnostic,
    };
    match expand_glob(root, &pattern.pattern, excluded) {
        Ok(paths) => {
            if paths.is_empty() {
                diagnostics.push(located(Diagnostic::warning(format!(
                    "pattern '{}' doesn't match any file",
                    pattern.pattern
                ))));
            }
            paths
        },
        Err(error) => {
            diagnostics.push(located(Diagnostic::error(format!(
                "invalid pattern '{}': {error}",
                pattern.pattern
            ))));
            Vec::new()
        },
    }
}

fn is_glob_component(component: &str) -> bool {
    component.contains(['*', '?', '[', '{'])
}

/// Splits `pattern` into its base directory, the longest prefix without glob characters
/// resolved against `root`, and the remaining components.
///
/// Without glob components, the base is the file the pattern names.
pub(crate) fn split_pattern<'pattern>(
    root: &Utf8Path,
    pattern: &'pattern str,
) -> (Utf8PathBuf, Vec<&'pattern str>) {
    let pattern_path = Utf8Path::new(pattern);
    let mut base = if pattern_path.is_absolute() {
        Utf8PathBuf::new()
    } else {
        root.to_owned()
    };
    let mut glob_components = Vec::new();
    for component in pattern_path.components() {
        if glob_components.is_empty() && !is_glob_component(component.as_str()) {
            base.push(component);
        } else {
            glob_components.push(component.as_str());
        }
    }
    (normalize(&base), glob_components)
}

/// Returns the files matching `pattern`, sorted, with `excluded` and `.git` directories
/// skipped.
///
/// The walk starts at the longest directory prefix without glob characters. It is recursive
/// only if the pattern contains `**`.
///
/// # Errors
///
/// Fails if the pattern is invalid.
pub fn expand_glob(
    root: &Utf8Path,
    pattern: &str,
    excluded: &Utf8Path,
) -> Result<Vec<Utf8PathBuf>, globset::Error> {
    let (base, glob_components) = split_pattern(root, pattern);
    let matcher: Option<GlobMatcher> = if glob_components.is_empty() {
        None
    } else {
        Some(
            GlobBuilder::new(&glob_components.join("/"))
                .literal_separator(true)
                .build()?
                .compile_matcher(),
        )
    };
    let is_excluded =
        |path: &Utf8Path| path.starts_with(excluded) || path.file_name() == Some(".git");
    if base.ancestors().any(is_excluded) {
        return Ok(Vec::new());
    }
    let Some(matcher) = matcher else {
        return Ok(if base.is_file() {
            vec![base]
        } else {
            Vec::new()
        });
    };
    let recursive = glob_components.contains(&"**");
    let mut walker = walkdir::WalkDir::new(&base)
        .min_depth(1)
        .follow_links(true)
        .sort_by_file_name();
    if !recursive {
        walker = walker.max_depth(glob_components.len());
    }

    let mut paths = Vec::new();
    let entries = walker.into_iter().filter_entry(|entry| {
        Utf8Path::from_path(entry.path()).is_some_and(|path| !is_excluded(path))
    });
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(%error, "skipping unreadable directory entry");
                continue;
            },
        };
        if entry.file_type().is_dir() {
            continue;
        }
        let Some(path) = Utf8Path::from_path(entry.path()) else {
            tracing::warn!(path = %entry.path().display(), "skipping non-UTF-8 path");
            continue;
        };
        let Ok(relative) = path.strip_prefix(&base) else {
            continue;
        };
        if matcher.is_match(relative.as_std_path()) {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

/// Removes `.` components and resolves `..` lexically, without following symlinks.
pub fn normalize(path: &Utf8Path) -> Utf8PathBuf {
    let mut normalized = Utf8PathBuf::new();
    for component in path.components() {
        match component {
            Utf8Component::CurDir => {},
            Utf8Component::ParentDir => {
                let can_pop = matches!(
                    normalized.components().next_back(),
                    Some(Utf8Component::Normal(_))
                );
                if can_pop {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            },
            other => normalized.push(other),
        }
    }
    normalized
}

/// A read, hashed and parsed VHDL file.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    /// The hash of the raw contents.
    pub content_hash: ContentHash,
    /// The parse result.
    pub design_file: Result<Arc<VhdlDesignFile>, ParseError>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    modified: Option<SystemTime>,
    size: u64,
    file: Arc<LoadedFile>,
}

/// A cache entry in `parse_cache.json`.
#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    modified: Option<FileTime>,
    size: u64,
    content_hash: ContentHash,
    parsed: Result<VhdlDesignFile, ParseError>,
}

#[derive(Serialize, Deserialize)]
struct PersistedCache {
    parser_version: u32,
    files: BTreeMap<Utf8PathBuf, PersistedEntry>,
}

const PARSE_CACHE_VERSION: u32 = 1;

/// Parse results of files, kept between loads of a project.
#[derive(Debug, Clone, Default)]
pub struct SourceCache {
    entries: FxHashMap<Utf8PathBuf, CacheEntry>,
}

impl SourceCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads `paths` in parallel, reusing cached results where possible.
    ///
    /// The cache keeps only the given paths afterwards.
    pub fn load(
        &mut self,
        paths: &[Utf8PathBuf],
    ) -> FxHashMap<Utf8PathBuf, io::Result<Arc<LoadedFile>>> {
        let unique: FxHashSet<&Utf8PathBuf> = paths.iter().collect();
        let previous = &self.entries;
        let results: Vec<(Utf8PathBuf, io::Result<CacheEntry>)> = unique
            .into_par_iter()
            .map(|path| (path.clone(), load_file(path, previous.get(path))))
            .collect();

        let mut entries = FxHashMap::default();
        let mut loaded = FxHashMap::default();
        for (path, result) in results {
            match result {
                Ok(entry) => {
                    loaded.insert(path.clone(), Ok(Arc::clone(&entry.file)));
                    entries.insert(path, entry);
                },
                Err(error) => {
                    loaded.insert(path, Err(error));
                },
            }
        }
        self.entries = entries;
        loaded
    }

    /// Reads a cache written by [`save`](Self::save).
    ///
    /// A missing or invalid file, or one written by another parser version, gives an empty
    /// cache.
    pub fn load_persisted(path: &Utf8Path) -> Self {
        let Some(persisted) = store::read_json::<PersistedCache>(path, PARSE_CACHE_VERSION) else {
            return Self::new();
        };
        if persisted.parser_version != PARSER_VERSION {
            tracing::info!(%path, "discarding the parse cache of another parser version");
            return Self::new();
        }
        let entries = persisted
            .files
            .into_iter()
            .map(|(file_path, entry)| {
                let file = LoadedFile {
                    content_hash: entry.content_hash,
                    design_file: entry.parsed.map(Arc::new),
                };
                let entry = CacheEntry {
                    modified: entry.modified.map(FileTime::to_system_time),
                    size: entry.size,
                    file: Arc::new(file),
                };
                (file_path, entry)
            })
            .collect();
        Self { entries }
    }

    /// Writes the cache to `path`.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be written.
    pub fn save(&self, path: &Utf8Path) -> io::Result<()> {
        let files = self
            .entries
            .iter()
            .map(|(file_path, entry)| {
                let persisted = PersistedEntry {
                    modified: entry.modified.and_then(FileTime::from_system_time),
                    size: entry.size,
                    content_hash: entry.file.content_hash,
                    parsed: entry
                        .file
                        .design_file
                        .as_ref()
                        .map(|design_file| VhdlDesignFile::clone(design_file))
                        .map_err(Clone::clone),
                };
                (file_path.clone(), persisted)
            })
            .collect();
        let persisted = PersistedCache {
            parser_version: PARSER_VERSION,
            files,
        };
        store::write_json(path, PARSE_CACHE_VERSION, &persisted)
    }
}

fn load_file(path: &Utf8Path, cached: Option<&CacheEntry>) -> io::Result<CacheEntry> {
    let metadata = fs::metadata(path)?;
    let modified = metadata.modified().ok();
    if let Some(cached) = cached
        && cached.modified.is_some()
        && cached.modified == modified
        && cached.size == metadata.len()
    {
        return Ok(cached.clone());
    }

    let contents = fs::read(path)?;
    let content_hash = ContentHash::of(&contents);
    let file = match cached {
        Some(cached) if cached.file.content_hash == content_hash => Arc::clone(&cached.file),
        _ => Arc::new(LoadedFile {
            content_hash,
            design_file: VhdlDesignFile::parse(&contents).map(Arc::new),
        }),
    };
    Ok(CacheEntry {
        modified,
        size: contents.len() as u64,
        file,
    })
}

/// A project built from a spec, with the problems found on the way.
#[derive(Debug, Clone)]
pub struct LoadedProject {
    /// The project.
    pub project: Project,
    /// Problems with the spec.
    pub config_diagnostics: Vec<Diagnostic>,
    /// Problems with files: unsupported types, read and parse failures, duplicate units.
    pub project_diagnostics: Vec<Diagnostic>,
}

/// Collects, loads and parses the sources of `spec` and builds the project.
pub fn build_project(
    root: &Utf8Path,
    spec: &ProjectSpec,
    builtins_dir: &Utf8Path,
    cache: &mut SourceCache,
) -> LoadedProject {
    let collected = collect(root, spec, builtins_dir);
    let paths: Vec<Utf8PathBuf> = collected
        .libraries
        .iter()
        .flat_map(|library| library.files.iter().cloned())
        .collect();
    let loaded = cache.load(&paths);

    let mut project = Project::new();
    let mut config_diagnostics = collected.config_diagnostics;
    let mut project_diagnostics = collected.project_diagnostics;
    let mut reported: FxHashSet<&Utf8Path> = FxHashSet::default();
    for library in &collected.libraries {
        let library_id = match project.add_library(
            &library.name,
            library.vhdl_standard,
            library.external_path.clone(),
        ) {
            Ok(id) => id,
            Err(error) => {
                config_diagnostics.push(Diagnostic::error(error.to_string()));
                continue;
            },
        };
        for path in &library.files {
            let Some(result) = loaded.get(path) else {
                continue;
            };
            // A file in two libraries is read once, so report its problems once.
            let first_time = reported.insert(path);
            match result {
                Ok(file) => {
                    let design_file = match &file.design_file {
                        Ok(design_file) => Some(Arc::clone(design_file)),
                        Err(error) => {
                            if first_time {
                                project_diagnostics.push(
                                    Diagnostic::warning(format!("failed to parse: {error}"))
                                        .in_file(path),
                                );
                            }
                            None
                        },
                    };
                    project.add_source_file(library_id, path, None, file.content_hash, design_file);
                },
                Err(error) => {
                    if first_time {
                        project_diagnostics.push(
                            Diagnostic::error(format!("failed to read: {error}")).in_file(path),
                        );
                    }
                },
            }
        }
    }
    project_diagnostics.extend(project.diagnostics().iter().cloned());
    LoadedProject {
        project,
        config_diagnostics,
        project_diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Severity;

    struct Workspace {
        _temp: tempfile::TempDir,
        root: Utf8PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = Utf8Path::from_path(temp.path()).unwrap().to_owned();
            Self { _temp: temp, root }
        }

        fn write(&self, rel_path: &str, contents: &str) -> Utf8PathBuf {
            let path = self.root.join(rel_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        fn glob(&self, pattern: &str) -> Vec<String> {
            expand_glob(&self.root, pattern, &self.root.join(OUTPUT_DIR))
                .unwrap()
                .into_iter()
                .map(|path| path.strip_prefix(&self.root).unwrap().to_string())
                .collect()
        }
    }

    #[test]
    fn glob_syntax() {
        let workspace = Workspace::new();
        for path in [
            "src/a.vhd",
            "src/b.vhd",
            "src/.hidden.vhd",
            "src/sub/c.vhd",
            "src/sub/deep/d.vhd",
            "src/x1.vhd",
            "tb/tb_a.vhd",
        ] {
            workspace.write(path, "");
        }
        assert_eq!(
            workspace.glob("src/*.vhd"),
            ["src/.hidden.vhd", "src/a.vhd", "src/b.vhd", "src/x1.vhd"]
        );
        assert_eq!(
            workspace.glob("src/**/*.vhd"),
            [
                "src/.hidden.vhd",
                "src/a.vhd",
                "src/b.vhd",
                "src/sub/c.vhd",
                "src/sub/deep/d.vhd",
                "src/x1.vhd",
            ]
        );
        assert_eq!(workspace.glob("src/?.vhd"), ["src/a.vhd", "src/b.vhd"]);
        assert_eq!(workspace.glob("src/[ab].vhd"), ["src/a.vhd", "src/b.vhd"]);
        assert_eq!(workspace.glob("src/*/c.vhd"), ["src/sub/c.vhd"]);
        assert_eq!(workspace.glob("*/tb_*.vhd"), ["tb/tb_a.vhd"]);
        assert_eq!(workspace.glob("src/a.vhd"), ["src/a.vhd"]);
        assert_eq!(workspace.glob("./src/../tb/tb_a.vhd"), ["tb/tb_a.vhd"]);
        assert!(workspace.glob("src/missing.vhd").is_empty());
        assert!(workspace.glob("missing/*.vhd").is_empty());
    }

    #[test]
    fn glob_is_case_sensitive() {
        let workspace = Workspace::new();
        workspace.write("src/A.VHD", "");
        assert!(workspace.glob("src/*.vhd").is_empty());
        assert_eq!(workspace.glob("src/*.VHD"), ["src/A.VHD"]);
    }

    #[test]
    fn glob_skips_output_and_git_directories() {
        let workspace = Workspace::new();
        workspace.write("a.vhd", "");
        workspace.write("risim-out/builtins/x/b.vhd", "");
        workspace.write(".git/c.vhd", "");
        workspace.write("sub/.git/d.vhd", "");
        assert_eq!(workspace.glob("**/*.vhd"), ["a.vhd"]);
        assert!(workspace.glob("risim-out/**/*.vhd").is_empty());
    }

    #[test]
    fn absolute_patterns() {
        let workspace = Workspace::new();
        workspace.write("src/a.vhd", "");
        let other_root = Utf8Path::new("/nonexistent-root");
        let pattern = format!("{}/src/*.vhd", workspace.root);
        let paths = expand_glob(other_root, &pattern, &other_root.join(OUTPUT_DIR)).unwrap();
        assert_eq!(paths, [workspace.root.join("src/a.vhd")]);
    }

    #[test]
    fn invalid_glob_is_an_error() {
        let workspace = Workspace::new();
        let excluded = workspace.root.join(OUTPUT_DIR);
        expand_glob(&workspace.root, "src/[a.vhd", &excluded).unwrap_err();
    }

    #[test]
    fn normalize_resolves_dots() {
        assert_eq!(normalize(Utf8Path::new("/a/./b/../c")), "/a/c");
        assert_eq!(normalize(Utf8Path::new("a/../../b")), "../b");
        assert_eq!(normalize(Utf8Path::new("/../a")), "/a");
    }

    #[test]
    fn file_types() {
        let file_type = |name: &str| file_type_of(Utf8Path::new(name));
        assert_eq!(file_type("file.vhd"), Some(FileType::Vhdl));
        assert_eq!(file_type("file.VHDL"), Some(FileType::Vhdl));
        assert_eq!(file_type("file.vho"), Some(FileType::Vhdl));
        assert_eq!(file_type("file.sv"), Some(FileType::Verilog));
        assert_eq!(file_type("file.svp"), Some(FileType::Verilog));
        assert_eq!(file_type("file.v"), Some(FileType::Verilog));
        assert_eq!(file_type("file.vams"), Some(FileType::Verilog));
        assert_eq!(file_type("file.foo"), None);
        assert_eq!(file_type("file"), None);
    }

    #[test]
    fn content_hash_round_trips_through_json() {
        let hash = ContentHash::of(b"entity foo is end;");
        let json = serde_json::to_string(&hash).unwrap();
        assert_eq!(json.len(), 66);
        assert_eq!(serde_json::from_str::<ContentHash>(&json).unwrap(), hash);
    }

    #[test]
    fn collect_reports_patterns_and_file_types() {
        let workspace = Workspace::new();
        let a_vhd = workspace.write("src/a.vhd", "");
        workspace.write("src/b.sv", "");
        workspace.write("src/readme.txt", "");
        let mut spec = ProjectSpec::new();
        spec.add_library("lib", ["src/*", "src/a.vhd", "nothing/*.vhd"]);
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));

        let names: Vec<_> = collected
            .libraries
            .iter()
            .map(|library| library.name.as_str())
            .collect();
        assert_eq!(names, ["vunit_lib", "osvvm", "lib"]);
        assert_eq!(collected.libraries[2].files, [a_vhd]);
        assert!(collected.libraries[0].files[0].starts_with("/builtins/vhdl/"));

        assert_eq!(collected.config_diagnostics.len(), 1);
        assert_eq!(collected.config_diagnostics[0].severity, Severity::Warning);
        assert!(
            collected.config_diagnostics[0]
                .message
                .contains("nothing/*.vhd")
        );
        assert_eq!(collected.project_diagnostics.len(), 2);
    }

    #[test]
    fn collect_validates_library_names() {
        let workspace = Workspace::new();
        let mut spec = ProjectSpec::new();
        spec.add_library("work", Vec::<String>::new())
            .add_library("VUnit_Lib", Vec::<String>::new())
            .add_library("Lib", Vec::<String>::new())
            .add_library("LIB", Vec::<String>::new())
            .add_library("OSVVM", Vec::<String>::new());
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));
        let names: Vec<_> = collected
            .libraries
            .iter()
            .map(|library| library.name.as_str())
            .collect();
        assert_eq!(names, ["vunit_lib", "Lib", "OSVVM"]);
        let severities: Vec<_> = collected
            .config_diagnostics
            .iter()
            .map(|diagnostic| diagnostic.severity)
            .collect();
        assert_eq!(
            severities,
            [
                Severity::Error,
                Severity::Error,
                Severity::Error,
                Severity::Warning
            ]
        );
    }

    #[test]
    fn cache_reuses_unchanged_files() {
        let workspace = Workspace::new();
        let path = workspace.write("a.vhd", "entity a is end;");
        let mut cache = SourceCache::new();
        let first = cache.load(std::slice::from_ref(&path));
        let first = Arc::clone(first[&path].as_ref().unwrap());
        let second = cache.load(std::slice::from_ref(&path));
        assert!(Arc::ptr_eq(&first, second[&path].as_ref().unwrap()));

        workspace.write("a.vhd", "entity b is end;");
        // Force a different modification time, in case the file system is coarse.
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        let third = cache.load(std::slice::from_ref(&path));
        let third = third[&path].as_ref().unwrap();
        assert_ne!(third.content_hash, first.content_hash);
        let design_file = third.design_file.as_ref().unwrap();
        assert_eq!(design_file.entities[0].identifier, "b");
    }

    #[test]
    fn cache_persists_parse_results() {
        let workspace = Workspace::new();
        let good = workspace.write("good.vhd", "entity good is end;");
        let bad = workspace.write("bad.vhd", "entity bad is\n port (x : in bit;\nend;");
        let cache_file = workspace.root.join("parse_cache.json");
        let paths = [good, bad.clone()];
        let mut cache = SourceCache::new();
        let first = cache.load(&paths);
        cache.save(&cache_file).unwrap();

        let mut restored = SourceCache::load_persisted(&cache_file);
        assert_eq!(restored.entries.len(), 2);
        let second = restored.load(&paths);
        for path in &paths {
            let (first, second) = (
                first[path].as_ref().unwrap(),
                second[path].as_ref().unwrap(),
            );
            assert_eq!(first.content_hash, second.content_hash);
            assert_eq!(first.design_file, second.design_file);
        }
        second[&bad]
            .as_ref()
            .unwrap()
            .design_file
            .as_ref()
            .unwrap_err();

        // A cache of another parser version is discarded.
        let text = fs::read_to_string(&cache_file).unwrap().replace(
            &format!("\"parser_version\":{PARSER_VERSION}"),
            "\"parser_version\":0",
        );
        fs::write(&cache_file, text).unwrap();
        assert!(SourceCache::load_persisted(&cache_file).entries.is_empty());
        assert!(
            SourceCache::load_persisted(&workspace.root.join("missing"))
                .entries
                .is_empty()
        );
    }

    #[test]
    fn cache_reports_missing_files() {
        let mut cache = SourceCache::new();
        let path = Utf8PathBuf::from("/nonexistent/a.vhd");
        let loaded = cache.load(std::slice::from_ref(&path));
        loaded[&path].as_ref().unwrap_err();
    }

    #[test]
    fn build_project_reports_parse_failures_once() {
        let workspace = Workspace::new();
        workspace.write(
            "src/bad.vhd",
            "entity foo is\n port (foo : in bit;\nend entity;\n",
        );
        workspace.write("src/good.vhd", "entity good is end entity;\n");
        let builtins_dir = builtins::extract(&workspace.root.join("risim-out/builtins")).unwrap();
        let mut spec = ProjectSpec::new();
        spec.add_library("a", ["src/*.vhd"])
            .add_library("b", ["src/*.vhd"]);
        let loaded = build_project(
            &workspace.root,
            &spec,
            &builtins_dir,
            &mut SourceCache::new(),
        );

        assert_eq!(loaded.config_diagnostics, []);
        let messages: Vec<_> = loaded
            .project_diagnostics
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("bad.vhd") && messages[0].contains("failed to parse"));

        let project = &loaded.project;
        let library_a = project.find_library("A").unwrap();
        let good = project.library(library_a).primary_unit("good").unwrap();
        assert_eq!(
            project.file(good.file).path,
            workspace.root.join("src/good.vhd")
        );
        // Builtins are parsed too.
        let vunit_lib = project.find_library("vunit_lib").unwrap();
        assert!(project.library(vunit_lib).primary_unit("run_pkg").is_some());
    }
}
