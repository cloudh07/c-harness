use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::git::{clear_git_location, git_output, os_args, run_git};
use crate::lock::{self, SessionLock};
use crate::process::{command, runner_info};
use crate::record::{self, SessionRecord};
use crate::sweep::delete_checkpoints;
use crate::{RunnerInfo, RunnerProcess, RuntimeError, SandboxProvider, session_branch};

const LOCATION: &str = "this machine";
const MAX_SESSION_LEN: usize = 64;
/// Starts the reason git shows for a session's locked worktree.
pub(crate) const LOCK_REASON_PREFIX: &str = "c-harness session ";

/// Runs sessions on this machine, each in its own detached git worktree.
///
/// Local sessions are not sandboxed; the agent runs with your user's permissions. It can read
/// and change anything you can, and the worktree only keeps it out of your own checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProvider {
    /// The repository the session works on.
    pub repo: PathBuf,
    /// The `ch-runner` program: a path, or a bare name looked up on PATH.
    pub runner: PathBuf,
    /// Where session worktrees go, one directory per session. Keep it outside `repo`.
    pub worktrees_dir: PathBuf,
    /// Where session records go, one `<session>.json` per worktree.
    pub sessions_dir: PathBuf,
}

impl LocalProvider {
    /// The absolute path of `session`'s worktree.
    pub fn worktree_path(&self, session: &str) -> Result<PathBuf, RuntimeError> {
        validate_session(session)?;
        // `git -C <repo>` would resolve a relative path against the repository, not against us.
        let dir = std::path::absolute(&self.worktrees_dir).map_err(|source| RuntimeError::Io {
            action: "resolve the worktrees directory",
            source,
        })?;
        Ok(dir.join(session))
    }

    /// Creates a detached worktree of `repo` at `HEAD` for `session` and returns its path.
    ///
    /// A [`SessionRecord`] is written first, so even a crash leaves a way back to the worktree.
    /// The worktree is locked, so `git worktree prune` keeps it when its directory goes missing.
    pub fn create_worktree(&self, session: &str) -> Result<PathBuf, RuntimeError> {
        let path = self.worktree_path(session)?;
        if path.exists() || self.record(session)?.is_some() {
            return Err(RuntimeError::WorktreeExists(path));
        }
        fs::create_dir_all(&self.worktrees_dir).map_err(|source| RuntimeError::Io {
            action: "create the worktrees directory",
            source,
        })?;
        let base_commit = git_output("read the base commit", base_commit_args(&self.repo))?;
        let record = SessionRecord {
            session: session.to_owned(),
            repo: std::path::absolute(&self.repo).map_err(|source| RuntimeError::Io {
                action: "resolve the repository path",
                source,
            })?,
            worktree: path.clone(),
            branch: session_branch(session),
            base_commit,
            created_unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            omp_session_file: None,
        };
        record::create(&self.sessions_dir, &record)?;
        let add = worktree_add_args(&self.repo, &path, session, &record.base_commit);
        if let Err(err) = run_git("create a worktree", add) {
            // The git error is the one worth reporting.
            let _ = record::remove(&self.sessions_dir, session);
            return Err(err);
        }
        Ok(path)
    }

    /// Deletes `session`'s worktree, including changes nobody committed, then its record and
    /// lock file. Stop its runner first: Windows refuses to delete a directory a running process
    /// is using.
    pub fn remove_worktree(&self, session: &str) -> Result<(), RuntimeError> {
        let path = self.worktree_path(session)?;
        // Also drops git's entry when the directory was deleted by hand.
        run_git(
            "remove the worktree",
            worktree_remove_args(&self.repo, &path),
        )?;
        // With no worktree left there is nothing to restore into.
        delete_checkpoints(&self.repo, session)?;
        record::remove(&self.sessions_dir, session)?;
        // A lock file nobody holds is harmless, so a failure here is not worth reporting.
        let _ = lock::remove(&self.sessions_dir, session);
        Ok(())
    }

    /// The record of `session`'s worktree, or `None` when it has none.
    pub fn record(&self, session: &str) -> Result<Option<SessionRecord>, RuntimeError> {
        validate_session(session)?;
        record::read(&self.sessions_dir, session)
    }

    pub(crate) fn runner_program(&self) -> Result<PathBuf, RuntimeError> {
        let bare_name = self
            .runner
            .parent()
            .is_none_or(|p| p.as_os_str().is_empty());
        if bare_name {
            return Ok(self.runner.clone());
        }
        // The runner starts inside the worktree, and a relative program path would be resolved
        // against that directory on some platforms.
        std::path::absolute(&self.runner).map_err(|source| RuntimeError::Io {
            action: "resolve the ch-runner path",
            source,
        })
    }

    pub(crate) fn runner_spawn_error(&self, source: io::Error) -> RuntimeError {
        if source.kind() == io::ErrorKind::NotFound {
            RuntimeError::RunnerNotFound {
                location: LOCATION.to_owned(),
            }
        } else {
            RuntimeError::Spawn {
                program: self.runner.display().to_string(),
                source,
            }
        }
    }
}

