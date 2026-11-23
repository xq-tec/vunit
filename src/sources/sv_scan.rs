// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Scanning SystemVerilog and Verilog files, with the results kept in memory between loads.
//!
//! Replaces the parse cache of `parsing/verilog/parser.py` (`_lookup_parse_cache`): a scan is
//! reused if the file, the options it was scanned with, and every file that its includes tried
//! to read are unchanged.
//! The include files count by what the search found: a file that a search tried and didn't
//! find, such as one earlier in the search path, invalidates the scan when it appears.
//! The results aren't persisted, unlike the VHDL parse results.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fs;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::time::SystemTime;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use super::ContentHash;
use super::FileKind;
use crate::spec::KeywordVersion;
use crate::verilog_parser;
use crate::verilog_parser::Defines;
use crate::verilog_parser::KeywordSet;
use crate::verilog_parser::ScanInput;
use crate::verilog_parser::SvDesignFile;

/// The files of one library that are scanned in order, with the options they share.
#[derive(Debug, Clone)]
pub struct ScanChain {
    /// The include directories, after the directory of the including file.
    pub include_dirs: Vec<Utf8PathBuf>,
    /// The keywords of the `keywords` option, else those of each file's kind.
    pub keywords: Option<KeywordVersion>,
    /// The macros that the first file starts with: those of the `defines` option.
    pub defines: Defines,
    /// Whether each file starts with the macros at the end of the previous one, as in a library
    /// whose files form one compilation unit; else each starts with [`defines`](Self::defines).
    pub carry_defines: bool,
    /// The files, SystemVerilog or Verilog sources, in order.
    pub files: Vec<(Utf8PathBuf, FileKind)>,
}

/// A scanned SystemVerilog or Verilog file.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// The hash of the raw contents.
    pub content_hash: ContentHash,
    /// What the scan found.
    pub design_file: Arc<SvDesignFile>,
    /// The hashes of the contents of the files that the
    /// [`includes`](SvDesignFile::includes) of the scan found, in the same order; `None` for an
    /// include that wasn't found.
    pub include_hashes: Vec<Option<ContentHash>>,
}

/// When a file was last modified, and its size: while both stay the same, so do its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    size: u64,
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
            size: metadata.len(),
        }
    }

    /// Whether the file with this stamp is unchanged since it had `earlier`.
    fn is_unchanged_since(self, earlier: Self) -> bool {
        self.modified.is_some() && self == earlier
    }
}

/// A scan, with what it depends on.
#[derive(Debug)]
struct ScanEntry {
    path: Utf8PathBuf,
    content_hash: ContentHash,
    include_dirs: Vec<Utf8PathBuf>,
    defines: Defines,
    keywords: KeywordSet,
    /// The files that the includes tried to read, in order, with the hashes of their contents,
    /// or `None` for those that couldn't be read.
    reads: Vec<(Utf8PathBuf, Option<ContentHash>)>,
    design_file: Arc<SvDesignFile>,
    /// The macros at the end of the file.
    end_defines: Defines,
}

impl ScanEntry {
    /// The hashes of the files that the includes of the scan found, in their order.
    fn include_hashes(&self) -> Vec<Option<ContentHash>> {
        self.design_file
            .includes
            .iter()
            .map(|include| {
                let path = include.path.as_ref()?;
                self.reads
                    .iter()
                    .find(|(read, _)| read == path)
                    .and_then(|&(_, hash)| hash)
            })
            .collect()
    }
}

/// Scan results of SystemVerilog and Verilog files, kept in memory between loads of a project.
#[derive(Debug, Clone, Default)]
pub struct SvScanCache {
    /// The scans of each file, one per set of options it was scanned with.
    scans: FxHashMap<Utf8PathBuf, Vec<Arc<ScanEntry>>>,
    /// The stamps and hashes of the files read: the sources and the include files.
    hashes: FxHashMap<Utf8PathBuf, (Stamp, ContentHash)>,
}

