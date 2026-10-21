// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The simulator and its command lines.
//!
//! risim-runner (`risim`) analyses every library with `risim analyze`, into the archive
//! `<library dir>/<library>.hlib`, and simulates with `risim run`, which reads the same archives.
//! It supports VHDL-2008 only.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
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
use crate::vhdl_standard::VhdlStandard;

/// What identifies a risim-runner installation.
///
/// Libraries compiled with another identity are discarded. The `--version` output names the
/// archive format, so a rebuilt risim-runner with the same format keeps the libraries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimulatorIdentity {
    /// The executable.
    pub path: Utf8PathBuf,
    /// The output of `--version`.
    pub version_output: String,
}

/// The version of risim-runner, from the `risim-runner <major>.<minor>` line of `--version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// The major version.
    pub major: u32,
    /// The minor version.
    pub minor: u32,
}

/// risim-runner can't be used.
#[derive(Debug, Error)]
pub enum DetectError {
    /// The executable can't be run.
    #[error("failed to run {path}")]
    Io {
        /// The executable.
        path: Utf8PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// `--version` failed or printed something unexpected.
    #[error("could not determine the risim-runner version from '{path} --version':\n{output}")]
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
    /// risim-runner supports VHDL-2008 only.
    #[error("risim supports only VHDL-2008, not VHDL-{standard}")]
    UnsupportedStandard {
        /// The standard.
        standard: VhdlStandard,
    },
}

/// Checks that risim-runner supports `standard`.
///
/// # Errors
///
/// Fails for every standard but VHDL-2008.
pub const fn check_standard(standard: VhdlStandard) -> Result<(), CommandError> {
    match standard {
        VhdlStandard::Vhdl2008 => Ok(()),
        VhdlStandard::Vhdl1993 | VhdlStandard::Vhdl2002 | VhdlStandard::Vhdl2019 => {
            Err(CommandError::UnsupportedStandard { standard })
        },
    }
}

/// A detected risim-runner executable; cheap enough to clone into every simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simulator {
    identity: SimulatorIdentity,
    version: Version,
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::unwrap_used, reason = "the regex is a valid constant")]
    Regex::new(r"^risim-runner ([0-9]+)\.([0-9]+)").unwrap()
});

/// The major and minor version in the `--version` output `output`.
fn parse_version(output: &str) -> Option<Version> {
    let captures = VERSION_RE.captures(output)?;
    Some(Version {
        major: captures.get(1)?.as_str().parse().ok()?,
        minor: captures.get(2)?.as_str().parse().ok()?,
    })
}