impl SandboxProvider for LocalProvider {
    fn label(&self) -> String {
        "This machine (not sandboxed)".to_owned()
    }

    fn check(&self) -> Result<RunnerInfo, RuntimeError> {
        let output = command(self.runner_program()?)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| self.runner_spawn_error(e))?;
        runner_info(LOCATION, &output)
    }

    fn start(&self, session: &str) -> Result<RunnerProcess, RuntimeError> {
        let program = self.runner_program()?;
        validate_session(session)?;
        // Taken before the record exists, so a scan never sees a session half started.
        let Some(lock) = SessionLock::acquire(&self.sessions_dir, session)? else {
            return Err(RuntimeError::SessionInUse);
        };
        let workdir = match self.create_worktree(session) {
            Ok(workdir) => workdir,
            Err(err) => {
                let _ = lock::remove(&self.sessions_dir, session);
                return Err(err);
            }
        };
        match spawn_runner(&program, &workdir) {
            Ok(child) => Ok(RunnerProcess::locked(child, lock)),
            Err(source) => {
                // So that a retry with the same session id can create the worktree again. The
                // spawn error is the one worth reporting, so a cleanup failure is dropped.
                let _ = self.remove_worktree(session);
                Err(self.runner_spawn_error(source))
            }
        }
    }
}

/// Checks that `session` can name a worktree directory on every platform.
pub fn validate_session(session: &str) -> Result<(), RuntimeError> {
    let allowed = (1..=MAX_SESSION_LEN).contains(&session.len())
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if allowed && !is_windows_device_name(session) {
        Ok(())
    } else {
        Err(RuntimeError::InvalidSession)
    }
}

// Windows will not create a file or directory with one of these names, in any letter case.
fn is_windows_device_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            upper.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && upper.as_bytes()[3].is_ascii_digit()
        }
    }
}

fn base_commit_args(repo: &Path) -> Vec<OsString> {
    os_args([&"-C", &repo, &"rev-parse", &"--verify", &"HEAD^{commit}"])
}

// Adds at the commit the record names, not at whatever HEAD is by then.
fn worktree_add_args(repo: &Path, path: &Path, session: &str, commit: &str) -> Vec<OsString> {
    let reason = format!("{LOCK_REASON_PREFIX}{session}");
    os_args([
        &"-C",
        &repo,
        &"worktree",
        &"add",
        &"--detach",
        &"--lock",
        &"--reason",
        &reason,
        &"--",
        &path,
        &commit,
    ])
}

// A locked worktree needs the second --force. It also drops a worktree whose directory is gone.
pub(crate) fn worktree_remove_args(repo: &Path, path: &Path) -> Vec<OsString> {
    os_args([
        &"-C",
        &repo,
        &"worktree",
        &"remove",
        &"--force",
        &"--force",
        &"--",
        &path,
    ])
}

fn serve_args(workdir: &Path) -> Vec<OsString> {
    os_args([&"serve", &"--workdir", &workdir])
}

