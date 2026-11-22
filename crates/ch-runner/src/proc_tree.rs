//! Starting children in their own process group and ending the whole group.

use std::ffi::OsStr;
use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(25);
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(2);
const REAP_WAIT: Duration = Duration::from_secs(5);

/// A command that opens no console window of its own on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The desktop app starts the runner without a console, so each console child would
        // otherwise flash a window of its own.
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Puts the child at the head of a new process group, so everything it starts can be ended.
pub fn own_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// Polls until the child exits or `timeout` passes.
pub fn wait_for(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

/// Waits up to `limit` for a thread to finish, without blocking past it.
pub fn finished_within<T>(handle: &JoinHandle<T>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL);
    }
    true
}

/// Joins a thread that finishes within `limit`; one that does not is left to run detached.
pub fn join_within<T>(handle: JoinHandle<T>, limit: Duration) -> Option<T> {
    if finished_within(&handle, limit) {
        handle.join().ok()
    } else {
        None
    }
}

/// Ends a child started with [`own_group`] and everything in its group, then reaps it.
///
/// Returns the child's exit status if it could be reaped in time.
pub fn kill_tree(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    signal_tree(child)?;
    wait_for(child, REAP_WAIT)
}

#[cfg(unix)]
const GROUP_POLL: Duration = Duration::from_millis(100);

#[cfg(unix)]
fn signal_tree(child: &mut Child) -> io::Result<()> {
    let group = format!("-{}", child.id());
    // The leader may be gone while others in its group live on, so the group is always
    // signalled; `kill` failing because nobody is left is not an error here.
    run_kill(&["-TERM", "--", &group])?;
    let deadline = Instant::now() + TERM_GRACE;
    loop {
        // Reap the leader first: a zombie still counts as a member of its group.
        let leader_gone = child.try_wait()?.is_some();
        if leader_gone && !group_alive(&group)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(GROUP_POLL);
    }
    run_kill(&["-KILL", "--", &group])?;
    Ok(())
}

#[cfg(unix)]
fn group_alive(group: &str) -> io::Result<bool> {
    Ok(run_kill(&["-0", "--", group])?.success())
}

#[cfg(unix)]
fn run_kill(args: &[&str]) -> io::Result<ExitStatus> {
    command("kill")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}

#[cfg(windows)]
fn signal_tree(child: &mut Child) -> io::Result<()> {
    let status = command("taskkill")
        .args(["/T", "/F", "/PID"])
        .arg(child.id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    // taskkill fails when the leader has already exited, and then cannot find the rest of
    // the tree either. If the leader is somehow still there, end it directly.
    if !status.success() && child.try_wait()?.is_none() {
        child.kill()?;
    }
    Ok(())
}

/// A command that runs for about 30 seconds and starts a child of its own.
#[cfg(test)]
pub(crate) fn sleeper() -> Command {
    #[cfg(unix)]
    {
        let mut command = command("sh");
        command.args(["-c", "sleep 30 & sleep 30"]);
        command
    }
    #[cfg(windows)]
    {
        let mut command = command("cmd");
        command.args(["/D", "/C", "ping -n 30 127.0.0.1 >NUL"]);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ends_a_running_group() {
        let mut command = sleeper();
        own_group(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let status = kill_tree(&mut child).unwrap();
        assert!(status.is_some(), "child was not reaped");
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn tolerates_a_child_that_already_exited() {
        let mut command = command("git");
        command.arg("--version");
        own_group(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let status = wait_for(&mut child, Duration::from_secs(30)).unwrap();
        assert!(status.is_some_and(|s| s.success()));
        assert!(kill_tree(&mut child).unwrap().is_some());
    }
}
