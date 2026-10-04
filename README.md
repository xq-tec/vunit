<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this file,
You can obtain one at http://mozilla.org/MPL/2.0/.

AI NOTICE: Generated, minimally reviewed.
-->

# risim-vunit-frontend

A Rust replacement for the Python frontend of [VUnit](https://github.com/VUnit/vunit), built for the riSim simulator.

This is a fork of VUnit. The VHDL libraries under `vunit/vhdl` (the testbench runner, logging, checks, data types, com, verification components and OSVVM) are unchanged upstream sources. The Python frontend is replaced by this crate, so existing VUnit testbenches run as they are and need no Python at build time or at runtime.

## Scope

The crate covers what riSim needs, and nothing more:

- The only supported simulator is `risim-ghdl`, riSim's build of GHDL.
- VHDL only. Verilog is not supported yet.
- There is no command-line interface. The crate is a library used by riSim's event-cache, which drives it over its control protocol; riSim's `risim-vunit` CLI is a client of that protocol.

The crate provides:

- the project model: libraries, source globs, VHDL parsing, dependency analysis and compile order;
- test discovery, with VUnit's testbench conventions (`runner_cfg`, `run("…")`, `-- vunit:` attributes, configurations and generics);
- incremental, parallel compilation, with recompilation decided by content hashes instead of timestamps;
- simulation runs, one simulation per test, with results persisted in `risim-out/`;
- workspaces that watch their sources, merge requests, support cancellation, and report progress and diagnostics as events.

[`UPSTREAM.md`](UPSTREAM.md) lists every intentional deviation from VUnit's behaviour, and maps each Rust module to the Python code it was ported from.

## Usage

A workspace is opened from a `risim-config.toml`:

```toml
# Flags for analysis and elaboration.
options = ["-fsynopsys", "-frelaxed"]

# Optional VUnit features on top of the defaults (VUnit core, com, OSVVM).
[vunit]
features = ["random", "verification_components"]

# One table per library, with source globs relative to the workspace root.
[libraries.my_lib]
files = ["src/**/*.vhd", "tb/*.vhd"]
```

Embedders can also build a `ProjectSpec` in code, which additionally supports configurations, generics and simulation options per testbench or test:

```rust
use risim_vunit_frontend::{ProjectSource, Runtime, RuntimeOptions, SimulationRequest};

let runtime = Runtime::new(RuntimeOptions {
    risim_ghdl: "/path/to/risim-ghdl".into(),
    max_parallel_simulations: parallelism,
    max_parallel_compiles: parallelism,
})
.await?;
let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
let workspace = runtime
    .open_workspace(root, ProjectSource::ConfigFile("risim-config.toml".into()), events)
    .await?;
workspace.simulate(
    vec![SimulationRequest { pattern: "my_lib.tb_*".into(), gui: false }],
    None,
);
while let Some(event) = receiver.recv().await {
    // CompileStarted, FileCompiling, TestStarted, TestFinished, SimulationFinished, …
}
```

Everything a workspace generates goes to `<root>/risim-out/`: the extracted VUnit and OSVVM sources, compiled libraries, compile and test output, and the JSON state files. A lock file keeps other processes from using the same directory at the same time.

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

- The unit tests and the `operations` integration tests need no simulator. The `operations` test binary doubles as a fake `risim-ghdl`.
- The `acceptance` tests run VUnit's acceptance projects and the test benches of the VUnit VHDL libraries with a real simulator. They are skipped unless `RISIM_GHDL` names a `risim-ghdl` executable:

  ```sh
  RISIM_GHDL=/path/to/risim-ghdl cargo test --test acceptance
  ```

## License

The Rust code and VUnit's VHDL libraries are licensed under the [Mozilla Public License, v. 2.0](LICENSE.rst). OSVVM (`vunit/vhdl/osvvm`) is redistributed under the Apache License, v. 2.0; see [`LICENSE.rst`](LICENSE.rst).

VUnit is © 2014-2026 Lars Asplund and contributors. OSVVM is © SynthWorks Design Inc.
