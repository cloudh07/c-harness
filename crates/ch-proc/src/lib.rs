//! Starting children in their own process group and ending the whole tree below them.

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

/// Ends a child started with [`own_group`] and every process below it, then reaps it.
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
    let root = child.id();
    // Children that started groups of their own sit outside the root's group, so they are
    // found by parent before anything is signalled; once the root dies the link is gone.
    let below = descendants(root, &process_table());
    let targets: Vec<String> = std::iter::once(root)
        .chain(below)
        .flat_map(|pid| [format!("-{pid}"), pid.to_string()])
        .collect();
    // A target that is already gone, or a pid that leads no group, makes `kill` fail for
    // that one target only; the rest are still signalled.
    run_kill("-TERM", &targets)?;
    let deadline = Instant::now() + TERM_GRACE;
    loop {
        // Reap the root first: a zombie still counts as a member of its group.
        let root_gone = child.try_wait()?.is_some();
        if root_gone && !any_alive(&targets)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(GROUP_POLL);
    }
    run_kill("-KILL", &targets)?;
    Ok(())
}

#[cfg(unix)]
fn any_alive(targets: &[String]) -> io::Result<bool> {
    for target in targets {
        if run_kill("-0", std::slice::from_ref(target))?.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn run_kill(signal: &str, targets: &[String]) -> io::Result<ExitStatus> {
    command("kill")
        .arg(signal)
        .arg("--")
        .args(targets)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}

/// Every running process as `(pid, parent)`. Empty when `ps` is missing or fails, which
/// leaves only the root's own group to be ended.
#[cfg(unix)]
fn process_table() -> Vec<(u32, u32)> {
    let output = command("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            parse_table(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Vec::new(),
    }
}

#[cfg(any(unix, test))]
fn parse_table(ps: &str) -> Vec<(u32, u32)> {
    ps.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            Some((pid, parent))
        })
        .collect()
}

/// Everyone below `root` in a `(pid, parent)` table, nearest first.
#[cfg(any(unix, test))]
fn descendants(root: u32, table: &[(u32, u32)]) -> Vec<u32> {
    use std::collections::{HashSet, VecDeque};

    let mut found = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(parent) = queue.pop_front() {
        for &(pid, ppid) in table {
            if ppid == parent && seen.insert(pid) {
                found.push(pid);
                queue.push_back(pid);
            }
        }
    }
    found
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
fn sleeper() -> Command {
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
    use std::fs::File;
    use std::path::{Path, PathBuf};

    use super::*;

    const CHILD_ROLE: &str = "CH_PROC_TEST_CHILD";
    const LOCK_FILE: &str = "CH_PROC_TEST_LOCK";

    /// A no-op as a test. The tests below run this binary again with only this test selected
    /// and `CHILD_ROLE` set.
    #[test]
    fn child_helper() {
        match std::env::var(CHILD_ROLE).as_deref() {
            // Starts a child in a group of its own, as the runner does with omp and git.
            Ok("parent") => {
                let mut hold = helper("hold");
                own_group(&mut hold);
                hold.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap()
                    .wait()
                    .unwrap();
            }
            // Holds a lock for as long as it lives, so the test can tell when it is gone.
            Ok("hold") => {
                let path = PathBuf::from(std::env::var_os(LOCK_FILE).unwrap());
                let file = File::create(&path).unwrap();
                file.lock().unwrap();
                File::create(started(&path)).unwrap();
                thread::sleep(Duration::from_secs(30));
            }
            _ => {}
        }
    }

    fn helper(role: &str) -> Command {
        let mut command = command(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::child_helper"])
            .env(CHILD_ROLE, role);
        command
    }

    fn started(lock: &Path) -> PathBuf {
        lock.with_extension("started")
    }

    fn lock_free(lock: &Path) -> bool {
        File::open(lock).is_ok_and(|file| file.try_lock().is_ok())
    }

    fn within_ten_seconds(check: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !check() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL);
        }
        true
    }

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
    fn ends_a_child_that_left_the_group() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("held");
        let mut command = helper("parent");
        command.env(LOCK_FILE, &lock);
        own_group(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        assert!(
            within_ten_seconds(|| started(&lock).exists()),
            "the grandchild never took its lock"
        );
        assert!(kill_tree(&mut child).unwrap().is_some());
        assert!(
            within_ten_seconds(|| lock_free(&lock)),
            "the grandchild outlived kill_tree"
        );
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

    #[test]
    fn reads_the_process_table() {
        let ps = "    1     0\n  412     1\nnot a row\n  413   412  extra\n\n";
        assert_eq!(parse_table(ps), [(1, 0), (412, 1), (413, 412)]);
    }

    #[test]
    fn finds_everyone_below_the_root_once() {
        // 20 started 21 and 22; 22 started 23. 30 is not ours. 24 and 25 name each other.
        let table = [
            (20, 1),
            (21, 20),
            (22, 20),
            (23, 22),
            (30, 1),
            (24, 25),
            (25, 24),
        ];
        assert_eq!(descendants(20, &table), [21, 22, 23]);
        assert_eq!(descendants(23, &table), [] as [u32; 0]);
        assert_eq!(descendants(24, &table), [25]);
    }
}
