//! Picking a local session up again after C-Harness closed while it was open.

use std::path::{Component, Path, Prefix};

use crate::local::spawn_runner;
use crate::lock::{self, SessionLock};
use crate::record::{self, SessionRecord};
use crate::sweep::same_dir;
use crate::{LeftoverKind, LocalProvider, RunnerProcess, RuntimeError, validate_session};

/// The longest omp session file path C-Harness will keep or pass back.
const MAX_OMP_SESSION_FILE_LEN: usize = 1024;

/// Whether `path` may be stored as omp's session file and handed back to omp as
/// `--resume=<path>`: absolute, UTF-8, a `.jsonl` file, no `..`, no control characters, and,
/// on Windows, on a drive letter rather than a network share.
pub fn valid_omp_session_file(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    path.is_absolute()
        && on_a_plain_root(path)
        && text.len() <= MAX_OMP_SESSION_FILE_LEN
        && !text.chars().any(char::is_control)
        && path.extension().is_some_and(|ext| ext == "jsonl")
        && !path.components().any(|part| part == Component::ParentDir)
}

/// `/` on Unix, `C:\` on Windows. A UNC or `\\?\` path could make the app or omp open a
/// network share, which sends your Windows credentials to it.
fn on_a_plain_root(path: &Path) -> bool {
    match path.components().next() {
        Some(Component::RootDir) => true,
        Some(Component::Prefix(prefix)) => matches!(prefix.kind(), Prefix::Disk(_)),
        _ => false,
    }
}

impl LocalProvider {
    /// Starts a runner again in `session`'s worktree, as [`SandboxProvider::start`] would have,
    /// and returns it with the session's record.
    ///
    /// Only a [`LeftoverKind::Live`] session of this provider's repository and worktrees folder
    /// can be resumed. When the runner cannot start, the worktree and record stay as they were.
    ///
    /// [`SandboxProvider::start`]: crate::SandboxProvider::start
    pub fn resume(&self, session: &str) -> Result<(RunnerProcess, SessionRecord), RuntimeError> {
        let program = self.runner_program()?;
        validate_session(session)?;
        let Some(lock) = SessionLock::acquire(&self.sessions_dir, session)? else {
            return Err(RuntimeError::SessionInUse);
        };
        let started = self.resumable(session).and_then(|record| {
            spawn_runner(&program, &record.worktree)
                .map(|child| (child, record))
                .map_err(|source| self.runner_spawn_error(source))
        });
        match started {
            Ok((child, record)) => Ok((RunnerProcess::locked(child, lock), record)),
            Err(err) => {
                // A lock file nobody holds is harmless, so a failure here is not worth reporting.
                let _ = lock::remove(&self.sessions_dir, session);
                Err(err)
            }
        }
    }

    /// Stores `file` as the omp session file of `session`'s record. Returns whether the record
    /// changed: a path that fails [`valid_omp_session_file`], a missing record, or the path the
    /// record already has leave it alone.
    pub fn note_omp_session_file(&self, session: &str, file: &Path) -> Result<bool, RuntimeError> {
        validate_session(session)?;
        if !valid_omp_session_file(file) {
            return Ok(false);
        }
        let Some(mut record) = record::read(&self.sessions_dir, session)? else {
            return Ok(false);
        };
        if record.omp_session_file.as_deref() == Some(file) {
            return Ok(false);
        }
        record.omp_session_file = Some(file.to_path_buf());
        record::write(&self.sessions_dir, &record)?;
        Ok(true)
    }

    /// Stores whether `session` is waiting for an answer, so a restart can say so. Returns
    /// whether the record changed; a missing record or an unchanged value leave it alone.
    pub fn note_needs_you(&self, session: &str, waiting: bool) -> Result<bool, RuntimeError> {
        validate_session(session)?;
        let Some(mut record) = record::read(&self.sessions_dir, session)? else {
            return Ok(false);
        };
        if record.needs_you == waiting {
            return Ok(false);
        }
        record.needs_you = waiting;
        record::write(&self.sessions_dir, &record)?;
        Ok(true)
    }

