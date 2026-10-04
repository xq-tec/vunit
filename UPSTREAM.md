<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this file,
You can obtain one at http://mozilla.org/MPL/2.0/.

AI NOTICE: Generated, minimally reviewed.
-->

# Upstream sync

The VHDL and Verilog trees match upstream VUnit commit `44736ccfb1a89ffdc53638a2bc42a33e9f02eb46` (`Start of next release candidate 5.0.0.dev14`, 2026-09-26).

The fork adds `2ad0add1c51798c4c7d117e585125221ea337ff7`, which rewrites the OSVVM submodule URL from the relative `../../OSVVM/OSVVM.git` to `https://github.com/OSVVM/OSVVM`. Keep that URL on later syncs.

## Sync procedure

1. Merge upstream `master` into the fork.
2. Keep `vunit/vhdl/**` and `vunit/verilog/**` from upstream, including their unused `run.py` and `tools/*.py` files.
3. Resolve conflicts on deleted files (the Python package outside `vunit/vhdl` and `vunit/verilog`, the Python tests, `docs/`, `examples/`, `tools/`, `setup.py`, `pyproject.toml` and the Python workflows) by keeping the deletion. Keep this fork's `README.md`, `.gitignore` and `.github/workflows/ci.yml`.
4. Review `git diff <recorded>..<new> -- vunit/*.py vunit/**/*.py` for logic to port, using the mapping below. Review the changes to `vunit/vhdl/*/run.py` and `tests/acceptance/test_artificial.py` too, and update their translations in `tests/acceptance/`.
5. Run the acceptance tests with `RISIM_GHDL` set, and update the expected failures.
6. Update the recorded commit in this file.

## Tests

The `acceptance` test target ports VUnit's `tests/acceptance` (artificial VHDL testbenches and the package body dependency test) and the `run.py` of every VUnit VHDL library with a test directory. It leaves out what needs unsupported features: Verilog, `add_package`, test history, the check and location preprocessors, com codec generation, and the testbenches that `run.py` generates with Python. The expected outcomes follow VUnit, adapted to the deviations below; `tests/acceptance/vhdl_libraries.rs` lists the tests that fail only because every test runs in its own simulation.

## Module mapping

Rust modules are added as the frontend is implemented. Paths are relative to the upstream `vunit/` Python package unless noted.

| Module                      | Ported from                                                         | Responsibility                                                              |
| --------------------------- | ------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `config`                    | event-cache `tb_manager/config.rs`                                  | `risim-config.toml` → `ProjectSpec`, with spans for diagnostics             |
| `spec`                      | `ui/__init__.py`, `ui/library.py`, `ui/testbench.py` (setters only) | Programmatic `ProjectSpec` (libraries, options, configurations, features)   |
| `sources`                   | `ostools.py` (globs), `cached.py`                                   | Glob expansion, file reading (Latin-1), content hashing, parse cache        |
| `vhdl_parser`               | `vhdl_parser.py`                                                    | Regex-based design-unit/reference/generic extraction                        |
| `vhdl_standard`             | `vhdl_standard.py`                                                  | `VhdlStandard` and file-name tags (`2008p`, `93m`, …)                       |
| `project`                   | `project.py`, `library.py`, `source_file.py`, `design_unit.py`      | Libraries, source files, design units, dependency extraction                |
| `dependency_graph`          | `dependency_graph.py`                                               | Graph, toposort, dependents/dependencies, cycle reporting                   |
| `builtins`                  | `builtins.py`                                                       | Selection of VUnit/OSVVM files from the generated table                     |
| `discovery`                 | `test/bench.py`, `test/bench_list.py`, `test/list.py`               | Testbenches, tests, attributes, pragmas, locations                          |
| `configuration`             | `configuration.py`                                                  | Configurations (generics, sim options, attributes, VHDL configuration name) |
| `pattern`                   | `fnmatch` usage in `ui/__init__.py`                                 | Testcase pattern matching                                                   |
| `simulator`                 | `sim_if/__init__.py`, `sim_if/risim_ghdl.py` (`origin/risim`)       | risim-ghdl version/capabilities, compile and simulate command lines         |
| `compile`                   | `sim_if/__init__.py` (`compile_source_files`), `project.py`         | Compile set, recompile decision, library scheduling, cancellation           |
| `runner`                    | `test/suites.py`, `test/runner.py`, `ui/__init__.py` (output paths) | `runner_cfg`, seed, output directories, `vunit_results` parsing, outcomes   |
| `diagnostics`               | event-cache `tb_manager/compile_output.rs`                          | `Diagnostic` type, GHDL message parsing                                     |
| `process`                   | event-cache `tb_manager/subprocess_output.rs`                       | Spawning, output capture, process-tree termination                          |
| `store`                     | `database.py`, `hashing.py` (replaced)                              | `risim-out/` layout, JSON state files, atomic writes, lock                  |
| `watch`                     | — (new)                                                             | `notify` watcher, debouncing, change classification                         |
| `workspace`                 | event-cache `tb_manager.rs` (`PendingAction`)                       | Per-workspace actor: state, request merging, operations, events             |
| `runtime`                   | event-cache `tb_manager.rs` (semaphore)                             | Shared limits and risim-ghdl detection across workspaces                    |
| `verilog_parser` (deferred) | `parsing/verilog/*`                                                 | Deferred                                                                    |

