//! Finding the local sessions earlier runs left behind, and discarding them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::git::{git_output, git_with_input, os_args, run_git};
use crate::local::{LOCK_REASON_PREFIX, worktree_remove_args};
use crate::lock::{self, SessionLock};
use crate::record::{self, SessionRecord};
use crate::{LocalProvider, RuntimeError, validate_session};

/// What is left of a session an earlier run did not clean up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftoverKind {
    /// The record and the worktree are both there.
    Live,
    /// Git still lists the worktree, but its folder is gone.
    FolderGone,
    /// The record is there, but git no longer lists the worktree.
    NoWorktree,
    /// The record names a repository git cannot read, for example one moved or deleted.
    RepoUnreadable,
    /// Git lists a C-Harness worktree that has no record.
    NoRecord,
    /// The record cannot be read.
    Damaged,
}

/// A worktree as `git worktree list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The repository whose list it is in.
    pub repo: PathBuf,
    pub path: PathBuf,
}

/// A session an earlier run did not clean up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    pub session: String,
    pub kind: LeftoverKind,
    pub record: Option<SessionRecord>,
    pub entry: Option<WorktreeEntry>,
}

impl Leftover {
    /// Where the session's worktree is or was.
    pub fn worktree(&self) -> Option<&Path> {
        self.entry
            .as_ref()
            .map(|entry| entry.path.as_path())
            .or_else(|| self.record.as_ref().map(|record| record.worktree.as_path()))
    }
}

impl LocalProvider {
    /// The sessions earlier runs left behind, in session id order. Sessions another window
    /// has open are skipped. Runs git once per repository, so call it off the UI thread.
    pub fn leftovers(&self) -> Result<Vec<Leftover>, RuntimeError> {
        self.scan(None)
    }

    /// Deletes what `session` left behind: its worktree with any uncommitted changes, its
    /// checkpoint refs, its record and its lock file. Its `ch/<session>` branch stays.
    ///
    /// Returns the record's worktree folder when git no longer knew it and it was kept: only a
    /// folder git lists as this session's worktree is ever deleted.
    pub fn discard(&self, session: &str) -> Result<Option<PathBuf>, RuntimeError> {
        validate_session(session)?;
        let Some(held) = SessionLock::acquire(&self.sessions_dir, session)? else {
            return Err(RuntimeError::SessionInUse);
        };
        let found = self.scan(Some(session))?.into_iter().next();
        let kept = match found {
            Some(leftover) => self.discard_leftover(&leftover)?,
            None => None,
        };
        // A lock file nobody holds is harmless, so a failure here is not worth reporting.
        let _ = lock::remove(&self.sessions_dir, session);
        drop(held);
        Ok(kept)
    }

    fn discard_leftover(&self, leftover: &Leftover) -> Result<Option<PathBuf>, RuntimeError> {
        if let Some(entry) = &leftover.entry {
            run_git(
                "remove the worktree",
                worktree_remove_args(&entry.repo, &entry.path),
            )?;
        }
        let repo = match (&leftover.entry, &leftover.record) {
            (Some(entry), _) => Some(&entry.repo),
            (None, Some(record)) if leftover.kind != LeftoverKind::RepoUnreadable => {
                Some(&record.repo)
            }
            _ => None,
        };
        if let Some(repo) = repo {
            delete_checkpoints(repo, &leftover.session)?;
        }
        record::remove(&self.sessions_dir, &leftover.session)?;
        Ok(match (&leftover.entry, &leftover.record) {
            (None, Some(record)) if record.worktree.exists() => Some(record.worktree.clone()),
            _ => None,
        })
    }

