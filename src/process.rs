// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Spawning and supervising simulator processes.
//!
//! Every process runs in its own process tree, which cancelling terminates as a whole:
//!
//! - On Unix, the process gets its own process group. Cancelling sends `SIGTERM` to the group,
//!   and `SIGKILL` after [`KILL_GRACE_PERIOD`].
//! - On Windows, the process is assigned to a job object that kills its processes when it is
//!   closed. Cancelling terminates the job. A grandchild spawned before the assignment escapes,
//!   which is acceptable because risim-ghdl doesn't spawn children for `-a` or `--elab-run`.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fs;
use std::fs::File;
use std::io;
use std::io::Write as _;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use camino::Utf8Path;
use tokio::io::AsyncBufReadExt as _;
use tokio::io::AsyncRead;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How long a cancelled process may take to exit after `SIGTERM` before it is killed.
pub const KILL_GRACE_PERIOD: Duration = Duration::from_secs(2);

/// How a supervised process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The process exited by itself.
    Exited(ExitStatus),
    /// The process was cancelled and terminated.
    Cancelled,
}

/// Creates a command for `program` without a console window on Windows.
///
/// The process is killed if the returned command's child is dropped.
pub fn command(program: &Utf8Path, args: &[String], cwd: Option<&Utf8Path>) -> Command {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn split_command_line(command_line: &[String]) -> io::Result<(&Utf8Path, &[String])> {
    match command_line {
        [program, args @ ..] => Ok((Utf8Path::new(program), args)),
        [] => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty command line",
        )),
    }
}

fn create_output_file(path: &Utf8Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create(path)
}

/// Runs `command_line` in `cwd` with stdout and stderr redirected to `output_file`, which
/// starts with `header`.
///
/// # Errors
///
/// Fails if the output file can't be created or the process can't be spawned or waited for.
pub async fn run_to_file(
    command_line: &[String],
    cwd: &Utf8Path,
    output_file: &Utf8Path,
    header: &[u8],
    cancel: &CancellationToken,
) -> io::Result<Outcome> {
    let (program, args) = split_command_line(command_line)?;
    let mut stdout = create_output_file(output_file)?;
    stdout.write_all(header)?;
    let stderr = stdout.try_clone()?;
    let mut command = command(program, args, Some(cwd));
    command.stdout(stdout).stderr(stderr);
    tracing::debug!(?command_line, %output_file, "spawning");
    let mut process = Supervised::spawn(command)?;
    process.wait(cancel).await
}

/// Runs `command_line` in `cwd`, copies stdout and stderr to `output_file`, and calls
/// `on_line` for every line, without the line terminator and decoded lossily as UTF-8.
///
/// Lines of stdout and stderr are interleaved in the order they are read.
///
/// # Errors
///
/// Fails if the output file can't be created or written, or the process can't be spawned or
/// waited for.
pub async fn run_piped(
    command_line: &[String],
    cwd: &Utf8Path,
    output_file: &Utf8Path,
    cancel: &CancellationToken,
    mut on_line: impl FnMut(&str),
) -> io::Result<Outcome> {
    let (program, args) = split_command_line(command_line)?;
    let mut output = io::BufWriter::new(create_output_file(output_file)?);
    let mut command = command(program, args, Some(cwd));
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    tracing::debug!(?command_line, %output_file, "spawning");
    let mut process = Supervised::spawn(command)?;

    let (sender, mut lines) = mpsc::unbounded_channel();
    if let Some(stdout) = process.child.stdout.take() {
        tokio::spawn(forward_lines(stdout, sender.clone()));
    }
    if let Some(stderr) = process.child.stderr.take() {
        tokio::spawn(forward_lines(stderr, sender));
    }

    loop {
        tokio::select! {
            line = lines.recv() => {
                let Some(line) = line else { break };
                let line = line?;
                output.write_all(&line)?;
                let text = String::from_utf8_lossy(&line);
                on_line(text.trim_end_matches(['\n', '\r']));
            },
            () = cancel.cancelled() => {
                process.terminate().await?;
                output.flush()?;
                return Ok(Outcome::Cancelled);
            },
        }
    }
    output.flush()?;
    process.wait(cancel).await
}

