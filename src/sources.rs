// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Collecting, reading and parsing the source files of a project.
//!
//! Replaces the globbing of `ui/common.py`, the file reading of `ostools.py` and the caching of
//! `cached.py`:
//!
//! - [`collect`] expands the file patterns of a [`ProjectSpec`], sorts the files by their
//!   extension ([`FileKind`]), and adds the builtin libraries.
//! - [`SourceCache`] reads, hashes and parses VHDL files, reusing earlier results while a file's
//!   modification time and size, or its contents, stay the same; its [`SvScanCache`] scans
//!   SystemVerilog and Verilog files with the `verilog_parser`, with their libraries' options.
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
use serde::Serialize;

use crate::builtins;
use crate::diagnostics::Diagnostic;
use crate::project::LibraryNames;
use crate::project::Project;
use crate::spec::CompilationUnit;
use crate::spec::Define;
use crate::spec::FilePattern;
use crate::spec::IncludeDir;
use crate::spec::KeywordVersion;
use crate::spec::LibrarySpec;
use crate::spec::Options;
use crate::spec::PatternLocation;
use crate::spec::ProjectSpec;
use crate::store;
use crate::store::FileTime;
use crate::verilog_parser::Defines;
use crate::verilog_parser::KeywordSet;
use crate::vhdl_parser::PARSER_VERSION;
use crate::vhdl_parser::ParseError;
use crate::vhdl_parser::VhdlDesignFile;
use crate::vhdl_standard::VhdlStandard;

mod sv_scan;

pub use self::sv_scan::SvScanCache;

/// The directory below the workspace root that holds all generated files.
pub const OUTPUT_DIR: &str = "risim-out";

/// The blake3 hash of a file's raw contents.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(#[serde(with = "store::hex32")] pub [u8; 32]);

impl ContentHash {
    /// Hashes `contents`.
    pub fn of(contents: &[u8]) -> Self {
        Self(*blake3::hash(contents).as_bytes())
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&blake3::Hash::from_bytes(self.0).to_hex())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({self})")
    }
}

/// What a file is, by its extension: VUnit's copy of hydrolyze's table (`hydrolyze::FileKind`),
/// which riSim's tests check against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// A VHDL source: `.vhd`, `.vhdl`, or `.vho`.
    Vhdl,
    /// A SystemVerilog source, `.sv`.
    SystemVerilog,
    /// A Verilog source, `.v` or `.vo`.
    Verilog,
    /// An include file, `.svh` or `.vh`, which is read only where a source includes it.
    Header,
    /// A file of the SystemVerilog family that isn't compiled, for the reason.
    Skipped(SkipReason),
}

/// Why a [`FileKind::Skipped`] file isn't compiled.
///
/// It displays as an adjective of "files": "encrypted", "Verilog-AMS".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// An encrypted source, `.svp` or `.vp`.
    Encrypted,
    /// A Verilog-AMS source, `.vams`.
    VerilogAms,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Encrypted => "encrypted",
            Self::VerilogAms => "Verilog-AMS",
        })
    }
}

impl FileKind {
    /// The extensions and their kinds, in lowercase and without the dot.
    pub const EXTENSIONS: [(&'static str, Self); 11] = [
        ("vhd", Self::Vhdl),
        ("vhdl", Self::Vhdl),
        ("vho", Self::Vhdl),
        ("sv", Self::SystemVerilog),
        ("v", Self::Verilog),
        ("vo", Self::Verilog),
        ("svh", Self::Header),
        ("vh", Self::Header),
        ("svp", Self::Skipped(SkipReason::Encrypted)),
        ("vp", Self::Skipped(SkipReason::Encrypted)),
        ("vams", Self::Skipped(SkipReason::VerilogAms)),
    ];

    /// Returns the kind that the extension of `path` stands for, compared case-insensitively, or
    /// `None` for any other extension and for a path without one.
    pub fn from_path(path: &Utf8Path) -> Option<Self> {
        let extension = path.extension()?;
        Self::EXTENSIONS
            .iter()
            .find(|(candidate, _)| extension.eq_ignore_ascii_case(candidate))
            .map(|&(_, kind)| kind)
    }

    /// Whether the file is a SystemVerilog or Verilog source.
    pub const fn is_sv(self) -> bool {
        matches!(self, Self::SystemVerilog | Self::Verilog)
    }

    /// The keywords that a SystemVerilog or Verilog source starts with unless the `keywords`
    /// option says otherwise, as a version of [`KEYWORD_VERSIONS`](crate::spec::KEYWORD_VERSIONS).
    pub const fn default_keywords(self) -> Option<&'static str> {
        match self {
            Self::SystemVerilog => Some("1800-2023"),
            Self::Verilog => Some("1364-2005"),
            Self::Vhdl | Self::Header | Self::Skipped(_) => None,
        }
    }

    /// The keywords that the scanner reads a SystemVerilog or Verilog source with: those of
    /// `keywords`, else those of the kind.
    fn keyword_set(self, keywords: Option<KeywordVersion>) -> KeywordSet {
        let version = keywords
            .map(KeywordVersion::text)
            .or_else(|| self.default_keywords())
            .unwrap_or_default();
        if version.starts_with("1364-") {
            KeywordSet::Verilog1364
        } else {
            KeywordSet::Sv
        }
    }
}