    fn scan(&self, only: Option<&str>) -> Result<Vec<Leftover>, RuntimeError> {
        let records = self.read_records(only)?;
        let mut repos = Vec::new();
        if !self.repo.as_os_str().is_empty() {
            repos.push(self.repo.clone());
        }
        repos.extend(records.values().flatten().map(|record| record.repo.clone()));

        let mut seen = Vec::new();
        let mut unreadable = Vec::new();
        let mut entries = BTreeMap::new();
        for repo in repos {
            let key = repo_key(&repo);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            let Some(listed) = list_worktrees(&repo)? else {
                unreadable.push(key);
                continue;
            };
            for worktree in listed {
                let Some(session) = session_of(&worktree, &self.worktrees_dir) else {
                    continue;
                };
                if only.is_some_and(|only| only != session) {
                    continue;
                }
                entries.entry(session).or_insert_with(|| WorktreeEntry {
                    repo: repo.clone(),
                    path: worktree.path,
                });
            }
        }

        let ids: BTreeSet<String> = records.keys().chain(entries.keys()).cloned().collect();
        let mut leftovers = Vec::new();
        for session in ids {
            // A lock that cannot be checked is treated as held: better hidden than discarded.
            if only.is_none() && lock::is_held(&self.sessions_dir, &session).unwrap_or(true) {
                continue;
            }
            let record = records.get(&session);
            let entry = entries.remove(&session);
            let kind = kind_of(record, entry.as_ref(), &unreadable);
            leftovers.push(Leftover {
                session,
                kind,
                record: record.cloned().flatten(),
                entry,
            });
        }
        Ok(leftovers)
    }

    /// Every record by session id; `None` for one that cannot be read.
    fn read_records(
        &self,
        only: Option<&str>,
    ) -> Result<BTreeMap<String, Option<SessionRecord>>, RuntimeError> {
        let ids = match only {
            Some(session) => vec![session.to_owned()],
            None => record_ids(&self.sessions_dir)?,
        };
        let mut records = BTreeMap::new();
        for session in ids {
            match record::read(&self.sessions_dir, &session) {
                Ok(None) => {}
                Ok(Some(record)) if record.session == session => {
                    records.insert(session, Some(record));
                }
                _ => {
                    records.insert(session, None);
                }
            }
        }
        Ok(records)
    }
}

fn kind_of(
    record: Option<&Option<SessionRecord>>,
    entry: Option<&WorktreeEntry>,
    unreadable: &[PathBuf],
) -> LeftoverKind {
    match (record, entry) {
        (Some(None), _) => LeftoverKind::Damaged,
        (None, _) => LeftoverKind::NoRecord,
        (Some(Some(_)), Some(entry)) if entry.path.is_dir() => LeftoverKind::Live,
        (Some(Some(_)), Some(_)) => LeftoverKind::FolderGone,
        (Some(Some(record)), None) if unreadable.contains(&repo_key(&record.repo)) => {
            LeftoverKind::RepoUnreadable
        }
        (Some(Some(_)), None) => LeftoverKind::NoWorktree,
    }
}

fn record_ids(dir: &Path) -> Result<Vec<String>, RuntimeError> {
    let failed = |source: io::Error| RuntimeError::Io {
        action: "read the sessions folder",
        source,
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(failed(source)),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let name = entry.map_err(failed)?.file_name();
        let Some(session) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if validate_session(session).is_ok() {
            ids.push(session.to_owned());
        }
    }
    Ok(ids)
}

/// One way to name a repository, so the same one is not listed twice.
fn repo_key(repo: &Path) -> PathBuf {
    fs::canonicalize(repo)
        .or_else(|_| std::path::absolute(repo))
        .unwrap_or_else(|_| repo.to_owned())
}

#[derive(Debug, PartialEq, Eq)]
struct Listed {
    path: PathBuf,
    /// The lock reason, empty when locked without one.
    locked: Option<String>,
}

