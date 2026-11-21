//! The file a local session leaves on disk, so its worktree can be found again after a crash.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::RuntimeError;

/// What C-Harness knows about a local session's worktree. It is written before the worktree is
/// created and removed after the worktree is, as `<session>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session: String,
    /// The repository the worktree belongs to.
    pub repo: PathBuf,
    pub worktree: PathBuf,
    /// The branch land pushes the work to.
    pub branch: String,
    /// The commit the worktree was created at.
    pub base_commit: String,
    pub created_unix_secs: u64,
}

fn path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.json"))
}

/// Writes `record` so that a crash leaves either no file or the whole file.
pub(crate) fn write(dir: &Path, record: &SessionRecord) -> Result<(), RuntimeError> {
    let failed = |source: io::Error| RuntimeError::Io {
        action: "write the session record",
        source,
    };
    fs::create_dir_all(dir).map_err(failed)?;
    let json = serde_json::to_vec_pretty(record).map_err(|e| failed(io::Error::other(e)))?;
    let temp = dir.join(format!(".{}.json.tmp", record.session));
    let written = File::create(&temp)
        .and_then(|mut file| {
            file.write_all(&json)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, path(dir, &record.session)));
    written.map_err(|source| {
        // Only a leftover; the write error is the one worth reporting.
        let _ = fs::remove_file(&temp);
        failed(source)
    })
}

/// The record of `session`, or `None` when there is none.
pub(crate) fn read(dir: &Path, session: &str) -> Result<Option<SessionRecord>, RuntimeError> {
    let failed = |source: io::Error| RuntimeError::Io {
        action: "read the session record",
        source,
    };
    match fs::read(path(dir, session)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| failed(io::Error::new(io::ErrorKind::InvalidData, e))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(failed(source)),
    }
}

/// Removes the record of `session`; a missing record is fine.
pub(crate) fn remove(dir: &Path, session: &str) -> Result<(), RuntimeError> {
    match fs::remove_file(path(dir, session)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(RuntimeError::Io {
            action: "remove the session record",
            source: e,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(session: &str) -> SessionRecord {
        SessionRecord {
            session: session.to_owned(),
            repo: PathBuf::from("/repo"),
            worktree: PathBuf::from("/worktrees").join(session),
            branch: format!("ch/{session}"),
            base_commit: "0123abcd".to_owned(),
            created_unix_secs: 1_700_000_000,
        }
    }

    #[test]
    fn a_record_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("sessions");
        write(&records, &record("s1")).unwrap();
        assert_eq!(read(&records, "s1").unwrap(), Some(record("s1")));

        let mut moved = record("s1");
        moved.base_commit = "4567ef".to_owned();
        write(&records, &moved).unwrap();
        assert_eq!(read(&records, "s1").unwrap(), Some(moved));

        let names: Vec<_> = fs::read_dir(&records)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["s1.json"]);
    }

    #[test]
    fn the_file_keeps_its_field_names() {
        let json = serde_json::to_value(record("s1")).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "base_commit",
                "branch",
                "created_unix_secs",
                "repo",
                "session",
                "worktree"
            ]
        );
    }

    #[test]
    fn a_missing_record_reads_as_none_and_removes_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), "s1").unwrap(), None);
        remove(dir.path(), "s1").unwrap();

        write(dir.path(), &record("s1")).unwrap();
        remove(dir.path(), "s1").unwrap();
        assert_eq!(read(dir.path(), "s1").unwrap(), None);
    }

    #[test]
    fn a_damaged_record_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s1.json"), "{ not json").unwrap();
        let err = read(dir.path(), "s1").unwrap_err();
        assert!(matches!(err, RuntimeError::Io { .. }), "{err}");
    }
}
