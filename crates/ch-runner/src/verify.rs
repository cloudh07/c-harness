//! Running a verification command and measuring it.

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ch_protocol::SUBSCRIPTION_TOKEN_ENV;
use ch_protocol::runner::{Evidence, MAX_OUTPUT_TAIL_BYTES};

use crate::proc_tree;
use crate::tail::TailBuffer;

pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

const POLL: Duration = Duration::from_millis(25);

/// How long the output may keep arriving after the command itself has exited, from processes
/// it left behind, before they are ended.
const LINGER: Duration = Duration::from_secs(2);

const CHANGED_DURING: &str = "the work changed while the last check ran (the agent or the check \
                              wrote files): run Verify again";
const CHANGED_AFTER: &str =
    "the work changed after the last check: run Verify again before landing";
const NOT_RECORDED: &str = "the last check could not record the work it ran on: run Verify again";

/// What the last finished check says about the work, for land to hold it to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    /// The check ran on this tree, which stood still while it ran.
    Tree(String),
    /// The check does not stand for any one tree; land is refused for this reason.
    Unusable(&'static str),
}

impl Checked {
    /// The trees read at the start and end of a check.
    pub fn from_trees(before: &Result<String, String>, after: &Result<String, String>) -> Self {
        match (before, after) {
            (Ok(before), Ok(after)) if before == after => Self::Tree(before.clone()),
            (Ok(_), Ok(_)) => Self::Unusable(CHANGED_DURING),
            _ => Self::Unusable(NOT_RECORDED),
        }
    }

    /// Whether work whose tree is `tree` may land on this check.
    pub fn allows(&self, tree: &str) -> Result<(), String> {
        match self {
            Self::Tree(checked) if checked == tree => Ok(()),
            Self::Tree(_) => Err(CHANGED_AFTER.to_owned()),
            Self::Unusable(reason) => Err((*reason).to_owned()),
        }
    }
}

pub struct Job {
    pub id: String,
    pub command: String,
    pub timeout_secs: Option<u64>,
    pub workdir: PathBuf,
    pub head: Option<String>,
}

/// Runs `job` to completion, or until its timeout or `stop` ends it.
pub fn run(job: Job, stop: &AtomicBool) -> Result<Evidence, String> {
    let mut command = shell(&job.command);
    proc_tree::own_group(&mut command);
    let (reader, writer) = io::pipe().map_err(|e| format!("could not make a pipe: {e}"))?;
    let writer_for_stderr = writer
        .try_clone()
        .map_err(|e| format!("could not make a pipe: {e}"))?;
    // The command runs code the agent wrote, which must not reach a subscription login either.
    for name in SUBSCRIPTION_TOKEN_ENV {
        command.env_remove(name);
    }
    command
        .current_dir(&job.workdir)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(writer_for_stderr);
    let started = Instant::now();
    let spawned = command.spawn();
    // The command holds this process's copies of the pipe's write end; until they are closed
    // the reader would never see the end of the output.
    drop(command);
    let mut child = spawned.map_err(|e| format!("could not run the command: {e}"))?;

    let output = Arc::new(Mutex::new(TailBuffer::new(MAX_OUTPUT_TAIL_BYTES)));
    let collector = collect(reader, Arc::clone(&output));

    let timeout = Duration::from_secs(job.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let deadline = started.checked_add(timeout);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        let expired = deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if expired || stop.load(Ordering::Relaxed) {
            timed_out = expired;
            break proc_tree::kill_tree(&mut child).map_err(|e| e.to_string())?;
        }
        thread::sleep(POLL);
    };
    let duration = started.elapsed();

    if !proc_tree::finished_within(&collector, LINGER) {
        // Something the command started still holds the output open; end its whole group.
        let _ = proc_tree::kill_tree(&mut child);
        proc_tree::finished_within(&collector, LINGER);
    }
    let (output_tail, truncated) = output.lock().unwrap_or_else(PoisonError::into_inner).text();
    let exit_code = if timed_out {
        None
    } else {
        status.and_then(|status| status.code())
    };
    Ok(Evidence {
        id: job.id,
        command: job.command,
        exit_code,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        head: job.head,
        tree_before: None,
        tree_after: None,
        output_tail,
        truncated,
        timed_out,
    })
}

#[cfg(unix)]
fn shell(script: &str) -> Command {
    let mut command = proc_tree::command("sh");
    command.arg("-c").arg(script);
    command
}

#[cfg(windows)]
fn shell(script: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = proc_tree::command("cmd");
    // `/S` makes cmd strip exactly the outer quotes and run the rest as typed; quoting the
    // script the way other programs expect would change what cmd sees.
    command.raw_arg(format!("/D /S /C \"{script}\""));
    command
}

fn collect(mut reader: io::PipeReader, output: Arc<Mutex<TailBuffer>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => output
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_tree_allows_only_itself() {
        let checked = Checked::from_trees(&Ok("t1".to_owned()), &Ok("t1".to_owned()));
        assert_eq!(checked, Checked::Tree("t1".to_owned()));
        assert_eq!(checked.allows("t1"), Ok(()));
        let refused = checked.allows("t2").unwrap_err();
        assert!(
            refused.contains("changed after the last check"),
            "{refused}"
        );
    }

    #[test]
    fn a_tree_that_moved_during_the_check_allows_nothing() {
        let checked = Checked::from_trees(&Ok("t1".to_owned()), &Ok("t2".to_owned()));
        for now in ["t1", "t2"] {
            let refused = checked.allows(now).unwrap_err();
            assert!(
                refused.contains("changed while the last check ran"),
                "{refused}"
            );
        }
    }

    #[test]
    fn an_unread_tree_allows_nothing() {
        let failed = Err("git write-tree failed".to_owned());
        for checked in [
            Checked::from_trees(&failed, &Ok("t1".to_owned())),
            Checked::from_trees(&Ok("t1".to_owned()), &failed),
        ] {
            let refused = checked.allows("t1").unwrap_err();
            assert!(refused.contains("could not record"), "{refused}");
        }
    }
}
