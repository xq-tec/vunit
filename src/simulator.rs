// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! The simulator and its command lines.
//!
//! riSim has two executables: the analyzer, the riSim CLI `risim`, analyses every library with
//! `risim analyze`, into the archive `<library dir>/<library>.hlib`, and the runner simulates with
//! `risim-runner run`, which reads the same archives. They support VHDL-2008 only.
//!
//! The runner takes the options of a simulation as one JSON object ([`RunOptions`]), whose
//! format the runner names in `--version` (`run options format N`); [`Simulator::detect`] rejects
//! a runner that reads another format than [`RUN_OPTIONS_FORMAT`].
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
use serde_json::Value;
use serde_json::json;
use thiserror::Error;

use crate::process;
use crate::spec::AssertLevel;
use crate::vhdl_standard::VhdlStandard;

/// The format of the run options that [`Simulator::simulate_command`] writes, their key `format`.
///
/// risim-runner names the format that it reads in its `--version` output, on the line
/// `run options format N`.
pub const RUN_OPTIONS_FORMAT: u32 = 2;

/// Options of risim-runner, as keys and values of the JSON object of a run, such as
/// `{"relaxed": true}`.
///
/// The keys are the long option names of `risim run`, as riSim's `risim-run-options` describes
/// them.
pub type RunOptions = serde_json::Map<String, Value>;

/// A check of run options that are one layer of a run's options, not all of them, such as riSim's
/// `risim_run_options::validate_partial`; it returns the message of the first problem.
pub type CheckRunOptions = fn(&RunOptions) -> Result<(), String>;

/// The key of the optimization preset in the run options.
const OPT_LEVEL_KEY: &str = "opt-level";

/// The prefix of the keys of the run options that set an optimization row or group, such as
/// `opt.wake-guards`.
const OPT_PREFIX: &str = "opt.";

/// Sets the keys of the run options `layer` in `options`, the keys of an earlier layer.
///
/// A key of `layer` replaces the same key of `options`; besides, as in riSim's
/// `risim-run-options`, a layer that sets `opt-level` drops the `opt.*` keys of `options`, and a
/// layer that sets `opt.<name>` drops the keys `opt.<name>.*` of `options`, the rows of the group
/// `<name>`. So a preset or a group of a later layer isn't undone by a row of an earlier one.
pub fn overlay_run_options(options: &mut RunOptions, layer: &RunOptions) {
    if layer.contains_key(OPT_LEVEL_KEY) {
        options.retain(|key, _| !key.starts_with(OPT_PREFIX));
    }
    for key in layer.keys().filter(|key| key.starts_with(OPT_PREFIX)) {
        let rows = format!("{key}.");
        options.retain(|earlier, _| !earlier.starts_with(&rows));
    }
    options.extend(layer.clone());
}

/// The keys of the run options that [`Simulator::simulate_command`] sets for every simulation, so
/// that the run options of a configuration can't set them.
pub const SIMULATION_KEYS: [&str; 10] = [
    "format",
    "work",
    "workdir",
    "library-paths",
    "top",
    "generics",
    "assert-level",
    "ieee-asserts",
    "paused",
    "name",
];

/// What identifies a riSim installation: the runner, the analyzer, and the runner's `--version`.
///
/// Libraries compiled with another identity are discarded. The `--version` output names the
/// archive format, so a rebuilt risim-runner with the same format keeps the libraries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimulatorIdentity {
    /// The runner executable, which simulates.
    pub path: Utf8PathBuf,
    /// The analyzer executable, which analyses.
    ///
    /// State stored before the analyzer had its own path loads with an empty one, so that its
    /// libraries are compiled again.
    #[serde(default)]
    pub analyzer: Utf8PathBuf,
    /// The output of the runner's `--version`.
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
    /// risim-runner reads another format of run options than [`RUN_OPTIONS_FORMAT`].
    #[error(
        "{path} reads run options format {format}, but this build writes format {RUN_OPTIONS_FORMAT}"
    )]
    Format {
        /// The executable.
        path: Utf8PathBuf,
        /// The format that `--version` names.
        format: String,
    },
    /// risim-runner names no format of run options in `--version`, as one from before the JSON
    /// object does.
    #[error(
        "{path} names no run options format in `--version`, so it reads none, but this build \
         writes format {RUN_OPTIONS_FORMAT}"
    )]
    NoFormat {
        /// The executable.
        path: Utf8PathBuf,
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
    /// The run options of a configuration set a key that every simulation sets
    /// ([`SIMULATION_KEYS`]).
    #[error("the run options can't set `{key}`, which every simulation sets itself")]
    SimulationKey {
        /// The key.
        key: &'static str,
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

/// A detected risim-runner executable, with the analyzer that compiles for it; cheap enough to
/// clone into every simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simulator {
    identity: SimulatorIdentity,
    version: Version,
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::unwrap_used, reason = "the regex is a valid constant")]
    Regex::new(r"^risim-runner ([0-9]+)\.([0-9]+)").unwrap()
});