impl Simulator {
    /// Runs `<path> --version` and records the identity of the executable.
    ///
    /// # Errors
    ///
    /// Fails if the executable can't be run, or isn't risim-runner.
    pub async fn detect(path: &Utf8Path) -> Result<Self, DetectError> {
        let output = process::command(path, ["--version"], None)
            .output()
            .await
            .map_err(|source| DetectError::Io {
                path: path.to_owned(),
                source,
            })?;
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
    /// Returns the identity if the output doesn't contain a risim-runner version.
    pub fn from_identity(identity: SimulatorIdentity) -> Result<Self, SimulatorIdentity> {
        match parse_version(&identity.version_output) {
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

    /// The beginning of a command line of the subcommand `mode`, up to the library paths.
    fn base_command(
        &self,
        mode: &str,
        vhdl_standard: VhdlStandard,
        library: &str,
        library_dir: &Utf8Path,
        library_dirs: &[Utf8PathBuf],
    ) -> Result<Vec<String>, CommandError> {
        check_standard(vhdl_standard)?;
        let mut command = vec![
            self.path().to_string(),
            mode.to_owned(),
            format!("--work={library}"),
            format!("--workdir={library_dir}"),
        ];
        for dir in library_dirs {
            command.extend(["-L".to_owned(), dir.to_string()]);
        }
        Ok(command)
    }

    /// The command line analysing files into one library, which prints the reports as JSON lines.
    ///
    /// # Errors
    ///
    /// Fails if the standard isn't supported.
    pub fn compile_command(&self, args: &CompileArgs<'_>) -> Result<Vec<String>, CommandError> {
        let mut command = self.base_command(
            "analyze",
            args.vhdl_standard,
            args.library,
            args.library_dir,
            args.library_dirs,
        )?;
        command.push("--message-format=json".to_owned());
        command.extend(args.flags.iter().cloned());
        command.extend(args.files.iter().map(ToString::to_string));
        Ok(command)
    }

    /// The command line elaborating and running a testbench.
    ///
    /// # Errors
    ///
    /// Fails if the standard isn't supported.
    pub fn simulate_command(&self, args: &SimulateArgs<'_>) -> Result<Vec<String>, CommandError> {
        let mut command = self.base_command(
            "run",
            args.vhdl_standard,
            args.library,
            args.library_dir,
            args.library_dirs,
        )?;
        command.extend(args.elab_flags.iter().cloned());
        command.extend(
            args.generics
                .iter()
                .map(|(name, value)| format!("--generic={name}={value}")),
        );
        command.push(format!("--assert-level={}", args.assert_level));
        if args.disable_ieee_asserts {
            command.push("--ieee-asserts=disable".to_owned());
        }
        if args.wait {
            command.push("--paused".to_owned());
        }
        command.push(format!("--name={}", args.name));
        command.extend(args.sim_flags.iter().cloned());
        command.push(match args.top {
            Top::Configuration(name) => name.clone(),
            Top::Entity {
                entity,
                architecture,
            } => format!("{entity}({architecture})"),
        });
        Ok(command)
    }
}

/// The arguments of [`Simulator::compile_command`].
#[derive(Debug, Clone)]
pub struct CompileArgs<'a> {
    /// The library to compile into.
    pub library: &'a str,
    /// The directory of the library's archive.
    pub library_dir: &'a Utf8Path,
    /// The standard.
    pub vhdl_standard: VhdlStandard,
    /// The directories of all libraries, passed with `-L`.
    pub library_dirs: &'a [Utf8PathBuf],
    /// Extra analysis options.
    pub flags: &'a [String],
    /// The files to compile, in compile order.
    pub files: &'a [&'a Utf8Path],
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
    /// The directory of that library's archive.
    pub library_dir: &'a Utf8Path,
    /// The directories of all libraries, passed with `-L`.
    pub library_dirs: &'a [Utf8PathBuf],
    /// Extra elaboration options, the `options` of the configuration.
    pub elab_flags: &'a [String],
    /// The unit to elaborate.
    pub top: &'a Top,
    /// Extra risim-runner options.
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
    use crate::test_support::simulator;
    use crate::test_support::simulator_identity;

    #[test]
    fn parses_version() {
        assert_eq!(simulator().version(), Version { major: 0, minor: 1 });
        assert_eq!(simulator().path(), "/bin/risim");
    }

    #[test]
    fn rejects_other_programs() {
        for output in [
            "GHDL 6.4.0-risim (tarball) [simulation adapter]\n",
            "risim 0.1.0",
            " risim-runner 0.1.0",
            "",
        ] {
            Simulator::from_identity(simulator_identity(output)).unwrap_err();
        }
    }

    #[test]
    fn only_vhdl_2008_is_supported() {
        assert_eq!(check_standard(VhdlStandard::Vhdl2008), Ok(()));
        for standard in [
            VhdlStandard::Vhdl1993,
            VhdlStandard::Vhdl2002,
            VhdlStandard::Vhdl2019,
        ] {
            let error = check_standard(standard).unwrap_err();
            assert_eq!(error, CommandError::UnsupportedStandard { standard });
            assert_eq!(
                error.to_string(),
                format!("risim supports only VHDL-2008, not VHDL-{standard}")
            );
        }
    }

    #[test]
    fn compile_command() {
        let library_dirs = vec![
            Utf8PathBuf::from("/out/libraries/vunit_lib"),
            Utf8PathBuf::from("/out/libraries/lib"),
        ];
        let mut args = CompileArgs {
            library: "lib",
            library_dir: Utf8Path::new("/out/libraries/lib"),
            vhdl_standard: VhdlStandard::Vhdl2008,
            library_dirs: &library_dirs,
            flags: &["-frelaxed".to_owned()],
            files: &[Utf8Path::new("/src/a.vhd"), Utf8Path::new("/src/b.vhd")],
        };
        assert_eq!(
            simulator().compile_command(&args).unwrap(),
            [
                "/bin/risim",
                "analyze",
                "--work=lib",
                "--workdir=/out/libraries/lib",
                "-L",
                "/out/libraries/vunit_lib",
                "-L",
                "/out/libraries/lib",
                "--message-format=json",
                "-frelaxed",
                "/src/a.vhd",
                "/src/b.vhd",
            ]
        );
        args.vhdl_standard = VhdlStandard::Vhdl2019;
        assert_eq!(
            simulator().compile_command(&args),
            Err(CommandError::UnsupportedStandard {
                standard: VhdlStandard::Vhdl2019
            })
        );
    }

    #[test]
    fn simulate_command() {
        let library_dirs = vec![
            Utf8PathBuf::from("/out/libraries/vunit_lib"),
            Utf8PathBuf::from("/out/libraries/lib"),
        ];
        let generics = BTreeMap::from([
            ("a".to_owned(), "1".to_owned()),
            ("runner_cfg".to_owned(), "x : y,z \"q\"".to_owned()),
        ]);
        let simulator = simulator();
        let mut args = SimulateArgs {
            vhdl_standard: VhdlStandard::Vhdl2008,
            library: "lib",
            library_dir: Utf8Path::new("/out/libraries/lib"),
            library_dirs: &library_dirs,
            elab_flags: &["-frelaxed".to_owned(), "-fsynopsys".to_owned()],
            top: &Top::Entity {
                entity: "tb".to_owned(),
                architecture: "a".to_owned(),
            },
            sim_flags: &["--finish-after=1ms".to_owned()],
            generics: &generics,
            assert_level: AssertLevel::Error,
            disable_ieee_asserts: true,
            wait: true,
            name: "lib.tb.test",
        };
        assert_eq!(
            simulator.simulate_command(&args).unwrap(),
            [
                "/bin/risim",
                "run",
                "--work=lib",
                "--workdir=/out/libraries/lib",
                "-L",
                "/out/libraries/vunit_lib",
                "-L",
                "/out/libraries/lib",
                "-frelaxed",
                "-fsynopsys",
                "--generic=a=1",
                "--generic=runner_cfg=x : y,z \"q\"",
                "--assert-level=error",
                "--ieee-asserts=disable",
                "--paused",
                "--name=lib.tb.test",
                "--finish-after=1ms",
                "tb(a)",
            ]
        );

        let configuration = Top::Configuration("cfg".to_owned());
        args.top = &configuration;
        args.assert_level = AssertLevel::Failure;
        args.disable_ieee_asserts = false;
        args.elab_flags = &[];
        args.sim_flags = &[];
        args.wait = false;
        let command = simulator.simulate_command(&args).unwrap();
        assert_eq!(
            command[8..],
            [
                "--generic=a=1",
                "--generic=runner_cfg=x : y,z \"q\"",
                "--assert-level=failure",
                "--name=lib.tb.test",
                "cfg",
            ]
        );

        args.vhdl_standard = VhdlStandard::Vhdl1993;
        assert_eq!(
            simulator.simulate_command(&args),
            Err(CommandError::UnsupportedStandard {
                standard: VhdlStandard::Vhdl1993
            })
        );
    }
}
