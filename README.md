<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this file,
You can obtain one at http://mozilla.org/MPL/2.0/.

AI NOTICE: Generated, minimally reviewed.
-->

# risim-vunit-frontend

A Rust replacement for the Python frontend of [VUnit](https://github.com/VUnit/vunit), built for the riSim simulator.

This is a fork of VUnit. The VHDL libraries under `vunit/vhdl` (the testbench runner, logging, checks, data types, com, and verification components) are unchanged upstream sources, apart from fixes to a few of their testbenches, and OSVVM 2023.04, which upstream no longer bundles, is a submodule of the fork ([`UPSTREAM.md`](UPSTREAM.md)). The Python frontend is replaced by this crate, so existing VUnit testbenches run as they are and need no Python at build time or at runtime.

## Scope

The scope of this crate is intentionally limited:

- riSim has two executables: the riSim CLI `risim` analyses all libraries with `risim analyze`, and the runner `risim-runner` simulates with `risim-runner run`.
  `risim-runner` simulates VHDL-2008 only.
- VHDL first. SystemVerilog and Verilog files (`.sv`, `.v`, `.vo`) compile, but don't simulate: they are scanned for their design elements, imports, and includes, and compiled with VUnit's SystemVerilog package and include directory, which projects with such files get; their testbenches aren't run, and get a warning. Include files (`.svh`, `.vh`) are read only where a file includes them, and encrypted and Verilog-AMS files are skipped with a warning.
- There is no command-line interface. Callers open a `Runtime` and drive workspaces through the library API.

The crate provides:

- the project model: libraries, source globs, VHDL parsing and SystemVerilog scanning, dependency analysis and compile order;
- test discovery, with VUnit's testbench conventions (`runner_cfg`, `run("…")`, `-- vunit:` attributes, configurations and generics);
- incremental, parallel compilation, with recompilation decided by content hashes instead of timestamps;
- simulation runs, one simulation per test, with results persisted in `risim-out/`;
- workspaces that watch their sources, merge requests, support cancellation, and report progress and diagnostics as events.

[`UPSTREAM.md`](UPSTREAM.md) lists every intentional deviation from VUnit's behaviour, and maps each Rust module to the Python code it was ported from.

## Usage

A workspace is opened from a `risim-config.toml`:

```toml
# The options of the project: relaxed analysis and elaboration, the optimization preset, and rows
# or groups of optimizations over it; then the options of SystemVerilog and Verilog files, and the
# encoding of all files. Only tables follow the header.
[options]
relaxed = true
opt-level = "full"
include-dirs = ["inc"]
defines = { WIDTH = "8", DEBUG = true, "MAX(a,b)" = "((a)>(b)?(a):(b))" }
encoding = "utf-8"

[options.opt]
wake-guards = "off"
"mir.cse" = "off"

# Optional VUnit features on top of the defaults (VUnit core, com, OSVVM).
[vunit]
features = ["random", "verification_components"]

# One table per library, with source globs relative to the workspace root, and its own options.
[libraries.my_lib]
files = ["src/**/*.vhd", "tb/*.vhd"]
options = { defines = { DEBUG = false }, keywords = "1800-2017" }

# The riSim version that the `risim` launcher runs for the workspace; ignored here.
[toolchain]
version = "1.4"
```

`[options]` holds the options of the project, which the `config` module turns into `ProjectSpec::options` for analysis and into run options of every simulation (riSim's `risim-run-options`):

- `relaxed = true` is `--relaxed` for analysis (`Options::analyze_args`) and the run option `relaxed` for elaboration; `relaxed = false` is the same as no key.
- `opt-level` is the run option `opt-level`, a preset name or its number.
- Each key `NAME` of `[options.opt]` (or `opt = { … }`) is the run option `opt.NAME`, with a string mode.
  A dotted name is quoted (`"mir.cse" = "off"`); a table in `[options.opt]`, such as `mir.cse = "off"` or `[options.opt.mir]`, is an error that names the quoted form.

- `include-dirs` is a list of directories, relative to the workspace root, that a quoted `` `include `` searches; a directory that doesn't exist gives a warning.
- `defines` is a table of macros, defined at the start of every compilation unit: a name, optionally with formal arguments (`"MAX(a,b)" = "…"`), and its text, or `true` for an empty macro.
  A macro named like a directive, and an integer as text, are errors.
- `compilation-unit` is `"file"` (the default: each file is a compilation unit) or `"library"` (the files of a library form one, in the order of collection).
- `keywords` is a version specifier of `` `begin_keywords `` (IEEE 1800 22.14), such as `"1800-2017"`, which replaces the default of the file's extension.
- `encoding` is `"utf-8"` or `"latin-1"`, the encoding of the library's files of both languages and of their include files; by default, VHDL files are Latin-1 and the others UTF-8.

The table can also be written inline, `options = { relaxed = true }`, which avoids the TOML pitfall that bare and dotted keys after the `[options]` header belong to it.

`[libraries.NAME.options]` holds a library's own options, over the project's (`Options::for_library`): a key of the library replaces the project's, except `defines`, which merge by name, and where `false` removes the project's macro of that name.
`relaxed`, `opt-level`, and `opt` are project level only.
`sv-libraries` is library level only: the libraries, in order, that the library's SystemVerilog files search for packages and definitions after their own library.
It can't name the library itself, and accepts `vunit_lib`, a library of every project.
Without it, the libraries are derived from the packages that the library's files import or refer to, and the interfaces they use, that other libraries define, in the order of their first use; `vunit_lib` is added last either way, so naming it is optional.
Libraries that depend on each other in a cycle are a project error, which names the uses that formed it, and nothing is compiled; an explicit list breaks it.
Instead of upstream's `include_dirs` and `defines` per file (`add_source_file`), these options are per library, as `risim analyze` takes them per call.
The builtin libraries take the project's `relaxed` only (`Options::for_builtins`), and the files of a user library `vunit_lib` join VUnit's library and take its options, so `[libraries.vunit_lib.options]` has no valid keys.
`Project::analyze_args` gives every library its arguments, for the files of both languages: a library without SystemVerilog or Verilog files gets `relaxed` and `encoding` only (`Options::without_sv_keys`), so that the other keys don't change its compile keys, and one with such files also gets the derived or listed libraries as `--sv-library`.

Unknown keys and wrong types in the options tables are configuration errors at their keys or values; the rest of the file still loads.
The former list `options = ["-frelaxed"]` and the former `[run]` table are errors that give the new spelling, and nothing of them is applied.
`RuntimeOptions::check_run_options` checks each run key of `[options]` when the file is read, as riSim's hub does with `risim_run_options::validate_partial`; a key that it rejects is a configuration error and is left out, and a rejected `relaxed` doesn't make the analysis relaxed either.
`[toolchain] version` is read by the riSim launcher, which selects the toolchain by it (riSim's `doc/installation.md`); this crate accepts it without a warning and otherwise ignores it.
Unknown keys outside the options tables, including other keys in `[toolchain]`, are warnings.

A `ProjectSpec` built in code describes the same project, and can also set external libraries, a VHDL standard per library, and configurations, generics and simulation options per testbench or test. Pass it as `ProjectSource::Spec` instead of `ProjectSource::ConfigFile`.
Such a spec sets the run option `relaxed` next to `Options::relaxed` for relaxed elaboration, as reading `relaxed = true` does.

```rust
use std::num::NonZeroU32;
use std::sync::Arc;

use risim_vunit_frontend::{ProjectSource, Runtime, RuntimeOptions, SimulationRequest, ThreadBudget};
use risim_vunit_frontend::threads::NoLeases;

let runtime = Runtime::new(RuntimeOptions {
    analyzer: "/path/to/risim".into(),
    runner: "/path/to/risim-runner".into(),
    run_defaults: Default::default(),
    check_run_options: None,
    threads: ThreadBudget::new(threads),
    max_threads_per_test: NonZeroU32::MIN,
    max_parallel_compiles: parallelism,
    leases: Arc::new(NoLeases),
    lock_owner: owner,
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
    None,
);
while let Some(event) = receiver.recv().await {
    // CompileStarted, FileCompiling, TestStarted, TestFinished, SimulationFinished, …
}
```

Everything a workspace generates goes to `<root>/risim-out/`: the extracted VUnit and OSVVM sources, compiled libraries, compile and test output, and the JSON state files.
A lock file keeps other processes from using the same directory at the same time, and `owner.json` names the process that holds it (`RuntimeOptions::lock_owner`).
A workspace whose `risim-out/` is deleted or replaced while it is open cancels its running operations and writes nothing more to it; its next operation locks the directory again and compiles everything (the `workspace` module documents the details).

Simulations share a budget of threads (`RuntimeOptions::threads`, resizable), and a simulate request can cap its own threads.
Each simulation takes a share of the free threads, up to the threads that it can use (`RuntimeOptions::max_threads_per_test`), and asks the `LeaseProvider` of the runtime for a lease of them, whose run options go over the run options of the configuration and the defaults, but not over the keys that every simulation sets; a test waiting for threads reports it as `TestThreads`.
The `threads` module documents the rules.

A simulation runs `risim-runner run <options>`, with its options as one JSON object, whose keys are the long option names of `risim run` (riSim's `risim-run-options`).
The object has the keys that every simulation sets (`simulator::SIMULATION_KEYS`: the libraries, the top-level unit, the generics, the assertion and GUI options, and the testcase name), the run options of the configuration (`SimOptions::run_options`, such as the run keys `relaxed`, `opt-level`, and `opt` of the `[options]` of `risim-config.toml`), and the defaults of the runtime that these don't set (`RuntimeOptions::run_defaults`, such as those that riSim's hub takes from its environment).
The run options go over the defaults by the layer rule of riSim's `risim-run-options` (`simulator::overlay_run_options`), and those of a test configuration over the project's by the same rule: a later layer's `opt-level` drops the `opt.*` keys of the earlier ones, and its `opt.<name>` the keys `opt.<name>.*`.
The run options of a `SimulationRequest` (`options`, such as the optimizations that riSim's GUI chooses) go over those of the configuration by the same rule.
Simulates whose requests have different run options don't merge, and a testcase that several requests with different run options match runs once, with those of the first, and a warning.
Run options that set a key of the simulation fail the test.
The runner names the format of the object in `--version` (`run options format N`); a runner that reads another format than `simulator::RUN_OPTIONS_FORMAT`, or names none, can't be used, and `Runtime::new` fails.

`Runtime::simulation_limit` returns the shared budget of threads as a `SimulationLimit`, so that a simulation that the application runs outside of a workspace, such as a standalone run of riSim's hub, waits for it like a testcase; `Runtime::leases` grants it its lease.

The compile set is what the testbenches need, and every SystemVerilog and Verilog file, since their testbenches don't run yet.
The consecutive files of one library that need compiling are analysed by one `risim analyze`, with the library's arguments, which prints its reports as JSON lines (`--message-format=json`); a library's batches wait for the libraries of its `--sv-library` arguments.
Each report goes to the file of its location, or, for one in an include file, to the file of the batch that includes it, while keeping its location; a file whose design units the call made obsolete is compiled again.
A file is compiled again when its compile key changes, a hash of its contents, the contents of the files its includes found (or that they found none), its library's arguments, the simulator, and the keys of its dependencies.
The SystemVerilog and Verilog files of a library with `compilation-unit = "library"` form one compile item, with one key: a change of one of them compiles all of them, in one call, in the order of collection.
The compiled libraries are kept as long as the paths of both executables and the `--version` output of `risim-runner` are the same, which names the archive format.

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
- The `acceptance` tests run VUnit's acceptance projects, the test benches of the VUnit VHDL libraries and a few fixtures of riSim's own with a real simulator; VUnit's Verilog acceptance testbenches are compiled only. They are skipped unless `RISIM_RUNNER` names a risim-runner executable and `RISIM_ANALYZER` a `risim` executable. Testcases that fail on risim are listed per trial, with the reason:

  ```sh
  RISIM_RUNNER=/path/to/risim-runner RISIM_ANALYZER=/path/to/risim cargo test --test acceptance
  ```

  The runners get a free plan of their own: as many threads as logical cores (`RISIM_FREE_THREADS`), in a temporary slot directory (`RISIM_SLOT_DIR`), so that they neither queue for the free plan's 2 threads nor wait for or take the slots of other riSim processes on the machine.
  Only debug builds of risim-runner honor these variables; with a release build, the trials run 2 simulations at a time, shared with the machine, and may time out.

## License

The Rust code and VUnit's VHDL libraries are licensed under the [Mozilla Public License, v. 2.0](LICENSE.rst). OSVVM (`vunit/vhdl/osvvm`) is redistributed under the Apache License, v. 2.0; see [`LICENSE.rst`](LICENSE.rst).

VUnit is © 2014-2026 Lars Asplund and contributors. OSVVM is © SynthWorks Design Inc.