/// A collected source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedFile {
    /// The absolute path.
    pub path: Utf8PathBuf,
    /// The kind, a VHDL, SystemVerilog, or Verilog source.
    pub kind: FileKind,
}

/// A library with its expanded source files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedLibrary {
    /// The library name.
    pub name: String,
    /// The standard of the library's VHDL files.
    pub vhdl_standard: VhdlStandard,
    /// For a precompiled library, its directory.
    pub external_path: Option<Utf8PathBuf>,
    /// The source files, without duplicates, in the order of their patterns: VHDL,
    /// SystemVerilog, and Verilog files, without include files and skipped files.
    pub files: Vec<CollectedFile>,
    /// The effective options of the library's files: [`Options::for_builtins`] of the project's
    /// for the builtin libraries (also for files added to `vunit_lib`),
    /// [`Options::for_library`] for user libraries, with the include directories resolved
    /// against the workspace root, and the default for precompiled ones.
    /// A user library with SystemVerilog or Verilog files searches VUnit's include directory
    /// (`verilog/include` of the builtins) before its own include directories, as VUnit does.
    pub options: Options,
}

impl CollectedLibrary {
    /// Whether the library has SystemVerilog or Verilog files.
    pub fn has_sv(&self) -> bool {
        self.files.iter().any(|file| file.kind.is_sv())
    }
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

/// The directory of VUnit's SystemVerilog include files below the builtins directory.
pub const BUILTIN_INCLUDE_DIR: &str = "verilog/include";

/// Expands the patterns of `spec` relative to `root`, and adds the builtin libraries, whose
/// files are expected in `builtins_dir` (see [`builtins::extract`]).
///
/// Include files (`.svh`, `.vh`) that a pattern matches are left out without a warning;
/// skipped files (`.svp`, `.vp`, `.vams`) and files of unknown extensions get one.
/// The builtins depend on the user libraries, since a project with SystemVerilog or Verilog
/// files gets VUnit's SystemVerilog package, so they are selected after them, and still come
/// first.
pub fn collect(root: &Utf8Path, spec: &ProjectSpec, builtins_dir: &Utf8Path) -> Collected {
    let mut collected = Collected::default();
    let excluded = root.join(OUTPUT_DIR);

    let (libraries, include_osvvm) = valid_libraries(spec, &mut collected.config_diagnostics);

    let mut user_libraries = Vec::new();
    // The files of a user library `vunit_lib`, which go into the builtin one.
    let mut vunit_lib_files = Vec::new();
    // The include directories that don't exist, so that each gets one warning.
    let mut missing_dirs = FxHashSet::default();
    for library in libraries {
        match library {
            LibrarySpec::Sources {
                name,
                files,
                vhdl_standard,
                options,
            } => {
                let library_files = collect_files(root, files, &excluded, &mut collected);
                if name.eq_ignore_ascii_case(builtins::VUNIT_LIB) {
                    // Like `ui.library("vunit_lib").add_source_files(…)` in VUnit.
                    if vhdl_standard.is_some_and(|standard| standard != spec.vhdl_standard) {
                        collected.config_diagnostics.push(Diagnostic::error(format!(
                            "library '{name}' must use the project's VHDL standard"
                        )));
                    } else {
                        vunit_lib_files.extend(library_files);
                    }
                    continue;
                }
                let mut collected_library = CollectedLibrary {
                    name: name.clone(),
                    vhdl_standard: vhdl_standard.unwrap_or(spec.vhdl_standard),
                    external_path: None,
                    files: library_files,
                    options: library_options(
                        root,
                        &spec.options,
                        options,
                        &mut missing_dirs,
                        &mut collected.config_diagnostics,
                    ),
                };
                if collected_library.has_sv() {
                    // `add_verilog_include_dir`: VUnit's directory comes first.
                    let builtin = IncludeDir::new(builtins_dir.join(BUILTIN_INCLUDE_DIR));
                    collected_library
                        .options
                        .include_dirs
                        .get_or_insert_default()
                        .insert(0, builtin);
                }
                user_libraries.push(collected_library);
            },
            LibrarySpec::External { name, path } => {
                let path = normalize(&root.join(path));
                if !path.is_dir() {
                    collected.config_diagnostics.push(Diagnostic::error(format!(
                        "the directory {path} of external library '{name}' doesn't exist"
                    )));
                    continue;
                }
                user_libraries.push(CollectedLibrary {
                    name: name.clone(),
                    vhdl_standard: spec.vhdl_standard,
                    external_path: Some(path),
                    files: Vec::new(),
                    options: Options::default(),
                });
            },
        }
    }

    let has_sv = user_libraries.iter().any(CollectedLibrary::has_sv)
        || vunit_lib_files.iter().any(|file| file.kind.is_sv());
    let selection = builtins::select(spec.vhdl_standard, &spec.features, include_osvvm, has_sv);
    collected.config_diagnostics.extend(selection.diagnostics);
    let builtin_options = spec.options.for_builtins();
    for library in selection.libraries {
        let mut files: Vec<CollectedFile> = library
            .files
            .iter()
            .filter_map(|file| {
                let path = builtins_dir.join(file.rel_path);
                let kind = FileKind::from_path(&path)?;
                Some(CollectedFile { path, kind })
            })
            .collect();
        if library.name == builtins::VUNIT_LIB {
            files.append(&mut vunit_lib_files);
        }
        collected.libraries.push(CollectedLibrary {
            name: library.name.to_owned(),
            vhdl_standard: spec.vhdl_standard,
            external_path: None,
            files,
            options: builtin_options.clone(),
        });
    }
    collected.libraries.extend(user_libraries);
    collected
}

/// Expands the patterns `files` of a library, and returns its source files without duplicates.
///
/// Include files are left out silently; skipped files and files of unknown extensions get a
/// warning in the project diagnostics of `collected`, and problems with patterns go to its
/// configuration diagnostics.
fn collect_files(
    root: &Utf8Path,
    files: &[FilePattern],
    excluded: &Utf8Path,
    collected: &mut Collected,
) -> Vec<CollectedFile> {
    let mut library_files = Vec::new();
    let mut seen = FxHashSet::default();
    for pattern in files {
        let paths = expand_pattern(root, pattern, excluded, &mut collected.config_diagnostics);
        for path in paths {
            match FileKind::from_path(&path) {
                Some(kind @ (FileKind::Vhdl | FileKind::SystemVerilog | FileKind::Verilog)) => {
                    if seen.insert(path.clone()) {
                        library_files.push(CollectedFile { path, kind });
                    }
                },
                Some(FileKind::Header) => {},
                Some(FileKind::Skipped(reason)) => collected.project_diagnostics.push(
                    Diagnostic::warning(format!("{reason} files aren't supported; skipped"))
                        .in_file(path),
                ),
                None => collected
                    .project_diagnostics
                    .push(Diagnostic::warning("unknown file type; skipped").in_file(path)),
            }
        }
    }
    library_files
}

/// Returns the effective options of a user library whose own options are `library`, with
/// `project` as the project's ([`Options::for_library`]).
///
/// The include directories are resolved against `root`; a directory that doesn't exist stays,
/// since it may be created later, and gives a warning in `diagnostics` unless it is in
/// `missing_dirs` already, as it is for the project's directories after the first library.
fn library_options(
    root: &Utf8Path,
    project: &Options,
    library: &Options,
    missing_dirs: &mut FxHashSet<(Utf8PathBuf, Option<PatternLocation>)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Options {
    let mut options = project.for_library(library);
    for directory in options.include_dirs.iter_mut().flatten() {
        directory.path = normalize(&root.join(&directory.path));
        if !directory.path.is_dir()
            && missing_dirs.insert((directory.path.clone(), directory.location.clone()))
        {
            let diagnostic = Diagnostic::warning(format!(
                "include directory {} doesn't exist",
                directory.path
            ));
            diagnostics.push(match &directory.location {
                Some(location) => diagnostic.in_file(&location.file).at(Some(location.range)),
                None => diagnostic,
            });
        }
    }
    options
}

/// Returns the libraries of `spec` with valid names, and whether the builtin OSVVM library is
/// needed. A user library named `osvvm` replaces the builtin one; the files of a library named
/// `vunit_lib` are added to the builtin one, which can't be replaced by a precompiled library.
fn valid_libraries<'spec>(
    spec: &'spec ProjectSpec,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<&'spec LibrarySpec>, bool) {
    let mut libraries: Vec<&LibrarySpec> = Vec::new();
    let mut names = LibraryNames::new();
    let mut include_osvvm = true;
    for library in &spec.libraries {
        let name = library.name();
        if matches!(library, LibrarySpec::External { .. })
            && name.eq_ignore_ascii_case(builtins::VUNIT_LIB)
        {
            diagnostics.push(Diagnostic::error(format!(
                "library name '{name}' is reserved for the VUnit library"
            )));
            continue;
        }
        if let Err(error) = names.insert(name) {
            diagnostics.push(Diagnostic::error(error.to_string()));
            continue;
        }
        if name.eq_ignore_ascii_case(builtins::OSVVM_LIB) {
            include_osvvm = false;
            diagnostics.push(Diagnostic::warning(format!(
                "library '{name}' replaces the builtin OSVVM library"
            )));
        }
        libraries.push(library);
    }
    (libraries, include_osvvm)
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
    // The output directories of nested workspaces hold extracted builtins too.
    let is_excluded = |path: &Utf8Path| {
        path.starts_with(excluded)
            || path.file_name() == Some(".git")
            || (path.file_name() == Some(OUTPUT_DIR) && path.join(".lock").is_file())
    };
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

/// A cache entry in `parse_cache.json`, as it is read.
#[derive(Deserialize)]
struct PersistedEntry {
    modified: Option<FileTime>,
    size: u64,
    content_hash: ContentHash,
    parsed: Result<VhdlDesignFile, ParseError>,
}

/// A cache entry in `parse_cache.json`, as it is written. Serializes like [`PersistedEntry`]
/// without copying the parse result.
#[derive(Serialize)]
struct PersistedEntryRef<'cache> {
    modified: Option<FileTime>,
    size: u64,
    content_hash: ContentHash,
    parsed: Result<&'cache VhdlDesignFile, &'cache ParseError>,
}

/// `parse_cache.json`, with `Files` mapping each path to its entry.
#[derive(Serialize, Deserialize)]
struct PersistedCache<Files> {
    parser_version: u32,
    files: Files,
}

const PARSE_CACHE_VERSION: u32 = 1;

/// Parse results of files, kept between loads of a project.
///
/// The VHDL parse results can be persisted ([`save`](Self::save)); the scans of SystemVerilog
/// and Verilog files are kept in memory only.
#[derive(Debug, Clone, Default)]
pub struct SourceCache {
    entries: FxHashMap<Utf8PathBuf, CacheEntry>,
    sv: SvScanCache,
}

impl SourceCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the VHDL files `paths` in parallel, reusing cached results where possible.
    ///
    /// The cache keeps only the given paths afterwards.
    /// A file that can't be read keeps its entry, so that its parse result is reused if it comes
    /// back with the same contents, like the builtins extracted again into a recreated
    /// `risim-out/`.
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
                    if let Some(previous) = previous.get(&path) {
                        entries.insert(path.clone(), previous.clone());
                    }
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
        let Some(persisted) = store::read_json::<
            PersistedCache<BTreeMap<Utf8PathBuf, PersistedEntry>>,
        >(path, PARSE_CACHE_VERSION) else {
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
        Self {
            entries,
            sv: SvScanCache::new(),
        }
    }