impl SvScanCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Scans the files of `chains`, reusing earlier scans where possible, and returns the
    /// results of each chain's files, in order.
    ///
    /// Chains are scanned in parallel, the files of a chain in order.
    /// The cache keeps only the scans of this call afterwards.
    pub fn scan(&mut self, chains: &[ScanChain]) -> Vec<Vec<io::Result<ScannedFile>>> {
        let files = Files {
            known: &self.hashes,
            seen: Mutex::default(),
        };
        let previous = &self.scans;
        let results: Vec<Vec<io::Result<Arc<ScanEntry>>>> = chains
            .par_iter()
            .map(|chain| scan_chain(chain, previous, &files))
            .collect();

        let mut scans: FxHashMap<Utf8PathBuf, Vec<Arc<ScanEntry>>> = FxHashMap::default();
        for entry in results.iter().flatten().flatten() {
            let entries = scans.entry(entry.path.clone()).or_default();
            if !entries.iter().any(|other| Arc::ptr_eq(other, entry)) {
                entries.push(Arc::clone(entry));
            }
        }
        let hashes = files.into_hashes();
        self.scans = scans;
        self.hashes = hashes;
        results
            .into_iter()
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|result| {
                        result.map(|entry| ScannedFile {
                            content_hash: entry.content_hash,
                            design_file: Arc::clone(&entry.design_file),
                            include_hashes: entry.include_hashes(),
                        })
                    })
                    .collect()
            })
            .collect()
    }
}

/// Scans the files of `chain` in order, carrying the macros if the chain says so.
fn scan_chain(
    chain: &ScanChain,
    previous: &FxHashMap<Utf8PathBuf, Vec<Arc<ScanEntry>>>,
    files: &Files<'_>,
) -> Vec<io::Result<Arc<ScanEntry>>> {
    let mut defines = chain.defines.clone();
    let mut results = Vec::with_capacity(chain.files.len());
    for (path, kind) in &chain.files {
        let input = FileInput {
            path,
            include_dirs: &chain.include_dirs,
            defines: &defines,
            keywords: kind.keyword_set(chain.keywords),
        };
        let result = scan_file(&input, previous.get(path).map_or(&[], Vec::as_slice), files);
        if chain.carry_defines
            && let Ok(entry) = &result
        {
            defines = entry.end_defines.clone();
        }
        results.push(result);
    }
    results
}

/// A file to scan, with its options.
struct FileInput<'a> {
    path: &'a Utf8Path,
    include_dirs: &'a [Utf8PathBuf],
    defines: &'a Defines,
    keywords: KeywordSet,
}

/// Scans a file, or returns an earlier scan of it that is still valid.
fn scan_file(
    input: &FileInput<'_>,
    previous: &[Arc<ScanEntry>],
    files: &Files<'_>,
) -> io::Result<Arc<ScanEntry>> {
    let hash = files.hash(input.path)?;
    let reusable = previous.iter().find(|entry| {
        entry.content_hash == hash
            && entry.keywords == input.keywords
            && entry.include_dirs == input.include_dirs
            && entry.defines == *input.defines
            && entry
                .reads
                .iter()
                .all(|(path, read)| files.hash(path).ok() == *read)
    });
    if let Some(entry) = reusable {
        tracing::trace!(path = %input.path, "reusing the scan of a SystemVerilog file");
        return Ok(Arc::clone(entry));
    }

    let (contents, content_hash) = files.read(input.path)?;
    let mut reads = Vec::new();
    let (design_file, end_defines) = verilog_parser::scan(
        &ScanInput {
            path: input.path,
            contents: &contents,
            include_dirs: input.include_dirs,
            defines: input.defines,
            keywords: input.keywords,
        },
        |candidate| {
            let result = files.read(candidate);
            reads.push((
                candidate.to_owned(),
                result.as_ref().ok().map(|&(_, read_hash)| read_hash),
            ));
            result.map(|(included, _)| included)
        },
    );
    Ok(Arc::new(ScanEntry {
        path: input.path.to_owned(),
        content_hash,
        include_dirs: input.include_dirs.to_vec(),
        defines: input.defines.clone(),
        keywords: input.keywords,
        reads,
        design_file: Arc::new(design_file),
        end_defines,
    }))
}

/// The files of one [`SvScanCache::scan`], each stamped and hashed once.
struct Files<'cache> {
    /// The stamps and hashes of the previous scan.
    known: &'cache FxHashMap<Utf8PathBuf, (Stamp, ContentHash)>,
    /// The files of this scan, with their contents once they were read.
    seen: Mutex<FxHashMap<Utf8PathBuf, Seen>>,
}

/// A file of [`Files`].
#[derive(Clone)]
struct Seen {
    stamp: Stamp,
    hash: ContentHash,
    contents: Option<Arc<[u8]>>,
}