async fn forward_lines(
    stream: impl AsyncRead + Unpin,
    sender: mpsc::UnboundedSender<io::Result<Vec<u8>>>,
) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => return,
            Ok(_) => {
                if sender.send(Ok(line)).is_err() {
                    return;
                }
            },
            Err(error) => {
                let _ignored = sender.send(Err(error));
                return;
            },
        }
    }
}

/// A child process in its own process tree.
struct Supervised {
    child: Child,
    #[cfg(unix)]
    process_group: Option<nix::unistd::Pid>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl Supervised {
    fn spawn(mut command: Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            command.process_group(0);
            let child = command.spawn()?;
            let process_group = child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .map(nix::unistd::Pid::from_raw);
            Ok(Self {
                child,
                process_group,
            })
        }
        #[cfg(windows)]
        {
            let child = command.spawn()?;
            let job = match job::Job::new().and_then(|job| {
                if let Some(handle) = child.raw_handle() {
                    job.assign(handle)?;
                }
                Ok(job)
            }) {
                Ok(job) => Some(job),
                Err(error) => {
                    tracing::warn!(%error, "failed to put the process into a job object");
                    None
                },
            };
            Ok(Self { child, job })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                child: command.spawn()?,
            })
        }
    }

    /// Waits for the process to exit, terminating it if `cancel` fires first.
    async fn wait(&mut self, cancel: &CancellationToken) -> io::Result<Outcome> {
        tokio::select! {
            status = self.child.wait() => Ok(Outcome::Exited(status?)),
            () = cancel.cancelled() => {
                self.terminate().await?;
                Ok(Outcome::Cancelled)
            },
        }
    }

    /// Terminates the whole process tree and waits for the process to exit.
    async fn terminate(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(group) = self.process_group {
            use nix::sys::signal::Signal;
            use nix::sys::signal::killpg;

            if let Err(error) = killpg(group, Signal::SIGTERM) {
                tracing::debug!(%error, "failed to send SIGTERM");
            }
            let exited = tokio::time::timeout(KILL_GRACE_PERIOD, self.child.wait()).await;
            // Kill the rest of the group even if the process itself exited, so that no
            // grandchild survives. The group is usually gone by now.
            if let Err(error) = killpg(group, Signal::SIGKILL) {
                tracing::trace!(%error, "failed to send SIGKILL");
            }
            if let Ok(status) = exited {
                status?;
                return Ok(());
            }
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        // Without a process group or job, only the process itself is killed.
        if let Err(error) = self.child.start_kill() {
            tracing::debug!(%error, "failed to kill process");
        }
        self.child.wait().await?;
        Ok(())
    }
}

