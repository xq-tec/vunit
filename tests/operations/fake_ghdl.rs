// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! A fake risim-ghdl. The test binary runs this instead of the tests when `RISIM_FAKE_GHDL` is
//! set.
//!
//! - `--version` prints a risim-ghdl version line.
//! - `--sleep <ms>` sleeps.
//! - `-a … <file>` "analyses" the file: it appends `start <work> <file name>` and
//!   `end <work> <file name>` to `fake-ghdl.log` in the working directory, and follows the
//!   directives in the file's comments, in order:
//!   - `-- fake: error <message>`, `warning`, `note`: print a GHDL message for that line; an
//!     error makes the exit code 1;
//!   - `-- fake: output <text>`: print `text` to stderr;
//!   - `-- fake: exit <code>`: set the exit code;
//!   - `-- fake: sleep <ms>`: sleep;
//!   - `-- fake: hang`: sleep for an hour;
//!   - `-- fake: spawn-sleeper <file>`: start a sleeping grandchild and write its PID to `file`.
//! - `--elab-run … --name=<testcase>` "simulates" the testcase: it appends `simstart <testcase>`
//!   and `simend <testcase>` to the log, writes its arguments to `args.txt` in the output path
//!   from `runner_cfg`, and follows the directives for the testcase in `fake-sim.txt` in the
//!   working directory (lines `<testcase>\t<directive>`), in order:
//!   - `fail`: the test starts but the suite doesn't complete, exit code 1;
//!   - `no-results`: don't write `vunit_results`;
//!   - `exit <code>`: set the exit code;
//!   - `print <text>`: print `text` to stdout;
//!   - `sleep <ms>`, `hang`: as for `-a`.
//!
//!   Without `fail` or `no-results`, the enabled test passes: `vunit_results` records its start
//!   and the end of the suite.
//!
//! AI NOTICE: Generated, minimally reviewed.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a fake simulator talks through stdout and stderr"
)]

use std::env;
use std::fs;
use std::io::Write as _;
use std::process::Command;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

/// The environment variable that turns the test binary into the fake simulator.
pub const ENV: &str = "RISIM_FAKE_GHDL";

/// The log file in the working directory.
pub const LOG: &str = "fake-ghdl.log";

/// The simulation directives in the working directory.
pub const SIM_DIRECTIVES: &str = "fake-sim.txt";

pub fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("GHDL 6.4.0-fake (tests) [simulation adapter]");
            ExitCode::SUCCESS
        },
        Some("--sleep") => {
            let millis = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or(0);
            thread::sleep(Duration::from_millis(millis));
            ExitCode::SUCCESS
        },
        Some("-a") => analyse(&args[1..]),
        Some("--elab-run") => simulate(&args[1..]),
        _ => {
            eprintln!("fake risim-ghdl: unsupported arguments {args:?}");
            ExitCode::from(2)
        },
    }
}

fn log(line: &str) {
    let mut file = fs::File::options()
        .create(true)
        .append(true)
        .open(LOG)
        .expect("open the log");
    // One write per line, so lines of concurrent processes don't interleave.
    file.write_all(format!("{line}\n").as_bytes())
        .expect("write the log");
}

