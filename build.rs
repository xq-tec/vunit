// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Generates the table of builtin VUnit and OSVVM files that the crate embeds.
//!
//! The table lists every file the `builtins` module can select, with its path relative to the
//! `vunit/` directory and its contents. `BUILTINS_HASH` identifies the whole set, so extracted
//! copies can be reused as long as the sources don't change.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let vunit_dir = manifest_dir.join("vunit");
    println!("cargo:rerun-if-changed=vunit/vhdl");
    println!("cargo:rerun-if-changed=vunit/verilog");

    let mut rel_paths = Vec::new();
    collect_files(&vunit_dir, "vhdl", &mut rel_paths, |rel_path| {
        let parts: Vec<&str> = rel_path.split('/').collect();
        // vhdl/*.vhd, vhdl/osvvm/*.vhd and vhdl/*/src/**/*.vhd
        matches!(
            parts.as_slice(),
            [_, _] | [_, "osvvm", _] | [_, _, "src", ..]
        ) && has_extension(rel_path, "vhd")
    });
    check_osvvm(&rel_paths);
    collect_files(&vunit_dir, "verilog", &mut rel_paths, |rel_path| {
        let parts: Vec<&str> = rel_path.split('/').collect();
        match parts.as_slice() {
            // verilog/*.sv
            [_, name] => has_extension(name, "sv"),
            // verilog/include/**
            [_, "include", ..] => true,
            _ => false,
        }
    });
    rel_paths.sort();

    let mut hasher = blake3::Hasher::new();
    let mut table = String::new();
    for rel_path in &rel_paths {
        let path = vunit_dir.join(rel_path);
        let contents = fs::read(&path).expect("builtin file is readable");
        // Lengths delimit the entries, so moving bytes between a path and contents changes the hash.
        hasher.update(&(rel_path.len() as u64).to_le_bytes());
        hasher.update(rel_path.as_bytes());
        hasher.update(&(contents.len() as u64).to_le_bytes());
        hasher.update(&contents);
        let path = path.to_str().expect("builtin path is UTF-8");
        writeln!(
            table,
            "    BuiltinFile {{ rel_path: {rel_path:?}, contents: include_bytes!({path:?}) }},"
        )
        .expect("writing to a String succeeds");
    }
    let hash = hasher.finalize().to_hex();
    // A short prefix keeps extracted paths short, which matters on Windows.
    let hash = hash.get(..16).expect("hash has 64 hex digits");

    let generated = format!(
        "/// Every builtin file, sorted by `rel_path`.\n\
         pub(crate) static BUILTIN_FILES: &[BuiltinFile] = &[\n{table}];\n\n\
         /// Hash over the paths and contents of all builtin files.\n\
         pub(crate) const BUILTINS_HASH: &str = {hash:?};\n"
    );
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    fs::write(out_dir.join("builtin_files.rs"), generated).expect("OUT_DIR is writable");
}

/// Adds the files below `vunit_dir/sub_dir` that `select` accepts, as `/`-separated paths
/// relative to `vunit_dir`.
fn collect_files(
    vunit_dir: &Path,
    sub_dir: &str,
    rel_paths: &mut Vec<String>,
    select: impl Fn(&str) -> bool + Copy,
) {
    let mut pending = vec![vunit_dir.join(sub_dir)];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("builtin directory is readable") {
            let entry = entry.expect("builtin directory entry is readable");
            let path = entry.path();
            let file_type = entry.file_type().expect("builtin file type is readable");
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            let rel_path = path
                .strip_prefix(vunit_dir)
                .expect("walked path is below the vunit directory")
                .to_str()
                .expect("builtin path is UTF-8")
                .replace('\\', "/");
            if select(&rel_path) {
                rel_paths.push(rel_path);
            }
        }
    }
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|found| found == extension)
}

fn check_osvvm(rel_paths: &[String]) {
    assert!(
        rel_paths.iter().any(|path| path.starts_with("vhdl/osvvm/")),
        "Found no OSVVM VHDL files in vunit/vhdl/osvvm. \
         Run `git submodule update --init --recursive`."
    );
}
