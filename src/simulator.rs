// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The risim-ghdl simulator: its identity and version, and its command lines.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::sync::LazyLock;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::process;
use crate::spec::AssertLevel;
use crate::store::FileTime;
use crate::vhdl_standard::VhdlStandard;

/// What identifies a risim-ghdl installation.
///
/// Libraries compiled with another identity are discarded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimulatorIdentity {
    /// The executable.
    pub path: Utf8PathBuf,
    /// The size of the executable.
    pub size: u64,
    /// The modification time of the executable.
    pub modified: Option<FileTime>,
    /// The output of `--version`.
    pub version_output: String,
}

/// The version of risim-ghdl, from the `GHDL <major>.<minor>` line of `--version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// The major version.
    pub major: u32,
    /// The minor version.
    pub minor: u32,
}

/// risim-ghdl can't be used.
#[derive(Debug, Error)]
pub enum DetectError {
    /// The executable can't be inspected or run.
    #[error("failed to run {path}")]
    Io {
        /// The executable.
        path: Utf8PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// `--version` failed or printed something unexpected.
    #[error("could not determine the risim-ghdl version from '{path} --version':\n{output}")]
    Version {
        /// The executable.
        path: Utf8PathBuf,
        /// What the executable printed.
        output: String,
    },
}

/// A command line can't be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    /// The VHDL standard needs a newer risim-ghdl.
    #[error("VHDL-2019 requires risim-ghdl 6.0 or later, but the version is {major}.{minor}")]
    UnsupportedStandard {
        /// The major version.
        major: u32,
        /// The minor version.
        minor: u32,
    },
}

/// A detected risim-ghdl executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simulator {
    identity: SimulatorIdentity,
    version: Version,
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::unwrap_used, reason = "the regex is a valid constant")]
    Regex::new(r"^GHDL ([0-9]+)\.([0-9]+)[^\n]*?\[simulation adapter\]").unwrap()
});

impl Simulator {
    /// Runs `<path> --version` and records the identity of the executable.
    ///
    /// # Errors
    ///
    /// Fails if the executable can't be run, or isn't risim-ghdl.
    pub async fn detect(path: &Utf8Path) -> Result<Self, DetectError> {
        let io_error = |source| DetectError::Io {
            path: path.to_owned(),
            source,
        };
        let metadata = fs::metadata(path).map_err(io_error)?;
        let output = process::command(path, ["--version"], None)
            .output()
            .await
            .map_err(io_error)?;
        let mut version_output = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            version_output.push_str(&String::from_utf8_lossy(&output.stderr));
            return Err(DetectError::Version {
                path: path.to_owned(),
                output: version_output,
            });
        }
        let identity = SimulatorIdentity {
            path: path.to_owned(),
            size: metadata.len(),
            modified: metadata
                .modified()
                .ok()
                .and_then(FileTime::from_system_time),
            version_output,
        };
        Self::from_identity(identity).map_err(|identity| DetectError::Version {
            path: path.to_owned(),
            output: identity.version_output,
        })
    }

    /// Creates a simulator from an identity, parsing the version from its `--version` output.
    ///
    /// # Errors
    ///
    /// Returns the identity if the output doesn't contain a risim-ghdl version.
    pub fn from_identity(identity: SimulatorIdentity) -> Result<Self, SimulatorIdentity> {
        let version = VERSION_RE
            .captures(&identity.version_output)
            .and_then(|captures| {
                Some(Version {
                    major: captures.get(1)?.as_str().parse().ok()?,
                    minor: captures.get(2)?.as_str().parse().ok()?,
                })
            });
        match version {
            Some(version) => Ok(Self { identity, version }),
            None => Err(identity),
        }
    }

    /// The identity of the executable.
    pub const fn identity(&self) -> &SimulatorIdentity {
        &self.identity
    }

    /// The executable.
    pub fn path(&self) -> &Utf8Path {
        &self.identity.path
    }

    /// The version.
    pub const fn version(&self) -> Version {
        self.version
    }

    /// The value of `--std=` for `standard`.
    ///
    /// # Errors
    ///
    /// Fails for VHDL-2019 before risim-ghdl 6.0.
    pub const fn std_flag(&self, standard: VhdlStandard) -> Result<&'static str, CommandError> {
        Ok(match standard {
            VhdlStandard::Vhdl1993 => "93",
            VhdlStandard::Vhdl2002 => "02",
            VhdlStandard::Vhdl2008 => "08",
            VhdlStandard::Vhdl2019 => {
                if self.version.major < 6 {
                    return Err(CommandError::UnsupportedStandard {
                        major: self.version.major,
                        minor: self.version.minor,
                    });
                }
                "19"
            },
        })
    }

    /// The beginning of a command line in `mode`, up to the `-P` options.
    fn base_command(
        &self,
        mode: &str,
        vhdl_standard: VhdlStandard,
        library: &str,
        library_dir: &Utf8Path,
        library_dirs: &[Utf8PathBuf],
    ) -> Result<Vec<String>, CommandError> {
        let mut command = vec![
            self.path().to_string(),
            mode.to_owned(),
            format!("--std={}", self.std_flag(vhdl_standard)?),
            format!("--work={library}"),
            format!("--workdir={library_dir}"),
        ];
        command.extend(library_dirs.iter().map(|dir| format!("-P{dir}")));
        Ok(command)
    }

    /// The command line analysing one file.
    ///
    /// # Errors
    ///
    /// Fails if the standard isn't supported.
    pub fn compile_command(&self, args: &CompileArgs<'_>) -> Result<Vec<String>, CommandError> {
        let mut command = self.base_command(
            "-a",
            args.vhdl_standard,
            args.library,
            args.library_dir,
            args.library_dirs,
        )?;
        command.extend(args.flags.iter().cloned());
        command.push(args.file.to_string());
        Ok(command)
    }

    /// The command line elaborating and running a testbench.
    ///
    /// # Errors
    ///
    /// Fails if the standard isn't supported.
    pub fn simulate_command(&self, args: &SimulateArgs<'_>) -> Result<Vec<String>, CommandError> {
        let mut command = self.base_command(
            "--elab-run",
            args.vhdl_standard,
            args.library,
            args.library_dir,
            args.library_dirs,
        )?;
        command.extend(args.elab_flags.iter().cloned());
        match args.top {
            Top::Configuration(name) => command.push(name.clone()),
            Top::Entity {
                entity,
                architecture,
            } => command.extend([entity.clone(), architecture.clone()]),
        }
        command.extend(args.sim_flags.iter().cloned());
        command.extend(
            args.generics
                .iter()
                .map(|(name, value)| format!("-g{name}={value}")),
        );
        command.push(format!("--assert-level={}", args.assert_level));
        if args.disable_ieee_asserts {
            command.push("--ieee-asserts=disable".to_owned());
        }
        if args.wait {
            command.push("--wait".to_owned());
        }
        command.push(format!("--name={}", args.name));
        Ok(command)
    }
}