/// The line of the `--version` output that names the format of the run options, before the
/// number.
const RUN_OPTIONS_FORMAT_LINE: &str = "run options format ";

/// Checks that the `--version` output `output` of the runner `path` names the format of the run
/// options that this build writes, [`RUN_OPTIONS_FORMAT`].
fn check_run_options_format(path: &Utf8Path, output: &str) -> Result<(), DetectError> {
    let format = output
        .lines()
        .find_map(|line| line.strip_prefix(RUN_OPTIONS_FORMAT_LINE))
        .map(str::trim);
    match format {
        Some(format) if format.parse() == Ok(RUN_OPTIONS_FORMAT) => Ok(()),
        Some(format) => Err(DetectError::Format {
            path: path.to_owned(),
            format: format.to_owned(),
        }),
        None => Err(DetectError::NoFormat {
            path: path.to_owned(),
        }),
    }
}

/// The major and minor version in the `--version` output `output`.
fn parse_version(output: &str) -> Option<Version> {
    let captures = VERSION_RE.captures(output)?;
    Some(Version {
        major: captures.get(1)?.as_str().parse().ok()?,
        minor: captures.get(2)?.as_str().parse().ok()?,
    })
}

impl Simulator {
    /// Runs `<runner> --version` and records the identity of the runner and the analyzer
    /// `analyzer`.
    ///
    /// The analyzer isn't run: a missing one fails the compiles that need it.
    ///
    /// # Errors
    ///
    /// Fails if the runner can't be run, isn't risim-runner, or reads another format of run
    /// options than [`RUN_OPTIONS_FORMAT`].
    pub async fn detect(runner: &Utf8Path, analyzer: &Utf8Path) -> Result<Self, DetectError> {
        let output = process::command(runner, ["--version"], None)
            .output()
            .await
            .map_err(|source| DetectError::Io {
                path: runner.to_owned(),
                source,
            })?;
        let mut version_output = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            version_output.push_str(&String::from_utf8_lossy(&output.stderr));
            return Err(DetectError::Version {
                path: runner.to_owned(),
                output: version_output,
            });
        }
        let identity = SimulatorIdentity {
            path: runner.to_owned(),
            analyzer: analyzer.to_owned(),
            version_output,
        };
        let simulator = Self::from_identity(identity).map_err(|identity| DetectError::Version {
            path: runner.to_owned(),
            output: identity.version_output,
        })?;
        check_run_options_format(runner, &simulator.identity.version_output)?;
        Ok(simulator)
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

    /// The runner executable.
    pub fn path(&self) -> &Utf8Path {
        &self.identity.path
    }

    /// The analyzer executable.
    pub fn analyzer(&self) -> &Utf8Path {
        &self.identity.analyzer
    }

    /// The version.
    pub const fn version(&self) -> Version {
        self.version
    }

    /// The command line of the analyzer analysing files into one library, which prints the
    /// reports as JSON lines.
    ///
    /// # Errors
    ///
    /// Fails if the standard isn't supported.
    pub fn compile_command(&self, args: &CompileArgs<'_>) -> Result<Vec<String>, CommandError> {
        check_standard(args.vhdl_standard)?;
        let mut command = vec![
            self.analyzer().to_string(),
            "analyze".to_owned(),
            format!("--work={}", args.library),
            format!("--workdir={}", args.library_dir),
        ];
        for dir in args.library_dirs {
            command.extend(["-L".to_owned(), dir.to_string()]);
        }
        command.push("--message-format=json".to_owned());
        command.extend(args.flags.iter().cloned());
        command.extend(args.files.iter().map(ToString::to_string));
        Ok(command)
    }

