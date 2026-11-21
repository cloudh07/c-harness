//! One lock file per local session, so two windows never drive or discard the same session.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::RuntimeError;

/// Holds a session's `<session>.lock` locked while it lives. The OS drops the lock when the
/// process ends, crash included, so a stale file never blocks anyone.
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
}

impl SessionLock {
    /// Locks `session`, or returns `None` when another handle holds it.
    pub(crate) fn acquire(dir: &Path, session: &str) -> Result<Option<Self>, RuntimeError> {
        fs::create_dir_all(dir).map_err(failed)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path(dir, session))
            .map_err(failed)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(source)) => Err(failed(source)),
        }
    }
}

/// Whether some handle, in this process or another, holds `session`'s lock. Creates nothing.
pub(crate) fn is_held(dir: &Path, session: &str) -> Result<bool, RuntimeError> {
    let file = match File::open(path(dir, session)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(failed(source)),
    };
    match file.try_lock() {
        // Dropping the file releases the lock it just took.
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(source)) => Err(failed(source)),
    }
}

/// Deletes `session`'s lock file. Callers ignore the error: a lock file nobody holds is harmless.
pub(crate) fn remove(dir: &Path, session: &str) -> io::Result<()> {
    match fs::remove_file(path(dir, session)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.lock"))
}

fn failed(source: io::Error) -> RuntimeError {
    RuntimeError::Io {
        action: "lock the session",
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_waits_for_the_first_to_drop() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        assert!(!is_held(&sessions, "s1").unwrap());

        let first = SessionLock::acquire(&sessions, "s1")
            .unwrap()
            .expect("free");
        assert!(SessionLock::acquire(&sessions, "s1").unwrap().is_none());
        assert!(is_held(&sessions, "s1").unwrap());
        // Asking does not take the lock away from its holder.
        assert!(is_held(&sessions, "s1").unwrap());
        assert!(!is_held(&sessions, "s2").unwrap());

        drop(first);
        assert!(!is_held(&sessions, "s1").unwrap());
        assert!(SessionLock::acquire(&sessions, "s1").unwrap().is_some());
    }

    #[test]
    fn asking_about_a_session_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_held(dir.path(), "s1").unwrap());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_holder_can_delete_its_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let lock = SessionLock::acquire(dir.path(), "s1").unwrap().unwrap();
        remove(dir.path(), "s1").unwrap();
        assert!(!dir.path().join("s1.lock").exists());
        drop(lock);
        remove(dir.path(), "s1").unwrap();
    }
}
