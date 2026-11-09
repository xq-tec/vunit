<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this file,
You can obtain one at http://mozilla.org/MPL/2.0/.

AI NOTICE: Generated, minimally reviewed.
-->

# risim-vunit-frontend

A Rust replacement for the Python frontend of [VUnit](https://github.com/VUnit/vunit), built for the riSim simulator.

This is a fork of VUnit. The VHDL libraries under `vunit/vhdl` (the testbench runner, logging, checks, data types, com, verification components and OSVVM) are unchanged upstream sources, apart from fixes to a few of their testbenches ([`UPSTREAM.md`](UPSTREAM.md)). The Python frontend is replaced by this crate, so existing VUnit testbenches run as they are and need no Python at build time or at runtime.

## Scope

The scope of this crate is intentionally limited:

- riSim has two executables: the riSim CLI `risim` analyses all libraries with `risim analyze`, and the runner `risim-runner` simulates with `risim-runner run`.
  They support VHDL-2008 only.
- VHDL only. Verilog is not supported yet.
- There is no command-line interface. Callers open a `Runtime` and drive workspaces through the library API.

The crate provides:

- the project model: libraries, source globs, VHDL parsing, dependency analysis and compile order;
- test discovery, with VUnit's testbench conventions (`runner_cfg`, `run("…")`, `-- vunit:` attributes, configurations and generics);
- incremental, parallel compilation, with recompilation decided by content hashes instead of timestamps;
- simulation runs, one simulation per test, with results persisted in `risim-out/`;
- workspaces that watch their sources, merge requests, support cancellation, and report progress and diagnostics as events.

[`UPSTREAM.md`](UPSTREAM.md) lists every intentional deviation from VUnit's behaviour, and maps each Rust module to the Python code it was ported from.

## Usage

A workspace is opened from a `risim-config.toml`. The `options` list is passed both to analysis (`risim analyze`) and to elaboration (`risim-runner run`). It accepts `--relaxed`, its alias `-frelaxed`, and `-fsynopsys`, which has no effect; any other option is a configuration error:

```toml
# Options of analysis and elaboration.
options = ["-frelaxed"]

# Options of the simulations: the optimization preset, and rows or groups over it.
[run]
opt-level = "full"

[run.opt]
wake-guards = "off"
"mir.cse" = "off"

# Optional VUnit features on top of the defaults (VUnit core, com, OSVVM).
[vunit]
features = ["random", "verification_components"]

# One table per library, with source globs relative to the workspace root.
[libraries.my_lib]
files = ["src/**/*.vhd", "tb/*.vhd"]

# The riSim version that the `risim` launcher runs for the workspace; ignored here.
[toolchain]
version = "1.4"
```

`[toolchain] version` is read by the riSim launcher, which selects the toolchain by it (riSim's `doc/installation.md`); this crate accepts it without a warning and otherwise ignores it.
Unknown keys, including other keys in `[toolchain]` and `[run]`, are warnings.

`[run]` sets run options of every simulation: `opt-level` the run option `opt-level`, and each key `NAME` of `[run.opt]` the run option `opt.NAME` (riSim's `risim-run-options`).
The values are strings.
A dotted name is quoted (`"mir.cse" = "off"`); a table in `[run.opt]`, such as `mir.cse = "off"` or `[run.opt.mir]`, is an error that names the quoted form.
`RuntimeOptions::check_run_options` checks each of these keys when the file is read, as riSim's hub does with `risim_run_options::validate_partial`; a key that it rejects is a configuration error and is left out.

A `ProjectSpec` built in code describes the same project, and can also set external libraries, a VHDL standard per library, and configurations, generics and simulation options per testbench or test. Pass it as `ProjectSource::Spec` instead of `ProjectSource::ConfigFile`.

```rust
use risim_vunit_frontend::{ProjectSource, Runtime, RuntimeOptions, SimulationRequest};

let runtime = Runtime::new(RuntimeOptions {
    analyzer: "/path/to/risim".into(),
    runner: "/path/to/risim-runner".into(),
    run_defaults: Default::default(),
    check_run_options: None,
    max_parallel_simulations: parallelism,
    max_parallel_compiles: parallelism,
})
.await?;
let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
let workspace = runtime
    .open_workspace(root, ProjectSource::ConfigFile("risim-config.toml".into()), events)
    .await?;
workspace.simulate(
    vec![SimulationRequest { pattern: "my_lib.tb_*".into(), gui: false, ..Default::default() }],
    None,
    None,
);
while let Some(event) = receiver.recv().await {
    // CompileStarted, FileCompiling, TestStarted, TestFinished, SimulationFinished, …
}
```

Everything a workspace generates goes to `<root>/risim-out/`: the extracted VUnit and OSVVM sources, compiled libraries, compile and test output, and the JSON state files.
A lock file keeps other processes from using the same directory at the same time, and `owner.json` names the process that holds it (`RuntimeOptions::lock_owner`).

A simulation runs `risim-runner run <options>`, with its options as one JSON object, whose keys are the long option names of `risim run` (riSim's `risim-run-options`).
The object has the keys that every simulation sets (`simulator::SIMULATION_KEYS`: the libraries, the top-level unit, the generics, the assertion and GUI options, and the testcase name), the run options of the configuration (`SimOptions::run_options`, such as `relaxed` from the `options` of `risim-config.toml` and the keys of its `[run]`), and the defaults of the runtime that these don't set (`RuntimeOptions::run_defaults`, such as those that riSim's hub takes from its environment).
The run options go over the defaults by the layer rule of riSim's `risim-run-options` (`simulator::overlay_run_options`), and those of a test configuration over the project's by the same rule: a later layer's `opt-level` drops the `opt.*` keys of the earlier ones, and its `opt.<name>` the keys `opt.<name>.*`.
The run options of a `SimulationRequest` (`options`, such as the optimizations that riSim's GUI chooses) go over those of the configuration by the same rule.
Simulates whose requests have different run options don't merge, and a testcase that several requests with different run options match runs once, with those of the first, and a warning.
Run options that set a key of the simulation fail the test.
The runner names the format of the object in `--version` (`run options format N`); a runner that reads another format than `simulator::RUN_OPTIONS_FORMAT`, or names none, can't be used, and `Runtime::new` fails.

`Runtime::simulation_limit` returns the shared limit of parallel simulations (`max_parallel_simulations`) as a `ProcessLimit`, so that a simulation that the application runs outside of a workspace, such as a standalone run of riSim's hub, waits for it like a testcase.

The consecutive files of one library that need compiling are analysed by one `risim analyze`, which prints its reports as JSON lines (`--message-format=json`); each report goes to the file of its location, and a file whose design units the call made obsolete is compiled again. The compiled libraries are kept as long as the paths of both executables and the `--version` output of `risim-runner` are the same, which names the archive format.

## Building

The OSVVM submodule is needed:

```sh
git submodule update --init --recursive
cargo build
```

## Tests

```sh
cargo test
```

- The unit tests and the `operations` integration tests need no simulator. The `operations` test binary doubles as a fake risim-runner. Its trial `real_risim_compiles_uart` compiles VUnit's UART example with the risim-runner named by `RISIM_RUNNER` and the analyzer (`risim`) named by `RISIM_ANALYZER`, and is skipped without either.
- The `acceptance` tests run VUnit's acceptance projects, the test benches of the VUnit VHDL libraries and a few fixtures of riSim's own with a real simulator. They are skipped unless `RISIM_RUNNER` names a risim-runner executable and `RISIM_ANALYZER` a `risim` executable. Testcases that fail on risim are listed per trial, with the reason:

  ```sh
  RISIM_RUNNER=/path/to/risim-runner RISIM_ANALYZER=/path/to/risim cargo test --test acceptance
  ```

## License

The Rust code and VUnit's VHDL libraries are licensed under the [Mozilla Public License, v. 2.0](LICENSE.rst). OSVVM (`vunit/vhdl/osvvm`) is redistributed under the Apache License, v. 2.0; see [`LICENSE.rst`](LICENSE.rst).

VUnit is © 2014-2026 Lars Asplund and contributors. OSVVM is © SynthWorks Design Inc.