#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::RawHandle;

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows::Win32::System::JobObjects::CreateJobObjectW;
    use windows::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    use windows::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
    use windows::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
    use windows::Win32::System::JobObjects::SetInformationJobObject;
    use windows::Win32::System::JobObjects::TerminateJobObject;
    use windows::core::PCWSTR;

    /// A job object that kills its processes when it is closed.
    pub(super) struct Job(HANDLE);

    // SAFETY: A job handle can be used from any thread.
    unsafe impl Send for Job {}
    // SAFETY: The job functions used here are thread-safe.
    unsafe impl Sync for Job {}

    impl Job {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: Creates an anonymous job with default security; no pointers are passed.
            let handle =
                unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(io::Error::from)?;
            let job = Self(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the structure is far smaller than u32::MAX"
            )]
            let size = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            // SAFETY: `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION of `size` bytes
            // that outlives the call.
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast::<c_void>(),
                    size,
                )
            }
            .map_err(io::Error::from)?;
            Ok(job)
        }

        pub(super) fn assign(&self, process: RawHandle) -> io::Result<()> {
            // SAFETY: Both handles are valid: the job is owned by `self`, and the process handle
            // belongs to a child that hasn't been waited for yet.
            unsafe { AssignProcessToJobObject(self.0, HANDLE(process)) }.map_err(io::Error::from)
        }

        pub(super) fn terminate(&self) {
            // SAFETY: The job handle is valid while `self` exists.
            if let Err(error) = unsafe { TerminateJobObject(self.0, 1) } {
                tracing::debug!(%error, "failed to terminate job");
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: The handle is valid and closed only here.
            let _ignored = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    #[cfg(unix)]
    use nix::sys::signal::kill;
    #[cfg(unix)]
    use nix::unistd::Pid;

    use super::*;

    fn temp() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().to_owned();
        (dir, path)
    }

    fn shell(script: &str) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".to_owned(), "/C".to_owned(), script.to_owned()]
        } else {
            vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
        }
    }

    #[tokio::test]
    async fn run_to_file_captures_output_and_status() {
        let (_dir, root) = temp();
        let output = root.join("nested/out.txt");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, "old contents\n").unwrap();
        let outcome = run_to_file(
            &shell("echo hello && echo oops 1>&2 && exit 3"),
            &root,
            &output,
            b"header\n",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let Outcome::Exited(status) = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(status.code(), Some(3));
        let contents = fs::read_to_string(&output).unwrap();
        assert!(contents.starts_with("header\n"), "{contents}");
        assert!(contents.contains("hello") && contents.contains("oops"));
        assert!(!contents.contains("old contents"));
    }

    #[tokio::test]
    async fn run_piped_reports_lines() {
        let (_dir, root) = temp();
        let output = root.join("out.txt");
        let mut lines = Vec::new();
        let outcome = run_piped(
            &shell("echo one && echo two 1>&2 && echo three"),
            &root,
            &output,
            &CancellationToken::new(),
            |line| lines.push(line.to_owned()),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Exited(status) if status.success()));
        lines.sort();
        assert_eq!(lines, ["one", "three", "two"]);
        let contents = fs::read_to_string(&output).unwrap();
        assert_eq!(contents.lines().count(), 3);
    }

    #[tokio::test]
    async fn spawn_failure_is_an_error() {
        let (_dir, root) = temp();
        let result = run_to_file(
            &[root.join("missing-program").to_string()],
            &root,
            &root.join("out.txt"),
            b"",
            &CancellationToken::new(),
        )
        .await;
        result.unwrap_err();
        run_to_file(
            &[],
            &root,
            &root.join("out.txt"),
            b"",
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_process_group() {
        let (_dir, root) = temp();
        let pid_file = root.join("grandchild.pid");
        let cancel = CancellationToken::new();
        let script = format!("sleep 30 & echo $! > {pid_file}; echo started; wait");
        let task = {
            let cancel = cancel.clone();
            let root = root.clone();
            tokio::spawn(async move {
                run_piped(
                    &shell(&script),
                    &root,
                    &root.join("out.txt"),
                    &cancel,
                    |_| {
                        cancel.cancel();
                    },
                )
                .await
            })
        };
        let outcome = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome, Outcome::Cancelled);

        let pid: i32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = Pid::from_raw(pid);
        // The grandchild may linger briefly as a zombie of the killed shell; wait until it's gone.
        for _ in 0..100 {
            if kill(pid, None).is_err() {
                return;
            }
            let status = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            if status.split_whitespace().nth(2) == Some("Z") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("grandchild {pid} survived");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_escalates_to_sigkill() {
        let (_dir, root) = temp();
        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let task = {
            let cancel = cancel.clone();
            let root = root.clone();
            tokio::spawn(async move {
                run_piped(
                    &shell("trap '' TERM; echo ready; while true; do sleep 0.05; done"),
                    &root,
                    &root.join("out.txt"),
                    &cancel,
                    |_| cancel.cancel(),
                )
                .await
            })
        };
        let outcome = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(started.elapsed() >= KILL_GRACE_PERIOD);
    }
}