impl Files<'_> {
    fn lock(&self) -> MutexGuard<'_, FxHashMap<Utf8PathBuf, Seen>> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Returns the hash of the file at `path`, which is read only if it changed since the
    /// previous scan.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be read, or isn't a file.
    fn hash(&self, path: &Utf8Path) -> io::Result<ContentHash> {
        if let Some(seen) = self.lock().get(path) {
            return Ok(seen.hash);
        }
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
        }
        let stamp = Stamp::of(&metadata);
        if let Some(&(known, hash)) = self.known.get(path)
            && stamp.is_unchanged_since(known)
        {
            let seen = Seen {
                stamp,
                hash,
                contents: None,
            };
            self.lock().insert(path.to_owned(), seen);
            return Ok(hash);
        }
        self.read(path).map(|(_, hash)| hash)
    }

    /// Returns the contents of the file at `path`, and their hash.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be read.
    fn read(&self, path: &Utf8Path) -> io::Result<(Arc<[u8]>, ContentHash)> {
        if let Some(Seen {
            hash,
            contents: Some(contents),
            ..
        }) = self.lock().get(path)
        {
            return Ok((Arc::clone(contents), *hash));
        }
        let metadata = fs::metadata(path)?;
        let contents: Arc<[u8]> = fs::read(path)?.into();
        let hash = ContentHash::of(&contents);
        let seen = Seen {
            stamp: Stamp {
                modified: metadata.modified().ok(),
                size: contents.len() as u64,
            },
            hash,
            contents: Some(Arc::clone(&contents)),
        };
        self.lock().insert(path.to_owned(), seen);
        Ok((contents, hash))
    }

    /// Returns the stamps and hashes of the files seen.
    fn into_hashes(self) -> FxHashMap<Utf8PathBuf, (Stamp, ContentHash)> {
        self.seen
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .into_iter()
            .map(|(path, seen)| (path, (seen.stamp, seen.hash)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    //! The cache tests of `test_verilog_parser.py`, and the macros of a library that is one
    //! compilation unit.

    use super::*;
    use crate::test_support::TempRoot;

    /// Gives the file at `path` another modification time, in case the file system is coarse.
    fn touch_back(path: &Utf8Path) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
    }

    fn chain(files: &[&Utf8Path], include_dirs: &[Utf8PathBuf], defines: &Defines) -> ScanChain {
        ScanChain {
            include_dirs: include_dirs.to_vec(),
            keywords: None,
            defines: defines.clone(),
            carry_defines: false,
            files: files
                .iter()
                .map(|path| ((*path).to_owned(), FileKind::SystemVerilog))
                .collect(),
        }
    }

    /// Scans `path` alone with `cache`.
    fn scan(
        cache: &mut SvScanCache,
        path: &Utf8Path,
        include_dirs: &[Utf8PathBuf],
        defines: &Defines,
    ) -> Arc<SvDesignFile> {
        let mut results = cache.scan(&[chain(&[path], include_dirs, defines)]);
        let result = results.pop().unwrap().pop().unwrap();
        result.unwrap().design_file
    }

    fn definition_names(design_file: &SvDesignFile) -> Vec<&str> {
        design_file
            .definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect()
    }

    #[test]
    fn result_is_cached() {
        let temp = TempRoot::new();
        let path = temp.write(
            "file_name.sv",
            "`include \"missing.sv\"\nmodule name;\n  true1 instance_name1();\n  true2 \
             instance_name2(.foo(bar));\n  true3 #(.param(1)) \
             instance_name3(.foo(bar));\nendmodule\n",
        );
        let mut cache = SvScanCache::new();
        let first = scan(&mut cache, &path, &[], &Defines::new());
        assert_eq!(first.instances, ["true1", "true2", "true3"]);
        let second = scan(&mut cache, &path, &[], &Defines::new());
        assert!(Arc::ptr_eq(&first, &second));

        let third = scan(&mut SvScanCache::new(), &path, &[], &Defines::new());
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(first, third);
    }

    #[test]
    fn cached_scan_updated_by_changing_file() {
        let temp = TempRoot::new();
        let path = temp.write("file_name.sv", "module mod1;\nendmodule\n");
        let mut cache = SvScanCache::new();
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &[], &Defines::new())),
            ["mod1"]
        );
        temp.write("file_name.sv", "module mod2;\nendmodule\n");
        touch_back(&path);
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &[], &Defines::new())),
            ["mod2"]
        );
    }

    #[test]
    fn cached_scan_updated_by_includes() {
        let temp = TempRoot::new();
        let include = temp.write("inc/include.svh", "\nmodule mod;\nendmodule;\n");
        let path = temp.write("src/file_name.sv", "`include \"include.svh\"\n");
        let include_dirs = [temp.root.join("inc")];
        let mut cache = SvScanCache::new();
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &include_dirs, &Defines::new())),
            ["mod"]
        );
        temp.write(
            "inc/include.svh",
            "\nmodule mod1;\nendmodule;\n\nmodule mod2;\nendmodule;\n",
        );
        touch_back(&include);
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &include_dirs, &Defines::new())),
            ["mod1", "mod2"]
        );
        // A deleted include file invalidates the scan too.
        fs::remove_file(&include).unwrap();
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &include_dirs, &Defines::new())),
            [] as [&str; 0]
        );
    }

    #[test]
    fn cached_scan_updated_by_higher_priority_file() {
        let temp = TempRoot::new();
        let path = temp.write("src/file_name.sv", "`include \"include.svh\"\n");
        temp.write(
            "lower_prio/include.svh",
            "\nmodule mod_lower_prio;\nendmodule;\n",
        );
        let include_dirs = [temp.root.join("higher_prio"), temp.root.join("lower_prio")];
        let mut cache = SvScanCache::new();
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &include_dirs, &Defines::new())),
            ["mod_lower_prio"]
        );
        temp.write(
            "higher_prio/include.svh",
            "\nmodule mod_higher_prio;\nendmodule;\n",
        );
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &include_dirs, &Defines::new())),
            ["mod_higher_prio"]
        );
        // The directory of the file comes first.
        temp.write("src/include.svh", "\nmodule mod_own;\nendmodule;\n");
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &include_dirs, &Defines::new())),
            ["mod_own"]
        );
    }

    #[test]
    fn cached_scan_updated_by_other_defines() {
        let temp = TempRoot::new();
        let path = temp.write(
            "file_name.sv",
            "`ifdef foo\nmodule `foo\nendmodule;\n`endif\n",
        );
        let mut cache = SvScanCache::new();
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &[], &Defines::new())),
            [] as [&str; 0]
        );
        let mut defines = Defines::new();
        defines.define("foo", None, "mod1");
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &[], &defines)),
            ["mod1"]
        );
        defines.define("foo", None, "mod2");
        assert_eq!(
            definition_names(&scan(&mut cache, &path, &[], &defines)),
            ["mod2"]
        );
    }

    #[test]
    fn cached_scan_updated_by_other_keywords() {
        let temp = TempRoot::new();
        let path = temp.write("file_name.sv", "package p;\nendpackage\n");
        let mut cache = SvScanCache::new();
        let mut scan_with = |keywords: Option<&str>| {
            let mut chain = chain(&[&path], &[], &Defines::new());
            chain.keywords = keywords.map(|keywords| keywords.parse().unwrap());
            let mut results = cache.scan(&[chain]);
            results.pop().unwrap().pop().unwrap().unwrap().design_file
        };
        assert_eq!(scan_with(None).packages.len(), 1);
        // `package` is an identifier in IEEE 1364.
        assert_eq!(scan_with(Some("1364-2005")).packages.len(), 0);
        assert_eq!(scan_with(Some("1800-2017")).packages.len(), 1);
    }

    #[test]
    fn library_mode_carries_macros_across_files() {
        let temp = TempRoot::new();
        let first = temp.write("a.sv", "`define NAME carried\n");
        let second = temp.write("b.sv", "module `NAME;\nendmodule\n");
        let mut library = chain(&[&first, &second], &[], &Defines::new());
        library.carry_defines = true;
        let mut cache = SvScanCache::new();
        let results = cache.scan(std::slice::from_ref(&library));
        let names: Vec<Vec<String>> = results[0]
            .iter()
            .map(|result| {
                definition_names(&result.as_ref().unwrap().design_file)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .collect();
        assert_eq!(names, [vec![], vec!["carried".to_owned()]]);

        // In file mode, each file starts with the options' macros.
        library.carry_defines = false;
        let file_mode = cache.scan(&[library]);
        assert_eq!(
            definition_names(&file_mode[0][1].as_ref().unwrap().design_file),
            [] as [&str; 0]
        );
    }

    #[test]
    fn unreadable_files_are_errors() {
        let mut cache = SvScanCache::new();
        let missing = Utf8PathBuf::from("/nonexistent/a.sv");
        let results = cache.scan(&[chain(&[&missing], &[], &Defines::new())]);
        results[0][0].as_ref().unwrap_err();
    }
}
