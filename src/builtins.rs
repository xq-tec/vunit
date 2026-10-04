// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The `VUnit` and OSVVM VHDL libraries that every project gets.
//!
//! The files are embedded at build time (see `build.rs`). [`select`] ports the selection
//! logic of `builtins.py` for risim-ghdl, which supports context declarations and package
//! generics, but not VHDL-2019 call paths or a vendor coverage API. [`extract`] writes the
//! files to disk so they can be compiled.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::process;

use camino::Utf8Path;
use camino::Utf8PathBuf;

use crate::diagnostics::Diagnostic;
use crate::spec::Feature;
use crate::vhdl_standard::VhdlStandard;

/// An embedded builtin file.
#[derive(Debug)]
pub struct BuiltinFile {
    /// The path relative to `VUnit`'s `vunit/` directory, with `/` separators, for example
    /// `vhdl/check/src/check.vhd`.
    pub rel_path: &'static str,
    /// The file contents.
    pub contents: &'static [u8],
}

impl BuiltinFile {
    /// The file name without directories.
    pub fn file_name(&self) -> &'static str {
        self.rel_path
            .rsplit_once('/')
            .map_or(self.rel_path, |(_, name)| name)
    }

    /// The directory part of `rel_path`.
    pub fn dir(&self) -> &'static str {
        self.rel_path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
}

include!(concat!(env!("OUT_DIR"), "/builtin_files.rs"));

/// The name of the library with the `VUnit` VHDL packages.
pub const VUNIT_LIB: &str = "vunit_lib";

/// The name of the OSVVM library.
pub const OSVVM_LIB: &str = "osvvm";

/// All embedded files, sorted by [`BuiltinFile::rel_path`].
pub fn all_files() -> &'static [BuiltinFile] {
    BUILTIN_FILES
}

/// A hash identifying the embedded files.
pub const fn hash() -> &'static str {
    BUILTINS_HASH
}

/// A builtin library and its files, in the order `VUnit` adds them.
#[derive(Debug)]
pub struct BuiltinLibrary {
    /// The library name.
    pub name: &'static str,
    /// The files to compile into the library.
    pub files: Vec<&'static BuiltinFile>,
}

/// The builtin libraries for a project, and problems with the requested features.
#[derive(Debug)]
pub struct Selection {
    /// `vunit_lib`, then `osvvm` unless it was left out.
    pub libraries: Vec<BuiltinLibrary>,
    /// Errors for features that the VHDL standard doesn't support.
    pub diagnostics: Vec<Diagnostic>,
}

/// OSVVM files that aren't compiled: the BVUL alert body, VHDL-2019 variants, and the Aldec
/// coverage API.
const OSVVM_EXCLUDED: [&str; 2] = ["AlertLogPkg_body_BVUL.vhd", "VendorCovApiPkg_Aldec.vhd"];

/// OSVVM variants for simulators without package generics, which risim-ghdl has.
const OSVVM_WITHOUT_PACKAGE_GENERICS: [&str; 4] = [
    "ScoreboardPkg_int_c.vhd",
    "ScoreboardPkg_slv_c.vhd",
    "MemoryPkg_c.vhd",
    "MemoryPkg_orig_c.vhd",
];