## Intentional deviations

- Every test runs in its own simulation (`run_all_in_same_sim` is ignored).
- No test history; no seed "repeat"; no xUnit/JUnit reports; no elaborate-only mode; no `pre_config`/`post_check` hooks.
- A test that never started counts as failed (VUnit: skipped).
- `runner_cfg` has no `run script path`, so `run_script_path(runner_cfg)` is empty. Tests of a simulation start in name order. A test cancelled before it started keeps its previous result.
- The compile set includes the files declaring the VHDL configurations that runs elaborate. VUnit's `--minimal` compiles only the testbench files and their dependencies, which misses them.
- Recompilation uses compile keys instead of timestamps. Independent libraries compile in parallel and continue after unrelated failures.
- Duplicate tests, invalid attributes, and testbenches with no or several architectures disable only the affected testbench instead of aborting. Configuration errors (duplicate or empty names, invalid attributes) skip only that configuration.
- Testcase patterns are case-insensitive on all platforms (Python's `fnmatch` is case-insensitive only on Windows).
- Compile and simulation processes run with the workspace root as working directory.
- Files that are independent of each other compile in the order they were added. VUnit sorts them by path. Both orders respect the dependencies.
- An ambiguous direct entity instantiation is an error for the instantiating file instead of aborting. Dependency cycles are reported only when they involve files that a testbench needs.
- Mixed VHDL standards are checked among the files to compile, not among all files of the project.
- A compile that fails without a parsable error message gets an error diagnostic with the compiler output, even if it printed parsable warnings.
- Library work directories are `risim-out/libraries/<lowercase library name>`.
- The VHDL parser skips enumeration, record and array types, which VUnit only uses for com codec generation. Word boundaries (`\b`) only treat ASCII characters as word characters.
- The files of a user library named `vunit_lib` are added to the VUnit library, like `ui.library("vunit_lib").add_source_files(…)`; a precompiled library can't be named `vunit_lib`. A user library named `osvvm` replaces the builtin OSVVM library, with a warning. Library names, including `work`, are compared case-insensitively.
- com is always added from VHDL-2008 on, and left out for older standards instead of raising an error.
- In file patterns, `*` also matches hidden files, and `risim-out/` and `.git/` directories are never searched. Files with unknown extensions are skipped with a warning instead of raising an error.
- Testcase names keep the case of the entity declaration; `VUnit` uses the lowercase entity name. Test names that differ only in case produce a warning.
- Test configurations are declarative: each `TestConfigSpec` first sets its generics, simulation options and VHDL configuration in all configurations of its target, then adds its configurations as copies of the target's default configuration. Generic names are compared case-insensitively, and generics of a configuration that the entity doesn't declare produce a warning and are skipped (`VUnit` sets them silently). A `fail_on_warning` attribute of a configuration only affects that configuration instead of the whole testbench.
