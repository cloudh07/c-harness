//! Private temporary directories outside the working tree.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, process};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

const CREATE_ATTEMPTS: usize = 16;

/// A directory only this user can read, removed with everything in it on drop.
#[derive(Debug)]
pub struct PrivateDir {
    path: PathBuf,
}

impl PrivateDir {
    /// Creates a fresh directory under the system temp dir.
    pub fn create(prefix: &str) -> io::Result<Self> {
        let base = env::temp_dir();
        for _ in 0..CREATE_ATTEMPTS {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let name = format!("{prefix}-{}-{}-{nanos}", process::id(), unique());
            let path = base.join(name);
            match create_private_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not pick an unused temp directory name",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A path inside this directory that nothing else in this process has been given.
    pub fn unique_path(&self, stem: &str) -> PathBuf {
        self.path.join(format!("{}-{stem}", unique()))
    }

    /// Writes a new file readable only by this user.
    pub fn write_file(&self, stem: &str, contents: &[u8]) -> io::Result<PathBuf> {
        let path = self.unique_path(stem);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(contents)?;
        Ok(path)
    }

    /// Fails when this directory is inside `workdir`, where the agent could read what it holds.
    pub fn ensure_outside(&self, workdir: &Path) -> io::Result<()> {
        let dir = fs::canonicalize(&self.path)?;
        let workdir = fs::canonicalize(workdir)?;
        if dir.starts_with(&workdir) {
            return Err(io::Error::other(format!(
                "the temp directory {} is inside the working tree; set TMPDIR (TEMP on Windows) \
                 to a directory outside it",
                self.path.display()
            )));
        }
        Ok(())
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        // Nothing useful can be done with a failure here; the directory is in the temp dir.
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn unique() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_itself_on_drop() {
        let dir = PrivateDir::create("ch-runner-test").unwrap();
        let file = dir.write_file("note", b"x").unwrap();
        let path = dir.path().to_owned();
        assert_eq!(fs::read(&file).unwrap(), b"x");
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn never_reuses_a_file_name() {
        let dir = PrivateDir::create("ch-runner-test").unwrap();
        let a = dir.write_file("same", b"a").unwrap();
        let b = dir.write_file("same", b"b").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn refuses_a_workdir_that_contains_it() {
        let dir = PrivateDir::create("ch-runner-test").unwrap();
        assert!(dir.ensure_outside(dir.path()).is_err());
        assert!(dir.ensure_outside(&env::temp_dir()).is_err());
        let other = tempfile::tempdir().unwrap();
        let elsewhere = other.path().join("work");
        fs::create_dir(&elsewhere).unwrap();
        assert!(dir.ensure_outside(&elsewhere).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = PrivateDir::create("ch-runner-test").unwrap();
        let mode = fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let file = dir.write_file("secret", b"x").unwrap();
        let mode = fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