/// Selects the builtin files for a project.
///
/// The `VUnit` core libraries are always selected, and so are com (from VHDL-2008 on) and OSVVM
/// unless `include_osvvm` is false because the project defines its own `osvvm` library.
pub fn select(
    standard: VhdlStandard,
    features: &BTreeSet<Feature>,
    include_osvvm: bool,
) -> Selection {
    let mut diagnostics = Vec::new();
    let mut vunit_lib = Vec::new();

    // `add_vhdl_builtins`: data types, logging, top-level contexts, and the core packages.
    add_files(&mut vunit_lib, standard, files_in("vhdl/data_types/src"));
    for key in ["string", "integer_vector"] {
        let path = format!("vhdl/data_types/src/api/external_{key}_pkg.vhd");
        add_files(&mut vunit_lib, standard, file(&path).into_iter());
    }
    // risim-ghdl has no call paths, so the VHDL-2008 location body is used for all standards.
    vunit_lib.extend(file("vhdl/logging/src/location_pkg-body-2008m.vhd"));
    vunit_lib.extend(
        files_in("vhdl/logging/src")
            .filter(|file| !file.file_name().starts_with("location_pkg-body"))
            .filter(|file| standard.is_allowed_by_file_name(file.file_name())),
    );
    add_files(&mut vunit_lib, standard, files_in("vhdl"));
    for dir in ["core", "string_ops", "check", "dictionary", "run", "path"] {
        add_files(
            &mut vunit_lib,
            standard,
            files_in(&format!("vhdl/{dir}/src")),
        );
    }

    let requires_2008 = |feature: Feature, errors: &mut Vec<Diagnostic>| {
        let supported = standard >= VhdlStandard::Vhdl2008;
        if !supported {
            errors.push(Diagnostic::error(format!(
                "VUnit feature '{feature}' requires VHDL-2008 or later, but the project uses \
                 VHDL-{standard}"
            )));
        }
        supported
    };

    // com is always added, but only exists from VHDL-2008 on.
    if standard >= VhdlStandard::Vhdl2008 {
        add_files(&mut vunit_lib, standard, files_in("vhdl/com/src"));
    }
    if features.contains(&Feature::Random) && requires_2008(Feature::Random, &mut diagnostics) {
        // `VUnit` adds these without filtering by standard.
        vunit_lib.extend(files_in("vhdl/random/src"));
    }
    if features.contains(&Feature::VerificationComponents)
        && requires_2008(Feature::VerificationComponents, &mut diagnostics)
    {
        add_files(
            &mut vunit_lib,
            standard,
            files_in("vhdl/verification_components/src"),
        );
    }

    let mut libraries = vec![BuiltinLibrary {
        name: VUNIT_LIB,
        files: vunit_lib,
    }];
    if include_osvvm {
        let osvvm = files_in("vhdl/osvvm")
            .filter(|file| {
                let name = file.file_name();
                !OSVVM_EXCLUDED.contains(&name)
                    && !name.contains("2019")
                    && !OSVVM_WITHOUT_PACKAGE_GENERICS.contains(&name)
            })
            .collect();
        libraries.push(BuiltinLibrary {
            name: OSVVM_LIB,
            files: osvvm,
        });
    }

    Selection {
        libraries,
        diagnostics,
    }
}

/// `Builtins._add_files`: skips files whose standard tags exclude `standard`, and context
/// declarations if the standard has none.
fn add_files(
    library: &mut Vec<&'static BuiltinFile>,
    standard: VhdlStandard,
    files: impl Iterator<Item = &'static BuiltinFile>,
) {
    library.extend(files.filter(|file| {
        standard.is_allowed_by_file_name(file.file_name())
            && (standard.supports_context() || !file.rel_path.ends_with("_context.vhd"))
    }));
}

/// The VHDL files directly in `dir`.
fn files_in(dir: &str) -> impl Iterator<Item = &'static BuiltinFile> + '_ {
    BUILTIN_FILES.iter().filter(move |file| {
        file.dir() == dir
            && Utf8Path::new(file.rel_path)
                .extension()
                .is_some_and(|extension| extension == "vhd")
    })
}

fn file(rel_path: &str) -> Option<&'static BuiltinFile> {
    BUILTIN_FILES.iter().find(|file| file.rel_path == rel_path)
}