    /// Writes the cache to `path`.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be written.
    pub fn save(&self, path: &Utf8Path) -> io::Result<()> {
        let files: BTreeMap<_, _> = self
            .entries
            .iter()
            .map(|(file_path, entry)| {
                let persisted = PersistedEntryRef {
                    modified: entry.modified.and_then(FileTime::from_system_time),
                    size: entry.size,
                    content_hash: entry.file.content_hash,
                    parsed: entry.file.design_file.as_deref(),
                };
                (file_path, persisted)
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

/// Collects, loads, parses, and scans the sources of `spec` and builds the project.
///
/// The SystemVerilog and Verilog files of a library are scanned with its options: those of a
/// library whose files form one compilation unit in order, each with the macros that the
/// previous one leaves, the others each on their own.
pub fn build_project(
    root: &Utf8Path,
    spec: &ProjectSpec,
    builtins_dir: &Utf8Path,
    cache: &mut SourceCache,
) -> LoadedProject {
    let collected = collect(root, spec, builtins_dir);
    let vhdl_paths: Vec<Utf8PathBuf> = collected
        .libraries
        .iter()
        .flat_map(|library| &library.files)
        .filter(|file| file.kind == FileKind::Vhdl)
        .map(|file| file.path.clone())
        .collect();
    let loaded = cache.load(&vhdl_paths);
    let mut scanned = scan_sv_files(&collected.libraries, &mut cache.sv);

    let mut project = Project::new();
    let mut config_diagnostics = collected.config_diagnostics;
    let mut project_diagnostics = collected.project_diagnostics;
    let mut reported: FxHashSet<&Utf8Path> = FxHashSet::default();
    for (index, library) in collected.libraries.iter().enumerate() {
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
        project.set_library_options(library_id, library.options.clone());
        for CollectedFile { path, kind } in &library.files {
            // A file in two libraries is read once, so report its problems once.
            let first_time = reported.insert(path);
            let read_error = match kind {
                FileKind::Vhdl => match loaded.get(path) {
                    Some(Ok(file)) => {
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
                        project.add_source_file(
                            library_id,
                            path,
                            None,
                            file.content_hash,
                            design_file,
                        );
                        None
                    },
                    Some(Err(error)) => Some(error.to_string()),
                    None => None,
                },
                _ => match scanned.remove(&(index, path.as_path())) {
                    Some(Ok(file)) => {
                        project.add_sv_file(
                            library_id,
                            path,
                            *kind,
                            file.content_hash,
                            Some(file.design_file),
                            file.include_hashes,
                        );
                        None
                    },
                    Some(Err(error)) => Some(error.to_string()),
                    None => None,
                },
            };
            if let Some(error) = read_error
                && first_time
            {
                project_diagnostics
                    .push(Diagnostic::error(format!("failed to read: {error}")).in_file(path));
            }
        }
    }
    project.finish();
    project_diagnostics.extend(project.diagnostics().iter().cloned());
    LoadedProject {
        project,
        config_diagnostics,
        project_diagnostics,
    }
}

/// Scans the SystemVerilog and Verilog files of the source libraries of `libraries` with
/// `cache`, and returns the results by the index of the library and the path.
fn scan_sv_files<'a>(
    libraries: &'a [CollectedLibrary],
    cache: &mut SvScanCache,
) -> FxHashMap<(usize, &'a Utf8Path), io::Result<sv_scan::ScannedFile>> {
    let mut chains = Vec::new();
    // The library and the paths of each chain.
    let mut chain_files: Vec<(usize, Vec<&Utf8Path>)> = Vec::new();
    for (index, library) in libraries.iter().enumerate() {
        if library.external_path.is_some() || !library.has_sv() {
            continue;
        }
        let options = &library.options;
        let template = sv_scan::ScanChain {
            include_dirs: options
                .include_dirs
                .iter()
                .flatten()
                .map(|directory| directory.path.clone())
                .collect(),
            keywords: options.keywords,
            defines: initial_defines(options),
            carry_defines: options.compilation_unit == Some(CompilationUnit::Library),
            files: Vec::new(),
        };
        let files = library.files.iter().filter(|file| file.kind.is_sv());
        if template.carry_defines {
            let paths = files.clone().map(|file| file.path.as_path()).collect();
            chains.push(sv_scan::ScanChain {
                files: files.map(|file| (file.path.clone(), file.kind)).collect(),
                ..template
            });
            chain_files.push((index, paths));
        } else {
            for file in files {
                chains.push(sv_scan::ScanChain {
                    files: vec![(file.path.clone(), file.kind)],
                    ..template.clone()
                });
                chain_files.push((index, vec![file.path.as_path()]));
            }
        }
    }
    let results = cache.scan(&chains);
    chain_files
        .into_iter()
        .zip(results)
        .flat_map(|((index, paths), results)| {
            paths
                .into_iter()
                .zip(results)
                .map(move |(path, result)| ((index, path), result))
        })
        .collect()
}

/// The macros of the `defines` of `options`, which every compilation unit starts with.
fn initial_defines(options: &Options) -> Defines {
    let mut defines = Defines::new();
    for (name, define) in &options.defines {
        if let Define::Defined { formals, text } = define {
            defines.define(name, formals.as_deref(), text);
        }
    }
    defines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Position;
    use crate::diagnostics::Range;
    use crate::diagnostics::Severity;
    use crate::spec::Encoding;
    use crate::test_support::TempRoot as Workspace;

    fn glob(workspace: &Workspace, pattern: &str) -> Vec<String> {
        let root = &workspace.root;
        expand_glob(root, pattern, &root.join(OUTPUT_DIR))
            .unwrap()
            .into_iter()
            .map(|path| path.strip_prefix(root).unwrap().to_string())
            .collect()
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
            glob(&workspace, "src/*.vhd"),
            ["src/.hidden.vhd", "src/a.vhd", "src/b.vhd", "src/x1.vhd"]
        );
        assert_eq!(
            glob(&workspace, "src/**/*.vhd"),
            [
                "src/.hidden.vhd",
                "src/a.vhd",
                "src/b.vhd",
                "src/sub/c.vhd",
                "src/sub/deep/d.vhd",
                "src/x1.vhd",
            ]
        );
        assert_eq!(glob(&workspace, "src/?.vhd"), ["src/a.vhd", "src/b.vhd"]);
        assert_eq!(glob(&workspace, "src/[ab].vhd"), ["src/a.vhd", "src/b.vhd"]);
        assert_eq!(glob(&workspace, "src/*/c.vhd"), ["src/sub/c.vhd"]);
        assert_eq!(glob(&workspace, "*/tb_*.vhd"), ["tb/tb_a.vhd"]);
        assert_eq!(glob(&workspace, "src/a.vhd"), ["src/a.vhd"]);
        assert_eq!(glob(&workspace, "./src/../tb/tb_a.vhd"), ["tb/tb_a.vhd"]);
        assert!(glob(&workspace, "src/missing.vhd").is_empty());
        assert!(glob(&workspace, "missing/*.vhd").is_empty());
    }

    #[test]
    fn glob_is_case_sensitive() {
        let workspace = Workspace::new();
        workspace.write("src/A.VHD", "");
        assert!(glob(&workspace, "src/*.vhd").is_empty());
        assert_eq!(glob(&workspace, "src/*.VHD"), ["src/A.VHD"]);
    }

    #[test]
    fn glob_skips_output_and_git_directories() {
        let workspace = Workspace::new();
        workspace.write("a.vhd", "");
        workspace.write("risim-out/builtins/x/b.vhd", "");
        workspace.write(".git/c.vhd", "");
        workspace.write("sub/.git/d.vhd", "");
        // The output directory of a nested workspace, recognized by its lock file.
        workspace.write("nested/risim-out/.lock", "");
        workspace.write("nested/risim-out/builtins/x/e.vhd", "");
        // Without a lock file, a directory named like it is an ordinary directory.
        workspace.write("other/risim-out/f.vhd", "");
        assert_eq!(
            glob(&workspace, "**/*.vhd"),
            ["a.vhd", "other/risim-out/f.vhd"]
        );
        assert!(glob(&workspace, "risim-out/**/*.vhd").is_empty());
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
    fn file_kinds() {
        let kind = |name: &str| FileKind::from_path(Utf8Path::new(name));
        assert_eq!(kind("file.vhd"), Some(FileKind::Vhdl));
        assert_eq!(kind("file.VHDL"), Some(FileKind::Vhdl));
        assert_eq!(kind("file.vho"), Some(FileKind::Vhdl));
        assert_eq!(kind("file.sv"), Some(FileKind::SystemVerilog));
        assert_eq!(kind("file.v"), Some(FileKind::Verilog));
        assert_eq!(kind("file.Vo"), Some(FileKind::Verilog));
        assert_eq!(kind("file.svh"), Some(FileKind::Header));
        assert_eq!(kind("file.VH"), Some(FileKind::Header));
        assert_eq!(
            kind("file.svp"),
            Some(FileKind::Skipped(SkipReason::Encrypted))
        );
        assert_eq!(
            kind("file.vp"),
            Some(FileKind::Skipped(SkipReason::Encrypted))
        );
        assert_eq!(
            kind("file.vams"),
            Some(FileKind::Skipped(SkipReason::VerilogAms))
        );
        assert_eq!(kind("file.foo"), None);
        assert_eq!(kind("file"), None);
        assert_eq!(kind(".sv"), None);

        assert!(FileKind::SystemVerilog.is_sv() && FileKind::Verilog.is_sv());
        assert!(!FileKind::Vhdl.is_sv() && !FileKind::Header.is_sv());
        assert_eq!(
            FileKind::SystemVerilog.default_keywords(),
            Some("1800-2023")
        );
        assert_eq!(FileKind::Verilog.default_keywords(), Some("1364-2005"));
        assert_eq!(FileKind::Header.default_keywords(), None);
        for sv_kind in [FileKind::SystemVerilog, FileKind::Verilog] {
            let version = sv_kind.default_keywords().unwrap();
            assert!(crate::spec::KEYWORD_VERSIONS.contains(&version));
        }
        let keywords = |version: &str| Some(version.parse().unwrap());
        assert_eq!(FileKind::Verilog.keyword_set(None), KeywordSet::Verilog1364);
        assert_eq!(FileKind::SystemVerilog.keyword_set(None), KeywordSet::Sv);
        assert_eq!(
            FileKind::SystemVerilog.keyword_set(keywords("1364-2001")),
            KeywordSet::Verilog1364
        );
        assert_eq!(
            FileKind::Verilog.keyword_set(keywords("1800-2017")),
            KeywordSet::Sv
        );
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
        assert_eq!(
            collected.libraries[2].files,
            [CollectedFile {
                path: a_vhd,
                kind: FileKind::Vhdl
            }]
        );
        assert!(
            collected.libraries[0].files[0]
                .path
                .starts_with("/builtins/vhdl/")
        );

        assert_eq!(collected.config_diagnostics.len(), 1);
        assert_eq!(collected.config_diagnostics[0].severity, Severity::Warning);
        assert!(
            collected.config_diagnostics[0]
                .message
                .contains("nothing/*.vhd")
        );
        assert_eq!(collected.project_diagnostics.len(), 1);
    }

    #[test]
    fn collect_sv_files() {
        let workspace = Workspace::new();
        let paths: Vec<Utf8PathBuf> = [
            "src/a.sv",
            "src/b.v",
            "src/c.vo",
            "src/d.vhd",
            "src/e.svh",
            "src/f.vh",
            "src/g.svp",
            "src/h.vp",
            "src/i.vams",
        ]
        .into_iter()
        .map(|path| workspace.write(path, ""))
        .collect();
        let mut spec = ProjectSpec::new();
        spec.add_library("lib", ["src/*"]);
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));

        assert_eq!(collected.config_diagnostics, []);
        let lib = &collected.libraries[2];
        let files: Vec<_> = lib
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.kind))
            .collect();
        assert_eq!(
            files,
            [
                (paths[0].as_str(), FileKind::SystemVerilog),
                (paths[1].as_str(), FileKind::Verilog),
                (paths[2].as_str(), FileKind::Verilog),
                (paths[3].as_str(), FileKind::Vhdl),
            ]
        );
        // Include files are left out silently, skipped files with their reason.
        assert_eq!(
            collected.project_diagnostics,
            [
                Diagnostic::warning("encrypted files aren't supported; skipped").in_file(&paths[6]),
                Diagnostic::warning("encrypted files aren't supported; skipped").in_file(&paths[7]),
                Diagnostic::warning("Verilog-AMS files aren't supported; skipped")
                    .in_file(&paths[8]),
            ]
        );
    }

    #[test]
    fn collect_adds_the_verilog_builtins_with_sv_files_only() {
        let workspace = Workspace::new();
        workspace.write("rtl/a.sv", "");
        workspace.write("vhdl/b.vhd", "");
        let mut spec = ProjectSpec::new();
        spec.options.include_dirs = Some(vec![IncludeDir::new("inc")]);
        spec.add_library("rtl", ["rtl/*"])
            .add_library("vhdl", ["vhdl/*"]);
        let builtins = Utf8Path::new("/builtins");
        let collected = collect(&workspace.root, &spec, builtins);

        let vunit_lib = &collected.libraries[0];
        let last = vunit_lib.files.last().unwrap();
        assert_eq!(
            (last.path.as_str(), last.kind),
            ("/builtins/verilog/vunit_pkg.sv", FileKind::SystemVerilog)
        );
        // The builtins take no include directory, and only SV libraries take VUnit's.
        assert_eq!(vunit_lib.options.include_dirs, None);
        let include_dirs = |index: usize| {
            collected.libraries[index]
                .options
                .include_dirs
                .iter()
                .flatten()
                .map(|directory| directory.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            include_dirs(2),
            [builtins.join("verilog/include"), workspace.root.join("inc")]
        );
        assert_eq!(include_dirs(3), [workspace.root.join("inc")]);

        // Without SV files, there is neither.
        let mut vhdl_spec = ProjectSpec::new();
        vhdl_spec.add_library("vhdl", ["vhdl/*"]);
        let vhdl_only = collect(&workspace.root, &vhdl_spec, builtins);
        assert!(
            vhdl_only
                .libraries
                .iter()
                .flat_map(|library| &library.files)
                .all(|file| file.kind == FileKind::Vhdl)
        );
        assert_eq!(vhdl_only.libraries[2].options.include_dirs, None);

        // An SV file added to `vunit_lib` counts too.
        let mut vunit_lib_spec = ProjectSpec::new();
        vunit_lib_spec.add_library("vunit_lib", ["rtl/*"]);
        let in_vunit_lib = collect(&workspace.root, &vunit_lib_spec, builtins);
        let files: Vec<&str> = in_vunit_lib.libraries[0]
            .files
            .iter()
            .rev()
            .take(2)
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            files,
            [
                workspace.root.join("rtl/a.sv").as_str(),
                "/builtins/verilog/vunit_pkg.sv"
            ]
        );
    }

    #[test]
    fn collect_adds_user_files_to_vunit_lib() {
        let workspace = Workspace::new();
        let tb = workspace.write("test/tb_queue.vhd", "");
        let mut spec = ProjectSpec::new();
        spec.add_library("VUnit_Lib", ["test/*.vhd"]);
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));
        assert_eq!(collected.config_diagnostics, []);
        let names: Vec<_> = collected
            .libraries
            .iter()
            .map(|library| library.name.as_str())
            .collect();
        assert_eq!(names, ["vunit_lib", "osvvm"]);
        assert_eq!(
            collected.libraries[0].files.last(),
            Some(&CollectedFile {
                path: tb,
                kind: FileKind::Vhdl
            })
        );
    }

    #[test]
    fn collect_gives_the_projects_relaxed_to_every_library() {
        let workspace = Workspace::new();
        let mut spec = ProjectSpec::new();
        spec.add_library("lib", Vec::<String>::new())
            .add_external_library("ext", ".");
        spec.options.relaxed = true;
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));
        let relaxed: Vec<_> = collected
            .libraries
            .iter()
            .map(|library| (library.name.as_str(), library.options.relaxed))
            .collect();
        assert_eq!(
            relaxed,
            [
                ("vunit_lib", true),
                ("osvvm", true),
                ("lib", true),
                ("ext", false)
            ]
        );
    }

    #[test]
    fn collect_merges_the_options_of_libraries() {
        let workspace = Workspace::new();
        workspace.write("inc/defs.svh", "");
        let mut spec = ProjectSpec::new();
        spec.options = Options {
            relaxed: true,
            include_dirs: Some(vec![IncludeDir::new("project")]),
            defines: BTreeMap::from([
                ("WIDTH".to_owned(), Define::text("8")),
                ("DEBUG".to_owned(), Define::text("")),
            ]),
            encoding: Some(Encoding::Latin1),
            ..Options::default()
        };
        spec.add_library("lib", Vec::<String>::new())
            .add_library("plain", Vec::<String>::new());
        let LibrarySpec::Sources { options, .. } = &mut spec.libraries[0] else {
            unreachable!();
        };
        *options = Options {
            include_dirs: Some(vec![IncludeDir::new("./inc/../inc")]),
            defines: BTreeMap::from([("DEBUG".to_owned(), Define::Removed)]),
            sv_libraries: Some(vec!["plain".to_owned()]),
            ..Options::default()
        };
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));

        let options_of = |name: &str| {
            &collected
                .libraries
                .iter()
                .find(|library| library.name == name)
                .unwrap()
                .options
        };
        let relaxed = Options {
            relaxed: true,
            ..Options::default()
        };
        assert_eq!(*options_of("vunit_lib"), relaxed);
        assert_eq!(*options_of("osvvm"), relaxed);
        assert_eq!(
            *options_of("lib"),
            Options {
                relaxed: true,
                include_dirs: Some(vec![IncludeDir::new(workspace.root.join("inc"))]),
                defines: BTreeMap::from([("WIDTH".to_owned(), Define::text("8"))]),
                encoding: Some(Encoding::Latin1),
                sv_libraries: Some(vec!["plain".to_owned()]),
                ..Options::default()
            }
        );
        // The project's include directories are resolved for libraries without their own.
        assert_eq!(
            options_of("plain").include_dirs,
            Some(vec![IncludeDir::new(workspace.root.join("project"))])
        );
    }

    #[test]
    fn collect_warns_once_about_a_missing_include_dir() {
        let workspace = Workspace::new();
        let location = PatternLocation {
            file: workspace.root.join("risim-config.toml"),
            range: Range {
                start: Position::new(3, 17),
                end: Position::new(3, 25),
            },
        };
        let mut spec = ProjectSpec::new();
        spec.options.include_dirs = Some(vec![IncludeDir {
            path: "missing".into(),
            location: Some(location.clone()),
        }]);
        spec.add_library("a", Vec::<String>::new())
            .add_library("b", Vec::<String>::new());
        let collected = collect(&workspace.root, &spec, Utf8Path::new("/builtins"));

        let missing = workspace.root.join("missing");
        assert_eq!(
            collected.config_diagnostics,
            [
                Diagnostic::warning(format!("include directory {missing} doesn't exist"))
                    .in_file(&location.file)
                    .at(Some(location.range))
            ]
        );
        // The directory stays, since it may be created later.
        for library in &collected.libraries[2..] {
            let directories = library.options.include_dirs.as_deref().unwrap();
            assert_eq!(directories[0].path, missing);
        }
    }

    #[test]
    fn collect_validates_library_names() {
        let workspace = Workspace::new();
        let mut spec = ProjectSpec::new();
        spec.add_library("work", Vec::<String>::new())
            .add_external_library("VUnit_Lib", ".")
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
    fn cache_keeps_the_parse_result_of_a_file_that_comes_back() {
        let workspace = Workspace::new();
        let path = workspace.write("a.vhd", "entity a is end;");
        let mut cache = SourceCache::new();
        let first = cache.load(std::slice::from_ref(&path));
        let first = Arc::clone(first[&path].as_ref().unwrap());

        fs::remove_file(&path).unwrap();
        cache.load(std::slice::from_ref(&path))[&path]
            .as_ref()
            .unwrap_err();
        // Written again with the same contents, but another modification time.
        workspace.write("a.vhd", "entity a is end;");
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        let again = cache.load(std::slice::from_ref(&path));
        assert!(Arc::ptr_eq(&first, again[&path].as_ref().unwrap()));

        // A path that isn't loaded any more is dropped.
        cache.load(&[]);
        assert!(cache.entries.is_empty());
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

    #[test]
    fn build_project_scans_sv_files_with_their_library_options() {
        let workspace = Workspace::new();
        workspace.write("inc/defs.svh", "`define WIDTH 8\n");
        workspace.write("pkgs/types_pkg.sv", "package types_pkg;\nendpackage\n");
        workspace.write(
            "rtl/a.sv",
            "`include \"defs.svh\"\n`define CARRIED carried\nmodule `NAME;\n  import \
             types_pkg::*;\nendmodule\n",
        );
        workspace.write("rtl/b.sv", "module `CARRIED;\nendmodule\n");
        workspace.write(
            "tb/tb_sv.sv",
            "`include \"vunit_defines.svh\"\nmodule tb_sv;\n  `TEST_SUITE begin\n  \
             end\nendmodule\n",
        );
        let builtins_dir = builtins::extract(&workspace.root.join("risim-out/builtins")).unwrap();
        let mut spec = ProjectSpec::new();
        spec.options.defines = BTreeMap::from([("NAME".to_owned(), Define::text("from_option"))]);
        spec.add_library("pkgs", ["pkgs/*.sv"])
            .add_library("rtl", ["rtl/*.sv"])
            .add_library("tb", ["tb/*.sv"]);
        let LibrarySpec::Sources { options, .. } = &mut spec.libraries[1] else {
            unreachable!();
        };
        *options = Options {
            include_dirs: Some(vec![IncludeDir::new("inc")]),
            compilation_unit: Some(CompilationUnit::Library),
            ..Options::default()
        };
        let mut cache = SourceCache::new();
        let loaded = build_project(&workspace.root, &spec, &builtins_dir, &mut cache);
        assert_eq!(loaded.config_diagnostics, []);
        assert_eq!(loaded.project_diagnostics, []);

        let project = &loaded.project;
        let library = |name: &str| project.library(project.find_library(name).unwrap());
        let rtl = library("rtl");
        // The options' macro, and in `library` mode, the macro of the previous file.
        assert!(rtl.sv_definition("from_option").is_some());
        assert!(rtl.sv_definition("carried").is_some());
        let rtl_dependencies: Vec<&str> = rtl
            .sv_dependencies()
            .map(|dependency| project.library(dependency).name.as_str())
            .collect();
        assert_eq!(rtl_dependencies, ["pkgs", "vunit_lib"]);
        // `vunit_pkg.sv` is in `vunit_lib`, and VUnit's macros are found.
        assert!(library("vunit_lib").sv_package("vunit_pkg").is_some());
        let tb = library("tb").sv_definition("tb_sv").unwrap().1;
        let parameters = &project.file(tb).sv_file.as_ref().unwrap().definitions[0].parameters;
        assert_eq!(parameters, &["runner_cfg"]);
        let args = project.analyze_args(project.find_library("rtl").unwrap());
        assert_eq!(
            args,
            [
                "--compilation-unit=library".to_owned(),
                format!("--include-dir={}", builtins_dir.join("verilog/include")),
                format!("--include-dir={}", workspace.root.join("inc")),
                "--define=NAME=from_option".to_owned(),
                "--sv-library=pkgs".to_owned(),
                "--sv-library=vunit_lib".to_owned(),
            ]
        );

        // A second build reuses the scans.
        let again = build_project(&workspace.root, &spec, &builtins_dir, &mut cache);
        let path = workspace.root.join("rtl/b.sv");
        let sv_file = |built: &LoadedProject| {
            let model = &built.project;
            let rtl_id = model.find_library("rtl").unwrap();
            let file = model.library(rtl_id).file(&path).unwrap();
            Arc::clone(model.file(file).sv_file.as_ref().unwrap())
        };
        assert!(Arc::ptr_eq(&sv_file(&loaded), &sv_file(&again)));
    }
}