    /// The record of `session` when its worktree is whole and is where this provider would put
    /// it, so a resumed session lands and cleans up like a new one.
    fn resumable(&self, session: &str) -> Result<SessionRecord, RuntimeError> {
        let found = self.scan(Some(session), None)?.into_iter().next();
        let Some(leftover) = found else {
            return Err(RuntimeError::NotResumable {
                reason: "nothing of it is left",
            });
        };
        let (LeftoverKind::Live, Some(record), Some(entry)) =
            (leftover.kind, leftover.record, leftover.entry)
        else {
            return Err(RuntimeError::NotResumable {
                reason: "its record or its worktree is missing",
            });
        };
        let ours = same_dir(&record.repo, &self.repo)
            && same_dir(&record.worktree, &entry.path)
            && same_dir(&record.worktree, &self.worktree_path(session)?);
        if !ours {
            return Err(RuntimeError::NotResumable {
                reason: "it belongs to another repository or worktrees folder; switch Settings \
                         back to resume it",
            });
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::test_support::{git, provider, repo_with_commit, worktree_count};

    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn only_a_plain_absolute_jsonl_path_is_kept() {
        assert!(valid_omp_session_file(&absolute("a.jsonl")));
        for bad in [
            PathBuf::from("a.jsonl"),
            PathBuf::from("sessions/a.jsonl"),
            absolute("a.json"),
            absolute("a"),
            absolute("x").join("..").join("a.jsonl"),
            absolute("a\nb.jsonl"),
            absolute(&format!("{}.jsonl", "x".repeat(MAX_OMP_SESSION_FILE_LEN))),
            PathBuf::from(r"\\server\share\a.jsonl"),
            PathBuf::from(r"\\?\C:\a.jsonl"),
            PathBuf::from(r"\\?\UNC\server\share\a.jsonl"),
        ] {
            assert!(!valid_omp_session_file(&bad), "{}", bad.display());
        }
    }

    #[test]
    fn a_failed_resume_keeps_the_worktree_and_frees_the_session() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo.clone());
        let worktree = provider.create_worktree("s1").unwrap();
        fs::write(worktree.join("work.txt"), "unsaved").unwrap();

        for _ in 0..2 {
            let err = provider.resume("s1").unwrap_err();
            assert!(matches!(err, RuntimeError::RunnerNotFound { .. }), "{err}");
        }
        assert!(worktree.join("work.txt").exists());
        assert!(provider.record("s1").unwrap().is_some());
        assert_eq!(worktree_count(&repo), 2);
        assert!(!provider.sessions_dir.join("s1.lock").exists());
    }

    #[test]
    fn a_resumed_runner_holds_the_session_in_its_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let mut provider = provider(root.path(), repo);
        provider.create_worktree("s1").unwrap();
        // Any program that starts will do: the lock is what is being checked.
        provider.runner = PathBuf::from("git");

        let (process, record) = provider.resume("s1").unwrap();
        assert_eq!(record.worktree, provider.worktree_path("s1").unwrap());
        assert!(matches!(
            provider.resume("s1"),
            Err(RuntimeError::SessionInUse)
        ));
        drop(process);
        assert!(
            provider
                .leftovers()
                .unwrap()
                .iter()
                .any(|l| l.session == "s1")
        );
    }

    #[test]
    fn only_a_live_session_of_this_repository_resumes() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let mut provider = provider(root.path(), repo.clone());
        let gone = provider.create_worktree("s2").unwrap();
        fs::remove_dir_all(&gone).unwrap();
        provider.create_worktree("s3").unwrap();
        record::remove(&provider.sessions_dir, "s3").unwrap();
        provider.create_worktree("s4").unwrap();

        for session in ["s2", "s3", "s9"] {
            let err = provider.resume(session).unwrap_err();
            assert!(
                matches!(err, RuntimeError::NotResumable { .. }),
                "{session}: {err}"
            );
            assert!(
                !provider
                    .sessions_dir
                    .join(format!("{session}.lock"))
                    .exists()
            );
        }
        let held = SessionLock::acquire(&provider.sessions_dir, "s4")
            .unwrap()
            .unwrap();
        assert!(matches!(
            provider.resume("s4"),
            Err(RuntimeError::SessionInUse)
        ));
        drop(held);

        let elsewhere = root.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        provider.repo = repo_with_commit(&elsewhere);
        let err = provider.resume("s4").unwrap_err();
        assert!(matches!(err, RuntimeError::NotResumable { .. }), "{err}");
        assert!(git(&repo, &["worktree", "list"]).contains("s4"));
    }

    #[test]
    fn the_omp_session_file_is_noted_once_and_only_when_sound() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo);
        provider.create_worktree("s1").unwrap();
        let file = absolute("omp-session.jsonl");

        assert!(
            !provider
                .note_omp_session_file("s1", Path::new("a.jsonl"))
                .unwrap()
        );
        assert!(provider.note_omp_session_file("s1", &file).unwrap());
        assert!(!provider.note_omp_session_file("s1", &file).unwrap());
        assert!(!provider.note_omp_session_file("s9", &file).unwrap());
        let record = provider.record("s1").unwrap().unwrap();
        assert_eq!(record.omp_session_file, Some(file));
        assert!(matches!(
            provider.note_omp_session_file("../x", Path::new("/a.jsonl")),
            Err(RuntimeError::InvalidSession)
        ));
    }

    #[test]
    fn needs_you_is_written_only_when_it_changes() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo);
        provider.create_worktree("s1").unwrap();
        let waiting = |p: &LocalProvider| p.record("s1").unwrap().unwrap().needs_you;

        assert!(!provider.note_needs_you("s1", false).unwrap());
        assert!(provider.note_needs_you("s1", true).unwrap());
        assert!(waiting(&provider));
        assert!(!provider.note_needs_you("s1", true).unwrap());
        assert!(provider.note_needs_you("s1", false).unwrap());
        assert!(!waiting(&provider));
        assert!(!provider.note_needs_you("s9", true).unwrap());
        assert!(matches!(
            provider.note_needs_you("../x", true),
            Err(RuntimeError::InvalidSession)
        ));
    }
}