/// The arguments of [`Simulator::compile_command`].
#[derive(Debug, Clone)]
pub struct CompileArgs<'a> {
    /// The library to compile into.
    pub library: &'a str,
    /// The work directory of the library.
    pub library_dir: &'a Utf8Path,
    /// The standard.
    pub vhdl_standard: VhdlStandard,
    /// The directories of all libraries, passed with `-P`.
    pub library_dirs: &'a [Utf8PathBuf],
    /// Extra analysis flags.
    pub flags: &'a [String],
    /// The file to compile.
    pub file: &'a Utf8Path,
}

/// The top-level unit of a simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Top {
    /// A VHDL configuration.
    Configuration(String),
    /// An entity and its architecture.
    Entity {
        /// The entity name.
        entity: String,
        /// The architecture name.
        architecture: String,
    },
}

/// The arguments of [`Simulator::simulate_command`].
#[derive(Debug, Clone)]
pub struct SimulateArgs<'a> {
    /// The standard.
    pub vhdl_standard: VhdlStandard,
    /// The library of the testbench.
    pub library: &'a str,
    /// The work directory of that library.
    pub library_dir: &'a Utf8Path,
    /// The directories of all libraries, passed with `-P`.
    pub library_dirs: &'a [Utf8PathBuf],
    /// Extra elaboration flags.
    pub elab_flags: &'a [String],
    /// The unit to elaborate.
    pub top: &'a Top,
    /// Extra simulation flags.
    pub sim_flags: &'a [String],
    /// Generic values.
    pub generics: &'a BTreeMap<String, String>,
    /// The assertion severity that stops the simulation.
    pub assert_level: AssertLevel,
    /// Whether to disable assertions in the IEEE libraries.
    pub disable_ieee_asserts: bool,
    /// Whether to start paused (GUI mode).
    pub wait: bool,
    /// The testcase name, which links the simulation to the testcase.
    pub name: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::simulator_identity;

    const VERSION_OUTPUT: &str = "GHDL 6.4.0-risim (tarball) [simulation adapter]\n \
                                  Compiled with GNAT Version: 10.5.0\n";

    fn simulator(version_output: &str) -> Simulator {
        Simulator::from_identity(simulator_identity(version_output)).unwrap()
    }

    #[test]
    fn parses_version() {
        assert_eq!(
            simulator(VERSION_OUTPUT).version(),
            Version { major: 6, minor: 4 }
        );
    }

    #[test]
    fn rejects_plain_ghdl() {
        for output in [
            "GHDL 4.1.0 (Ubuntu) [Dunoon edition]\n",
            "something else",
            "GHDL 6.4.0\n[simulation adapter]",
        ] {
            Simulator::from_identity(simulator_identity(output)).unwrap_err();
        }
    }

    #[test]
    fn vhdl_2019_needs_version_6() {
        assert_eq!(
            simulator(VERSION_OUTPUT).std_flag(VhdlStandard::Vhdl2019),
            Ok("19")
        );
        let old = simulator("GHDL 5.1.0 [simulation adapter]");
        assert_eq!(old.std_flag(VhdlStandard::Vhdl2008), Ok("08"));
        assert_eq!(old.std_flag(VhdlStandard::Vhdl1993), Ok("93"));
        assert_eq!(
            old.std_flag(VhdlStandard::Vhdl2019),
            Err(CommandError::UnsupportedStandard { major: 5, minor: 1 })
        );
    }

    #[test]
    fn compile_command() {
        let library_dirs = vec![
            Utf8PathBuf::from("/out/libraries/vunit_lib"),
            Utf8PathBuf::from("/out/libraries/lib"),
        ];
        let command = simulator(VERSION_OUTPUT)
            .compile_command(&CompileArgs {
                library: "lib",
                library_dir: Utf8Path::new("/out/libraries/lib"),
                vhdl_standard: VhdlStandard::Vhdl2008,
                library_dirs: &library_dirs,
                flags: &["-fsynopsys".to_owned()],
                file: Utf8Path::new("/src/a.vhd"),
            })
            .unwrap();
        assert_eq!(
            command,
            [
                "/bin/risim-ghdl",
                "-a",
                "--std=08",
                "--work=lib",
                "--workdir=/out/libraries/lib",
                "-P/out/libraries/vunit_lib",
                "-P/out/libraries/lib",
                "-fsynopsys",
                "/src/a.vhd",
            ]
        );
    }

    #[test]
    fn simulate_command() {
        let library_dirs = vec![Utf8PathBuf::from("/out/libraries/lib")];
        let generics = BTreeMap::from([
            ("a".to_owned(), "1".to_owned()),
            ("runner_cfg".to_owned(), "x : y".to_owned()),
        ]);
        let simulator = simulator(VERSION_OUTPUT);
        let mut args = SimulateArgs {
            vhdl_standard: VhdlStandard::Vhdl2008,
            library: "lib",
            library_dir: Utf8Path::new("/out/libraries/lib"),
            library_dirs: &library_dirs,
            elab_flags: &["-frelaxed".to_owned()],
            top: &Top::Entity {
                entity: "tb".to_owned(),
                architecture: "a".to_owned(),
            },
            sim_flags: &["--stop-time=1ms".to_owned()],
            generics: &generics,
            assert_level: AssertLevel::Error,
            disable_ieee_asserts: false,
            wait: false,
            name: "lib.tb.test",
        };
        assert_eq!(
            simulator.simulate_command(&args).unwrap(),
            [
                "/bin/risim-ghdl",
                "--elab-run",
                "--std=08",
                "--work=lib",
                "--workdir=/out/libraries/lib",
                "-P/out/libraries/lib",
                "-frelaxed",
                "tb",
                "a",
                "--stop-time=1ms",
                "-ga=1",
                "-grunner_cfg=x : y",
                "--assert-level=error",
                "--name=lib.tb.test",
            ]
        );

        let configuration = Top::Configuration("cfg".to_owned());
        args.top = &configuration;
        args.assert_level = AssertLevel::Warning;
        args.disable_ieee_asserts = true;
        args.wait = true;
        let command = simulator.simulate_command(&args).unwrap();
        assert_eq!(
            command[7..],
            [
                "cfg",
                "--stop-time=1ms",
                "-ga=1",
                "-grunner_cfg=x : y",
                "--assert-level=warning",
                "--ieee-asserts=disable",
                "--wait",
                "--name=lib.tb.test",
            ]
        );
    }
}