fn analyse(args: &[String]) -> ExitCode {
    let work = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--work="))
        .unwrap_or("?");
    let file = args.last().map_or("", String::as_str);
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    log(&format!("start {work} {name}"));

    let contents = String::from_utf8_lossy(&fs::read(file).unwrap_or_default()).into_owned();
    let mut code = 0;
    for (number, line) in contents.lines().enumerate() {
        let Some((_, directive)) = line.split_once("-- fake:") else {
            continue;
        };
        let directive = directive.trim();
        let (command, argument) = directive.split_once(' ').unwrap_or((directive, ""));
        let line_number = number + 1;
        match command {
            "error" | "warning" | "note" => {
                println!("{file}:{line_number}:1:{command}: {argument}");
                if command == "error" {
                    code = 1;
                }
            },
            "output" => eprintln!("{argument}"),
            "spawn-sleeper" => {
                #[expect(
                    clippy::zombie_processes,
                    reason = "the sleeper must outlive this process"
                )]
                let child = Command::new(env::current_exe().expect("current exe"))
                    .args(["--sleep", "60000"])
                    .spawn()
                    .expect("spawn the sleeper");
                fs::write(argument, child.id().to_string()).expect("write the PID");
            },
            _ if shared_directive(command, argument, &mut code) => {},
            _ => eprintln!("fake risim-ghdl: unknown directive {directive}"),
        }
    }
    log(&format!("end {work} {name}"));
    ExitCode::from(code)
}

/// Runs a directive that `analyse` and `simulate` handle alike: `exit`, `sleep` or `hang`.
/// Returns `false` for any other directive.
fn shared_directive(command: &str, argument: &str, code: &mut u8) -> bool {
    match command {
        "exit" => *code = argument.parse().unwrap_or(1),
        "sleep" => thread::sleep(Duration::from_millis(argument.parse().unwrap_or(0))),
        "hang" => thread::sleep(Duration::from_secs(3600)),
        _ => return false,
    }
    true
}

/// Decodes VUnit's dictionary encoding (`encode_dict`).
fn decode_dict(encoded: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut entry = String::new();
    let mut chars = encoded.chars().peekable();
    let mut push = |current: &mut String| {
        if let Some((key, value)) = current.split_once(" : ") {
            let unescape = |text: &str| text.replace("::", ":");
            entries.push((unescape(key), unescape(value)));
        }
        current.clear();
    };
    while let Some(ch) = chars.next() {
        if ch == ',' {
            if chars.peek() == Some(&',') {
                chars.next();
                entry.push(',');
            } else {
                push(&mut entry);
            }
        } else {
            entry.push(ch);
        }
    }
    push(&mut entry);
    entries
}

fn simulate(args: &[String]) -> ExitCode {
    let name = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--name="))
        .unwrap_or("?")
        .to_owned();
    log(&format!("simstart {name}"));
    let runner_cfg = args
        .iter()
        .find_map(|arg| arg.strip_prefix("-grunner_cfg="))
        .map(decode_dict)
        .unwrap_or_default();
    let value = |key: &str| {
        runner_cfg
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    let output_path = value("output path");
    fs::write(format!("{output_path}args.txt"), args.join("\n")).expect("write args.txt");

    let directives = fs::read_to_string(SIM_DIRECTIVES).unwrap_or_default();
    let mut code = 0;
    let mut results = true;
    let mut suite_done = true;
    for directive in directives.lines().filter_map(|line| {
        let (testcase, directive) = line.split_once('\t')?;
        (testcase == name).then_some(directive)
    }) {
        let (command, argument) = directive.split_once(' ').unwrap_or((directive, ""));
        match command {
            "fail" => {
                suite_done = false;
                code = 1;
            },
            "no-results" => results = false,
            "print" => println!("{argument}"),
            _ if shared_directive(command, argument, &mut code) => {},
            _ => eprintln!("fake risim-ghdl: unknown directive {directive}"),
        }
    }
    if results {
        // The enabled test is still encoded with `encode_test_case`.
        let enabled = value("enabled_test_cases").replace(",,", ",");
        let mut contents = String::new();
        if !enabled.is_empty() {
            contents.push_str("test_start:");
            contents.push_str(&enabled);
            contents.push('\n');
        }
        if suite_done {
            contents.push_str("test_suite_done\n");
        }
        let mut file = fs::File::options()
            .append(true)
            .open(format!("{output_path}vunit_results"))
            .expect("open vunit_results");
        file.write_all(contents.as_bytes())
            .expect("write vunit_results");
    }
    log(&format!("simend {name}"));
    ExitCode::from(code)
}
