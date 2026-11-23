use std::ffi::OsStr;
use std::io;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output};

use crate::{RunnerInfo, RuntimeError, SessionLock};

const MAX_STDERR_TAIL_BYTES: usize = 2000;

/// A running `ch-runner serve`, or the `ssh` client that carries one.
///
/// Dropping it kills and reaps the process it owns, then releases the session lock it holds.
/// Over SSH that process is the local `ssh` client: the remote runner sees its stdin close and
/// is expected to exit on its own.
#[derive(Debug)]
pub struct RunnerProcess {
    child: Child,
    // Fields drop after `Drop::drop`, so the lock outlives the process.
    lock: Option<SessionLock>,
}

impl RunnerProcess {
    pub(crate) fn new(child: Child) -> Self {
        Self { child, lock: None }
    }

    pub(crate) fn locked(child: Child, lock: SessionLock) -> Self {
        Self {
            child,
            lock: Some(lock),
        }
    }

    /// Takes the session lock, so the caller can keep it until the session's worktree is gone.
    pub fn take_lock(&mut self) -> Option<SessionLock> {
        self.lock.take()
    }

    /// The OS process id of the local child.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Kills the process. Succeeds if it has already exited.
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Waits for the process to exit. Closes its stdin first if it was not taken.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }
}

impl Drop for RunnerProcess {
    fn drop(&mut self) {
        // Errors here mean the process is already gone or cannot be signalled; there is nothing
        // left to do from a destructor either way.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The desktop app has no console, so without this every git or ssh call flashes one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The end of a process's stderr, trimmed, for an error message.
pub(crate) fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.len() <= MAX_STDERR_TAIL_BYTES {
        return text.to_owned();
    }
    let start = text.ceil_char_boundary(text.len() - MAX_STDERR_TAIL_BYTES);
    format!("...{}", &text[start..])
}

/// Reads the output of `ch-runner --version` run at `location`.
pub(crate) fn runner_info(location: &str, output: &Output) -> Result<RunnerInfo, RuntimeError> {
    let failed = |stderr: String| RuntimeError::RunnerCheckFailed {
        location: location.to_owned(),
        code: output.status.code(),
        stderr,
    };
    if !output.status.success() {
        return Err(failed(stderr_tail(&output.stderr)));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().map(str::trim).find(|line| !line.is_empty());
    let version = line.map(|line| line.strip_prefix("ch-runner ").unwrap_or(line).trim());
    match version {
        Some(version) if !version.is_empty() => Ok(RunnerInfo {
            version: version.to_owned(),
        }),
        _ => Err(failed("it printed no version".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::output;

    #[test]
    fn version_line_loses_the_program_name() {
        let info = runner_info("here", &output(0, "\nch-runner 0.1.0\n", "")).unwrap();
        assert_eq!(info.version, "0.1.0");
        let info = runner_info("here", &output(0, "0.2.0-dev\n", "")).unwrap();
        assert_eq!(info.version, "0.2.0-dev");
    }

    #[test]
    fn failing_or_silent_version_is_an_error() {
        let err = runner_info("box", &output(2, "", "boom")).unwrap_err();
        assert!(matches!(
            err,
            RuntimeError::RunnerCheckFailed { code: Some(2), ref stderr, .. } if stderr == "boom"
        ));
        assert!(matches!(
            runner_info("box", &output(0, "  \n", "")),
            Err(RuntimeError::RunnerCheckFailed { .. })
        ));
    }

    #[test]
    fn stderr_tail_keeps_the_end_on_a_char_boundary() {
        let long = format!("{}end", "é".repeat(MAX_STDERR_TAIL_BYTES));
        let tail = stderr_tail(long.as_bytes());
        assert!(tail.starts_with("..."));
        assert!(tail.ends_with("end"));
        assert!(tail.len() <= MAX_STDERR_TAIL_BYTES + 3);
        assert_eq!(stderr_tail(b"  short \n"), "short");
    }
}
