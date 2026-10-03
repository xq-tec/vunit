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
            "exit" => code = argument.parse().unwrap_or(1),
            "sleep" => thread::sleep(Duration::from_millis(argument.parse().unwrap_or(0))),
            "hang" => thread::sleep(Duration::from_secs(3600)),
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
            _ => eprintln!("fake risim-ghdl: unknown directive {directive}"),
        }
    }
    log(&format!("end {work} {name}"));
    ExitCode::from(code)
}