/// Starts `ch-runner serve` in `workdir` with piped stdio.
pub(crate) fn spawn_runner(program: &Path, workdir: &Path) -> io::Result<Child> {
    let mut runner = command(program);
    clear_git_location(&mut runner);
    runner
        .args(serve_args(workdir))
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, provider, repo_with_commit, worktree_count};

    #[test]
    fn session_ids_are_portable_directory_names() {
        for ok in ["s1", "a-b_C", "COM10", &"x".repeat(64)] {
            assert!(validate_session(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            &"x".repeat(65),
            "a/b",
            "a\\b",
            "..",
            "a b",
            "é",
            "con",
            "Nul",
            "COM1",
            "lpt9",
        ] {
            assert!(validate_session(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn git_and_runner_arguments() {
        let repo = Path::new("repo dir");
        let path = Path::new("wt/-s1");
        assert_eq!(
            base_commit_args(repo),
            ["-C", "repo dir", "rev-parse", "--verify", "HEAD^{commit}"]
        );
        assert_eq!(
            worktree_add_args(repo, path, "s1", "abc123"),
            [
                "-C",
                "repo dir",
                "worktree",
                "add",
                "--detach",
                "--lock",
                "--reason",
                "c-harness session s1",
                "--",
                "wt/-s1",
                "abc123"
            ]
        );
        assert_eq!(
            worktree_remove_args(repo, path),
            [
                "-C", "repo dir", "worktree", "remove", "--force", "--force", "--", "wt/-s1"
            ]
        );
        assert_eq!(serve_args(path), ["serve", "--workdir", "wt/-s1"]);
    }

    #[test]
    fn worktree_path_is_absolute_even_for_a_relative_dir() {
        let provider = LocalProvider {
            repo: "repo".into(),
            runner: "ch-runner".into(),
            worktrees_dir: "worktrees".into(),
            sessions_dir: "sessions".into(),
        };
        let path = provider.worktree_path("s1").unwrap();
        assert!(path.is_absolute());
        assert!(path.ends_with(Path::new("worktrees").join("s1")));
        assert!(provider.worktree_path("../x").is_err());
    }

    #[test]
    fn creates_and_removes_a_detached_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo.clone());

        let path = provider.create_worktree("s1").unwrap();
        assert!(path.join(".git").is_file());
        // core.autocrlf may turn the line ending into CRLF on checkout.
        assert_eq!(
            fs::read_to_string(path.join("README")).unwrap().trim_end(),
            "hello"
        );
        assert_eq!(
            git(&path, &["rev-parse", "HEAD"]),
            git(&repo, &["rev-parse", "HEAD"])
        );
        assert_eq!(git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");
        assert!(
            git(&repo, &["worktree", "list", "--porcelain"])
                .contains("locked c-harness session s1")
        );
        assert!(matches!(
            provider.create_worktree("s1"),
            Err(RuntimeError::WorktreeExists(_))
        ));

        let record = provider.record("s1").unwrap().expect("a record");
        assert_eq!(record.session, "s1");
        assert_eq!(record.repo, std::path::absolute(&repo).unwrap());
        assert_eq!(record.worktree, path);
        assert_eq!(record.branch, "ch/s1");
        assert_eq!(record.base_commit, git(&repo, &["rev-parse", "HEAD"]));
        assert!(record.created_unix_secs > 1_700_000_000);

        fs::write(path.join("scratch.txt"), "uncommitted").unwrap();
        git(&repo, &["update-ref", "refs/c-harness/s1/turn-1", "HEAD"]);
        git(&repo, &["update-ref", "refs/c-harness/s10/turn-1", "HEAD"]);
        provider.remove_worktree("s1").unwrap();
        assert!(!path.exists());
        assert_eq!(worktree_count(&repo), 1);
        assert_eq!(git(&repo, &["status", "--porcelain"]), "");
        assert_eq!(provider.record("s1").unwrap(), None);
        // Its checkpoints go with it; another session's stay.
        let refs = git(
            &repo,
            &["for-each-ref", "--format=%(refname)", "refs/c-harness/"],
        );
        assert_eq!(refs, "refs/c-harness/s10/turn-1");
    }

    #[test]
    fn a_worktree_deleted_by_hand_stays_locked_until_removed() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo.clone());
        let path = provider.create_worktree("s1").unwrap();
        fs::remove_dir_all(&path).unwrap();

        // The lock keeps git's own cleanup from dropping it.
        git(&repo, &["worktree", "prune"]);
        assert_eq!(worktree_count(&repo), 2);
        assert!(provider.record("s1").unwrap().is_some());

        provider.remove_worktree("s1").unwrap();
        assert_eq!(worktree_count(&repo), 1);
        assert_eq!(provider.record("s1").unwrap(), None);
    }

    #[test]
    fn a_record_that_cannot_be_written_stops_the_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let mut provider = provider(root.path(), repo.clone());
        // A file where the records directory should be.
        provider.sessions_dir = root.path().join("sessions-file");
        fs::write(&provider.sessions_dir, "not a directory").unwrap();

        let err = provider.create_worktree("s1").unwrap_err();
        assert!(matches!(err, RuntimeError::Io { .. }), "{err}");
        assert!(!provider.worktree_path("s1").unwrap().exists());
        assert_eq!(worktree_count(&repo), 1);
    }

    #[test]
    fn a_leftover_record_keeps_its_session_id_taken() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo.clone());
        let path = provider.create_worktree("s1").unwrap();
        fs::remove_dir_all(&path).unwrap();
        let before = provider.record("s1").unwrap();

        assert!(matches!(
            provider.create_worktree("s1"),
            Err(RuntimeError::WorktreeExists(_))
        ));
        assert_eq!(provider.record("s1").unwrap(), before);
        assert_eq!(worktree_count(&repo), 2);
    }

    #[test]
    fn a_repository_without_commits_is_a_git_error() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("empty");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let provider = provider(root.path(), repo);
        let err = provider.create_worktree("s1").unwrap_err();
        assert!(matches!(err, RuntimeError::Git { .. }), "{err}");
        assert_eq!(provider.record("s1").unwrap(), None);
    }

    #[test]
    fn missing_runner_leaves_no_worktree_behind() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo);

        let err = provider.start("s1").unwrap_err();
        assert!(matches!(err, RuntimeError::RunnerNotFound { .. }), "{err}");
        assert!(!provider.worktree_path("s1").unwrap().exists());
        assert_eq!(provider.record("s1").unwrap(), None);
        assert!(!root.path().join("sessions").join("s1.lock").exists());
        assert!(matches!(
            provider.check(),
            Err(RuntimeError::RunnerNotFound { .. })
        ));
    }

    #[test]
    fn a_session_open_elsewhere_is_not_started_twice() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo.clone());
        let held = SessionLock::acquire(&provider.sessions_dir, "s1")
            .unwrap()
            .unwrap();

        let err = provider.start("s1").unwrap_err();
        assert!(matches!(err, RuntimeError::SessionInUse), "{err}");
        assert_eq!(provider.record("s1").unwrap(), None);
        assert_eq!(worktree_count(&repo), 1);
        drop(held);
    }
}
