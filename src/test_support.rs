// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Helpers shared by the unit tests.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fs;
use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;

use crate::project::FileId;
use crate::project::Project;
use crate::simulator::Simulator;
use crate::simulator::SimulatorIdentity;
use crate::sources::ContentHash;
use crate::sources::FileKind;
use crate::verilog_parser;
use crate::verilog_parser::Defines;
use crate::verilog_parser::KeywordSet;
use crate::verilog_parser::ScanInput;
use crate::vhdl_parser::VhdlDesignFile;

/// A temporary directory with a UTF-8 path, deleted when dropped.
pub(crate) struct TempRoot {
    _dir: tempfile::TempDir,
    pub(crate) root: Utf8PathBuf,
}

impl TempRoot {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap().to_owned();
        Self { _dir: dir, root }
    }

    /// Writes `contents` to `rel_path`, creating its directory, and returns the full path.
    pub(crate) fn write(&self, rel_path: &str, contents: impl AsRef<[u8]>) -> Utf8PathBuf {
        let path = self.root.join(rel_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
}

/// The identity of a fake risim-runner at `/bin/risim-runner` that prints `version_output` for
/// `--version`, with the analyzer `/bin/risim`.
pub(crate) fn simulator_identity(version_output: &str) -> SimulatorIdentity {
    SimulatorIdentity {
        path: "/bin/risim-runner".into(),
        analyzer: "/bin/risim".into(),
        version_output: version_output.to_owned(),
    }
}

/// A fake risim-runner 0.1.
pub(crate) fn simulator() -> Simulator {
    Simulator::from_identity(simulator_identity(
        "risim-runner 0.1.0\nhydrolyze archive format 1\n",
    ))
    .unwrap()
}

/// Parses `code` and adds it to `library` as the file `path`; unparsable code is added
/// without design units.
pub(crate) fn add_vhdl(
    project: &mut Project,
    library: &str,
    path: &Utf8Path,
    code: &str,
) -> FileId {
    let library = project.find_library(library).unwrap();
    let design_file = VhdlDesignFile::parse(code.as_bytes()).ok().map(Arc::new);
    project.add_source_file(
        library,
        path,
        None,
        ContentHash::of(code.as_bytes()),
        design_file,
    )
}

/// Scans `code` and adds it to `library` as the SystemVerilog file `path`; includes are read
/// from disk, from the directory of `path` only.
pub(crate) fn add_sv(project: &mut Project, library: &str, path: &Utf8Path, code: &str) -> FileId {
    add_sv_including(project, library, path, code, &[])
}

/// Scans `code` and adds it to `library` as the SystemVerilog file `path`; includes are read
/// from disk, from the directory of `path` and then from `include_dirs`.
pub(crate) fn add_sv_including(
    project: &mut Project,
    library: &str,
    path: &Utf8Path,
    code: &str,
    include_dirs: &[Utf8PathBuf],
) -> FileId {
    let library = project.find_library(library).unwrap();
    let (design_file, _) = verilog_parser::scan(
        &ScanInput {
            path,
            contents: code.as_bytes(),
            include_dirs,
            defines: &Defines::new(),
            keywords: KeywordSet::Sv,
        },
        |candidate| fs::read(candidate).map(Arc::from),
    );
    let include_hashes = design_file
        .includes
        .iter()
        .map(|include| {
            let contents = fs::read(include.path.as_ref()?).ok()?;
            Some(ContentHash::of(&contents))
        })
        .collect();
    project.add_sv_file(
        library,
        path,
        FileKind::SystemVerilog,
        ContentHash::of(code.as_bytes()),
        Some(Arc::new(design_file)),
        include_hashes,
    )
}
