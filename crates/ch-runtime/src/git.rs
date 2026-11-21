//! Running git on this machine for local sessions.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::process::{Command, Output, Stdio};

use crate::RuntimeError;
use crate::process::{command, stderr_tail};

// A git hook or wrapper may export these, and they would point git at another repository.
pub(crate) fn clear_git_location(command: &mut Command) {
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(var);
    }
}

pub(crate) fn run_git(action: &'static str, args: Vec<OsString>) -> Result<(), RuntimeError> {
    git_output(action, args).map(drop)
}

/// Runs git and returns its trimmed stdout.
pub(crate) fn git_output(
    action: &'static str,
    args: Vec<OsString>,
) -> Result<String, RuntimeError> {
    let output = git(args, Stdio::null()).output().map_err(spawn_error)?;
    finish(action, &output)
}

/// Runs git with `input` on its stdin. For commands that print little, such as
/// `update-ref --stdin`.
pub(crate) fn git_with_input(
    action: &'static str,
    args: Vec<OsString>,
    input: &[u8],
) -> Result<String, RuntimeError> {
    let mut child = git(args, Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(input));
    let output = child.wait_with_output().map_err(spawn_error)?;
    // A write error means git exited early; its stderr says why.
    if written.is_err() && output.status.success() {
        return Err(RuntimeError::Git {
            action,
            stderr: "git stopped reading its input".to_owned(),
        });
    }
    finish(action, &output)
}

pub(crate) fn os_args<const N: usize>(args: [&dyn AsRef<OsStr>; N]) -> Vec<OsString> {
    args.iter().map(|arg| arg.as_ref().to_owned()).collect()
}

fn git(args: Vec<OsString>, stdin: Stdio) -> Command {
    let mut git = command("git");
    clear_git_location(&mut git);
    git.args(args).stdin(stdin);
    git
}

fn spawn_error(source: io::Error) -> RuntimeError {
    if source.kind() == io::ErrorKind::NotFound {
        RuntimeError::ToolNotFound { tool: "git" }
    } else {
        RuntimeError::Spawn {
            program: "git".to_owned(),
            source,
        }
    }
}

fn finish(action: &'static str, output: &Output) -> Result<String, RuntimeError> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(RuntimeError::Git {
            action,
            stderr: stderr_tail(&output.stderr),
        })
    }
}