/// Writes the builtin files to `builtins_root/<hash>/` unless that directory exists, and
/// deletes other entries of `builtins_root`. Returns the directory with the files.
///
/// The files are written to a temporary directory first and then renamed, so a crash never
/// leaves an incomplete directory behind.
///
/// # Errors
///
/// Fails if the files can't be written. Failures to delete old directories are only logged.
pub fn extract(builtins_root: &Utf8Path) -> io::Result<Utf8PathBuf> {
    let target = builtins_root.join(BUILTINS_HASH);
    if !target.is_dir() {
        let temp = builtins_root.join(format!(".tmp-{BUILTINS_HASH}-{}", process::id()));
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        for file in BUILTIN_FILES {
            let path = temp.join(file.rel_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, file.contents)?;
        }
        if let Err(error) = fs::rename(&temp, &target) {
            // Another process may have extracted the same files in the meantime.
            let _ignored = fs::remove_dir_all(&temp);
            if !target.is_dir() {
                return Err(error);
            }
        }
    }

    for entry in fs::read_dir(builtins_root)? {
        let entry = entry?;
        if entry.file_name() == BUILTINS_HASH {
            continue;
        }
        let path = entry.path();
        let result = if entry.file_type()?.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(error) = result {
            tracing::warn!(path = %path.display(), %error, "failed to delete old builtins");
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempRoot;

    fn rel_paths(library: &BuiltinLibrary) -> Vec<&'static str> {
        library.files.iter().map(|file| file.rel_path).collect()
    }

    fn select_default(standard: VhdlStandard) -> Selection {
        select(standard, &BTreeSet::new(), true)
    }

    #[test]
    fn table_is_sorted_and_has_osvvm() {
        let files = all_files();
        assert!(
            files
                .windows(2)
                .all(|pair| pair[0].rel_path < pair[1].rel_path)
        );
        assert!(
            files
                .iter()
                .any(|file| file.rel_path == "vhdl/osvvm/OsvvmContext.vhd")
        );
        assert!(
            files
                .iter()
                .any(|file| file.rel_path == "verilog/vunit_pkg.sv")
        );
        assert!(!files.iter().any(|file| file.rel_path.contains("/test/")));
        assert_eq!(hash().len(), 16);
    }

    #[test]
    fn default_selection_for_2008() {
        let selection = select_default(VhdlStandard::Vhdl2008);
        assert!(selection.diagnostics.is_empty());
        let [vunit_lib, osvvm] = selection.libraries.as_slice() else {
            panic!("expected vunit_lib and osvvm");
        };
        assert_eq!(vunit_lib.name, VUNIT_LIB);
        let vunit_files = rel_paths(vunit_lib);
        for expected in [
            "vhdl/vunit_context.vhd",
            "vhdl/run/src/run.vhd",
            "vhdl/check/src/check.vhd",
            "vhdl/com/src/com.vhd",
            "vhdl/logging/src/location_pkg-body-2008m.vhd",
            "vhdl/data_types/src/api/external_string_pkg.vhd",
        ] {
            assert!(vunit_files.contains(&expected), "missing {expected}");
        }
        assert!(
            !vunit_files
                .iter()
                .any(|path| path.contains("location_pkg-body-2019p"))
        );
        assert!(
            !vunit_files
                .iter()
                .any(|path| path.starts_with("vhdl/random/"))
        );
        assert!(
            !vunit_files
                .iter()
                .any(|path| path.starts_with("vhdl/verification_components/"))
        );
        // Each file is selected once.
        let unique: BTreeSet<_> = vunit_files.iter().collect();
        assert_eq!(unique.len(), vunit_files.len());

        let osvvm_files = rel_paths(osvvm);
        assert!(osvvm_files.contains(&"vhdl/osvvm/ScoreboardGenericPkg.vhd"));
        assert!(osvvm_files.contains(&"vhdl/osvvm/VendorCovApiPkg.vhd"));
        for excluded in [
            "vhdl/osvvm/AlertLogPkg_body_BVUL.vhd",
            "vhdl/osvvm/VendorCovApiPkg_Aldec.vhd",
            "vhdl/osvvm/ScoreboardPkg_int_c.vhd",
            "vhdl/osvvm/MemoryPkg_orig_c.vhd",
        ] {
            assert!(!osvvm_files.contains(&excluded), "{excluded} is selected");
        }
        assert!(!osvvm_files.iter().any(|path| path.contains("2019")));
    }

    #[test]
    fn features_add_files() {
        let features = BTreeSet::from(Feature::ALL);
        let selection = select(VhdlStandard::Vhdl2019, &features, false);
        assert!(selection.diagnostics.is_empty());
        assert_eq!(selection.libraries.len(), 1);
        let vunit_files = rel_paths(&selection.libraries[0]);
        assert!(
            vunit_files
                .iter()
                .any(|path| path.starts_with("vhdl/random/src/"))
        );
        assert!(
            vunit_files
                .iter()
                .any(|path| path.starts_with("vhdl/verification_components/src/"))
        );
    }

    #[test]
    fn vhdl_93_has_no_contexts_or_com() {
        let selection = select_default(VhdlStandard::Vhdl1993);
        assert!(selection.diagnostics.is_empty());
        let vunit_files = rel_paths(&selection.libraries[0]);
        assert!(
            !vunit_files
                .iter()
                .any(|path| path.ends_with("_context.vhd"))
        );
        assert!(!vunit_files.iter().any(|path| path.starts_with("vhdl/com/")));
        assert!(vunit_files.contains(&"vhdl/logging/src/location_pkg-body-2008m.vhd"));
    }

    #[test]
    fn features_need_vhdl_2008() {
        let features = BTreeSet::from(Feature::ALL);
        let selection = select(VhdlStandard::Vhdl2002, &features, true);
        assert_eq!(selection.diagnostics.len(), 2);
        let vunit_files = rel_paths(&selection.libraries[0]);
        assert!(
            !vunit_files
                .iter()
                .any(|path| path.starts_with("vhdl/random/"))
        );
    }

    #[test]
    fn extract_writes_files_once_and_removes_old_ones() {
        let temp = TempRoot::new();
        let root = temp.root.join("builtins");
        fs::create_dir_all(root.join("old")).unwrap();

        let target = extract(&root).unwrap();
        assert_eq!(target, root.join(hash()));
        assert!(!root.join("old").exists());
        let check = target.join("vhdl/check/src/check.vhd");
        assert!(check.is_file());

        // An existing directory is reused as it is.
        fs::write(&check, "changed").unwrap();
        assert_eq!(extract(&root).unwrap(), target);
        assert_eq!(fs::read_to_string(&check).unwrap(), "changed");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }
}