/// The repository's linked worktrees, or `None` when git cannot read it.
fn list_worktrees(repo: &Path) -> Result<Option<Vec<Listed>>, RuntimeError> {
    let args = os_args([&"-C", &repo, &"worktree", &"list", &"--porcelain"]);
    match git_output("list worktrees", args) {
        Ok(porcelain) => Ok(Some(parse_worktrees(&porcelain))),
        Err(RuntimeError::Git { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

// Not `-z`: it needs git 2.36. Paths come raw, one per line.
fn parse_worktrees(porcelain: &str) -> Vec<Listed> {
    let mut listed: Vec<Listed> = Vec::new();
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            listed.push(Listed {
                path: PathBuf::from(path),
                locked: None,
            });
        } else if let Some(last) = listed.last_mut() {
            if line == "locked" {
                last.locked = Some(String::new());
            } else if let Some(reason) = line.strip_prefix("locked ") {
                last.locked = Some(reason.to_owned());
            }
        }
    }
    // The first entry is the main worktree.
    listed.into_iter().skip(1).collect()
}

/// The session a worktree belongs to: the id in its lock reason, else its folder name when it
/// sits in `worktrees_dir`. Worktrees from before the lock reason have only the folder name.
fn session_of(listed: &Listed, worktrees_dir: &Path) -> Option<String> {
    let from_reason = listed
        .locked
        .as_deref()
        .and_then(|reason| reason.strip_prefix(LOCK_REASON_PREFIX));
    let session = if let Some(session) = from_reason {
        session
    } else {
        let parent = listed.path.parent()?;
        if !same_dir(parent, worktrees_dir) {
            return None;
        }
        listed.path.file_name()?.to_str()?
    };
    validate_session(session).ok()?;
    Some(session.to_owned())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Where a session's checkpoints live: `refs/c-harness/<session>/`.
pub(crate) fn checkpoint_prefix(session: &str) -> String {
    format!("refs/c-harness/{session}/")
}

fn delete_checkpoints(repo: &Path, session: &str) -> Result<(), RuntimeError> {
    let prefix = checkpoint_prefix(session);
    let list = os_args([&"-C", &repo, &"for-each-ref", &"--format=%(refname)", &prefix]);
    let refs = git_output("list the session's checkpoints", list)?;
    let input: String = refs
        .lines()
        .filter(|name| name.starts_with(&prefix))
        .flat_map(|name| ["delete ", name, "\n"])
        .collect();
    if input.is_empty() {
        return Ok(());
    }
    let delete = os_args([&"-C", &repo, &"update-ref", &"--stdin"]);
    git_with_input("delete the session's checkpoints", delete, input.as_bytes()).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, provider, repo_with_commit, worktree_count};

    fn listed(path: &str, locked: Option<&str>) -> Listed {
        Listed {
            path: PathBuf::from(path),
            locked: locked.map(str::to_owned),
        }
    }

    #[test]
    fn the_list_skips_the_main_worktree_and_keeps_lock_reasons() {
        let porcelain = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                         worktree /wt/s1\nHEAD abc\ndetached\nlocked c-harness session s1\n\n\
                         worktree /wt/s 2\nHEAD abc\ndetached\nlocked\n\n\
                         worktree /other\nHEAD abc\ndetached\n\
                         prunable gitdir file points to non-existent location";
        assert_eq!(
            parse_worktrees(porcelain),
            [
                listed("/wt/s1", Some("c-harness session s1")),
                listed("/wt/s 2", Some("")),
                listed("/other", None),
            ]
        );
        assert_eq!(parse_worktrees(""), []);
    }

    #[test]
    fn a_worktree_belongs_to_the_session_in_its_reason_or_folder() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("worktrees");
        let at = |name: &str| ours.join(name).to_string_lossy().into_owned();
        let elsewhere = dir.path().join("mine").join("s7");
        let elsewhere = elsewhere.to_string_lossy();

        let by_reason = listed(&elsewhere, Some("c-harness session s1"));
        assert_eq!(session_of(&by_reason, &ours).as_deref(), Some("s1"));
        let bad_reason = listed(&at("x"), Some("c-harness session ../x"));
        assert_eq!(session_of(&bad_reason, &ours), None);
        assert_eq!(
            session_of(&listed(&at("s7"), None), &ours).as_deref(),
            Some("s7")
        );
        let other_reason = listed(&at("s8"), Some("on a USB disk"));
        assert_eq!(session_of(&other_reason, &ours).as_deref(), Some("s8"));
        assert_eq!(session_of(&listed(&elsewhere, None), &ours), None);
        assert_eq!(session_of(&listed(&at("CON"), None), &ours), None);
    }

    fn kinds(provider: &LocalProvider) -> Vec<(String, LeftoverKind)> {
        provider
            .leftovers()
            .unwrap()
            .into_iter()
            .map(|leftover| (leftover.session, leftover.kind))
            .collect()
    }

    fn refs(repo: &Path) -> String {
        git(repo, &["for-each-ref", "--format=%(refname)"])
    }

    #[test]
    fn the_scan_names_each_kind_and_discard_clears_it() {
        use LeftoverKind::{Damaged, FolderGone, Live, NoRecord, NoWorktree, RepoUnreadable};
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let provider = provider(root.path(), repo.clone());
        let path_str = |path: &Path| path.to_str().unwrap().to_owned();

        provider.create_worktree("s1").unwrap();
        let gone = provider.create_worktree("s2").unwrap();
        fs::remove_dir_all(&gone).unwrap();
        let unlisted = provider.create_worktree("s3").unwrap();
        git(
            &repo,
            &["worktree", "remove", "--force", "--force", &path_str(&unlisted)],
        );
        fs::create_dir_all(&unlisted).unwrap();
        fs::write(unlisted.join("notes.txt"), "mine").unwrap();
        provider.create_worktree("s4").unwrap();
        record::remove(&provider.sessions_dir, "s4").unwrap();
        fs::write(provider.sessions_dir.join("s5.json"), "{ not json").unwrap();
        let moved = SessionRecord {
            session: "s6".to_owned(),
            repo: root.path().join("gone"),
            worktree: provider.worktree_path("s6").unwrap(),
            branch: "ch/s6".to_owned(),
            base_commit: "abc".to_owned(),
            created_unix_secs: 1,
        };
        record::write(&provider.sessions_dir, &moved).unwrap();
        // From before worktrees were locked.
        let old = provider.worktree_path("s7").unwrap();
        git(&repo, &["worktree", "add", "--detach", &path_str(&old)]);
        // The user's own worktree, and a session another window has open.
        let mine = root.path().join("mine");
        git(&repo, &["worktree", "add", "--detach", &path_str(&mine)]);
        provider.create_worktree("s8").unwrap();
        let held = SessionLock::acquire(&provider.sessions_dir, "s8")
            .unwrap()
            .unwrap();

        let expected = [
            ("s1", Live),
            ("s2", FolderGone),
            ("s3", NoWorktree),
            ("s4", NoRecord),
            ("s5", Damaged),
            ("s6", RepoUnreadable),
            ("s7", NoRecord),
        ];
        let expected: Vec<_> = expected.map(|(id, kind)| (id.to_owned(), kind)).into();
        assert_eq!(kinds(&provider), expected);

        // Discard keeps the branch and only this session's checkpoints go.
        git(&repo, &["branch", "ch/s1"]);
        git(&repo, &["update-ref", "refs/c-harness/s1/turn-1", "HEAD"]);
        git(&repo, &["update-ref", "refs/c-harness/s10/turn-1", "HEAD"]);
        assert_eq!(provider.discard("s1").unwrap(), None);
        assert!(!provider.worktree_path("s1").unwrap().exists());
        let after = refs(&repo);
        assert!(after.contains("refs/heads/ch/s1"), "{after}");
        assert!(after.contains("refs/c-harness/s10/turn-1"), "{after}");
        assert!(!after.contains("refs/c-harness/s1/"), "{after}");

        assert!(matches!(
            provider.discard("s8"),
            Err(RuntimeError::SessionInUse)
        ));
        drop(held);

        // A folder git does not list as the worktree is kept and handed back.
        assert_eq!(provider.discard("s3").unwrap(), Some(unlisted.clone()));
        assert!(unlisted.join("notes.txt").exists());

        for session in ["s2", "s4", "s5", "s6", "s7", "s8"] {
            provider.discard(session).unwrap();
        }
        assert_eq!(kinds(&provider), []);
        assert_eq!(worktree_count(&repo), 2);
        assert!(mine.is_dir());
        assert_eq!(provider.discard("s99").unwrap(), None);
        let lock_files: Vec<_> = fs::read_dir(&provider.sessions_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(lock_files.is_empty(), "{lock_files:?}");
    }

    #[test]
    fn nothing_is_left_without_records_or_a_repository() {
        let root = tempfile::tempdir().unwrap();
        let mut provider = provider(root.path(), PathBuf::new());
        assert_eq!(kinds(&provider), []);
        provider.repo = root.path().join("not-a-repo");
        assert_eq!(kinds(&provider), []);
        assert!(matches!(
            provider.discard("../x"),
            Err(RuntimeError::InvalidSession)
        ));
    }
}