    /// The command line of the runner elaborating and running a testbench:
    /// `<runner> run <options>`, with the options of the simulation as one JSON object.
    ///
    /// The object has the keys [`SIMULATION_KEYS`], and the run options of the configuration over
    /// the defaults of the runtime ([`overlay_run_options`]).
    ///
    /// # Errors
    ///
    /// Fails if the standard isn't supported, or if the run options of the configuration set one
    /// of [`SIMULATION_KEYS`].
    pub fn simulate_command(&self, args: &SimulateArgs<'_>) -> Result<Vec<String>, CommandError> {
        check_standard(args.vhdl_standard)?;
        if let Some(&key) = SIMULATION_KEYS
            .iter()
            .find(|&&key| args.run_options.contains_key(key))
        {
            return Err(CommandError::SimulationKey { key });
        }
        let top = match args.top {
            Top::Configuration(name) => name.clone(),
            Top::Entity {
                entity,
                architecture,
            } => format!("{entity}({architecture})"),
        };
        let generics: Vec<String> = args
            .generics
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let ieee_asserts = if args.disable_ieee_asserts {
            "disable"
        } else {
            "enable"
        };
        let mut options = args.run_defaults.clone();
        overlay_run_options(&mut options, args.run_options);
        options.extend(
            [
                ("format", json!(RUN_OPTIONS_FORMAT)),
                ("work", json!(args.library)),
                ("workdir", json!(args.library_dir)),
                ("library-paths", json!(args.library_dirs)),
                ("top", json!(top)),
                ("generics", json!(generics)),
                ("assert-level", json!(args.assert_level.name())),
                ("ieee-asserts", json!(ieee_asserts)),
                ("paused", json!(args.wait)),
                ("name", json!(args.name)),
            ]
            .map(|(key, value)| (key.to_owned(), value)),
        );
        Ok(vec![
            self.path().to_string(),
            "run".to_owned(),
            Value::Object(options).to_string(),
        ])
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
    /// The directories of all libraries, the run option `library-paths`.
    pub library_dirs: &'a [Utf8PathBuf],
    /// The unit to elaborate.
    pub top: &'a Top,
    /// The run options of the configuration, which mustn't set [`SIMULATION_KEYS`].
    pub run_options: &'a RunOptions,
    /// The run options of every simulation, which the configuration's override
    /// ([`RuntimeOptions::run_defaults`](crate::RuntimeOptions::run_defaults)).
    pub run_defaults: &'a RunOptions,
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
        assert_eq!(simulator().path(), "/bin/risim-runner");
        assert_eq!(simulator().analyzer(), "/bin/risim");
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

    /// An identity stored before the analyzer had its own path loads with an empty one, which
    /// differs from every detected identity.
    #[test]
    fn identity_without_analyzer_loads() {
        let identity: SimulatorIdentity =
            serde_json::from_str(r#"{"path":"/bin/risim","version_output":"risim-runner 0.1.0"}"#)
                .unwrap();
        assert_eq!(identity.path, "/bin/risim");
        assert_eq!(identity.analyzer, "");
        assert_ne!(identity, simulator_identity("risim-runner 0.1.0"));
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

    /// The runner must name the format of the run options that this build writes.
    #[test]
    fn run_options_format_is_checked() {
        let path = Utf8Path::new("/bin/risim-runner");
        let version =
            |format: &str| format!("risim-runner 0.1.0\nhydrolyze archive format 4\n{format}");
        check_run_options_format(
            path,
            &version(&format!("run options format {RUN_OPTIONS_FORMAT}\n")),
        )
        .unwrap();
        let other = check_run_options_format(
            path,
            &version(&format!("run options format {}\n", RUN_OPTIONS_FORMAT + 1)),
        )
        .unwrap_err();
        assert_eq!(
            other.to_string(),
            format!(
                "/bin/risim-runner reads run options format {}, but this build writes format \
                 {RUN_OPTIONS_FORMAT}",
                RUN_OPTIONS_FORMAT + 1
            )
        );
        let none = check_run_options_format(path, &version("")).unwrap_err();
        assert!(matches!(none, DetectError::NoFormat { .. }), "{none}");
        assert!(
            none.to_string()
                .ends_with(&format!("writes format {RUN_OPTIONS_FORMAT}")),
            "{none}"
        );
    }

    /// A later layer replaces the same keys; its `opt-level` drops the earlier `opt.*` keys, and
    /// its `opt.<name>` the earlier `opt.<name>.*` keys, but not those of other names.
    #[test]
    fn overlay_run_options_by_the_layer_rule() {
        let overlaid = |earlier: &Value, layer: Value| {
            let (Value::Object(mut options), Value::Object(layer)) = (earlier.clone(), layer)
            else {
                unreachable!("objects");
            };
            overlay_run_options(&mut options, &layer);
            Value::Object(options)
        };
        let earlier = json!({
            "jit": "off",
            "opt-level": "full",
            "opt.wake-guards": "off",
            "opt.mir": "off",
            "opt.mir.cse": "on",
            "opt.mirror": "on",
        });
        assert_eq!(
            overlaid(&earlier, json!({"jit": "eager", "opt.wake-guards": "on"})),
            json!({
                "jit": "eager",
                "opt-level": "full",
                "opt.wake-guards": "on",
                "opt.mir": "off",
                "opt.mir.cse": "on",
                "opt.mirror": "on",
            })
        );
        assert_eq!(
            overlaid(&earlier, json!({"opt.mir": "on"})),
            json!({
                "jit": "off",
                "opt-level": "full",
                "opt.wake-guards": "off",
                "opt.mir": "on",
                "opt.mirror": "on",
            })
        );
        assert_eq!(
            overlaid(
                &earlier,
                json!({"opt-level": "off", "opt.collapse-signals": "on"})
            ),
            json!({"jit": "off", "opt-level": "off", "opt.collapse-signals": "on"})
        );
        // A layer's own keys stay, also those of a group and its rows.
        assert_eq!(
            overlaid(&json!({}), json!({"opt.mir": "off", "opt.mir.cse": "on"})),
            json!({"opt.mir": "off", "opt.mir.cse": "on"})
        );
    }

    /// The options of a simulation are one JSON object: the runtime's defaults, the
    /// configuration's run options over them, and the simulation's own keys.
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
        let run_options = RunOptions::from_iter([
            ("relaxed".to_owned(), json!(true)),
            ("finish-after".to_owned(), json!("1 ms")),
            ("opt-level".to_owned(), json!("off")),
        ]);
        let run_defaults = RunOptions::from_iter([
            ("jit".to_owned(), json!("off")),
            ("relaxed".to_owned(), json!(false)),
            ("opt.wake-guards".to_owned(), json!("off")),
        ]);
        let simulator = simulator();
        let mut args = SimulateArgs {
            vhdl_standard: VhdlStandard::Vhdl2008,
            library: "lib",
            library_dir: Utf8Path::new("/out/libraries/lib"),
            library_dirs: &library_dirs,
            top: &Top::Entity {
                entity: "tb".to_owned(),
                architecture: "a".to_owned(),
            },
            run_options: &run_options,
            run_defaults: &run_defaults,
            generics: &generics,
            assert_level: AssertLevel::Error,
            disable_ieee_asserts: true,
            wait: true,
            name: "lib.tb.test",
        };
        let options = |command: Vec<String>| {
            let [runner, run, options] = command.as_slice() else {
                panic!("not a runner, `run` and the options: {command:?}");
            };
            assert_eq!(
                (runner.as_str(), run.as_str()),
                ("/bin/risim-runner", "run")
            );
            serde_json::from_str::<Value>(options).unwrap()
        };
        assert_eq!(
            options(simulator.simulate_command(&args).unwrap()),
            json!({
                "format": RUN_OPTIONS_FORMAT,
                "work": "lib",
                "workdir": "/out/libraries/lib",
                "library-paths": ["/out/libraries/vunit_lib", "/out/libraries/lib"],
                "top": "tb(a)",
                "generics": ["a=1", "runner_cfg=x : y,z \"q\""],
                "assert-level": "error",
                "ieee-asserts": "disable",
                "paused": true,
                "name": "lib.tb.test",
                "relaxed": true,
                "finish-after": "1 ms",
                "opt-level": "off",
                "jit": "off",
            })
        );

        let configuration = Top::Configuration("cfg".to_owned());
        let no_options = RunOptions::new();
        args.top = &configuration;
        args.assert_level = AssertLevel::Failure;
        args.disable_ieee_asserts = false;
        args.run_options = &no_options;
        args.run_defaults = &no_options;
        args.wait = false;
        let options = options(simulator.simulate_command(&args).unwrap());
        assert_eq!(options["top"], "cfg");
        assert_eq!(options["assert-level"], "failure");
        assert_eq!(options["ieee-asserts"], "enable");
        assert_eq!(options["paused"], false);
        assert_eq!(options.as_object().unwrap().len(), SIMULATION_KEYS.len());

        for key in SIMULATION_KEYS {
            let setting_key = RunOptions::from_iter([(key.to_owned(), json!(true))]);
            let args = SimulateArgs {
                run_options: &setting_key,
                ..args.clone()
            };
            assert_eq!(
                simulator.simulate_command(&args),
                Err(CommandError::SimulationKey { key })
            );
        }

        args.run_options = &no_options;
        args.vhdl_standard = VhdlStandard::Vhdl1993;
        assert_eq!(
            simulator.simulate_command(&args),
            Err(CommandError::UnsupportedStandard {
                standard: VhdlStandard::Vhdl1993
            })
        );
    }
}
