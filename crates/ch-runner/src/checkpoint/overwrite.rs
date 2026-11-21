//! What a restore would write over that no checkpoint holds.
//!
//! `read-tree -m -u` overwrites an ignored file, and the checkpoint taken before a restore leaves
//! ignored files out, so such a file would be lost. A restore checks first and refuses. The check
//! fails closed: anything it cannot account for counts as in the way.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use crate::git::{self, Git};

/// The most entries read inside a folder the restore turns into a file.
const MAX_FOLDER_ENTRIES: usize = 10_000;
/// The most paths a refusal names.
const NAMED: usize = 5;
/// The most bytes of changed paths read. Over it, the restore is refused.
const CHANGES_BUDGET: usize = 16 * 1024 * 1024;

/// Refuses a restore from tree `current` to tree `target` that would overwrite or delete
/// something on disk that `current` does not hold.
pub(super) fn check(git: &Git, current: &str, target: &str) -> Result<(), String> {
    check_within(git, current, target, CHANGES_BUDGET)
}

/// [`check`], reading at most `budget` bytes of changed paths.
pub(super) fn check_within(
    git: &Git,
    current: &str,
    target: &str,
    budget: usize,
) -> Result<(), String> {
    let mut command = git.command();
    command.args([
        "diff-tree",
        "-r",
        "-z",
        "--no-renames",
        "--name-status",
        current,
        target,
    ]);
    let output = git::run_within(command, "diff-tree", budget)?.ok_or_else(|| {
        "nothing was restored: it changes too many paths to check what it would write over"
            .to_owned()
    })?;
    let (added, deleted) = changes(&output)?;
    let blocked = in_the_way(git.workdir(), &added, &deleted);
    if blocked.is_empty() {
        Ok(())
    } else {
        Err(refusal(&blocked))
    }
}

/// The paths `-z --name-status` output adds and deletes.
fn changes(output: &[u8]) -> Result<(Vec<&str>, HashSet<&str>), String> {
    let mut fields = output.split(|&b| b == 0);
    let mut added = Vec::new();
    let mut deleted = HashSet::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        if status != b"A" && status != b"D" {
            continue;
        }
        let path = std::str::from_utf8(path)
            .map_err(|_| "nothing was restored: a path in it is not UTF-8".to_owned())?;
        if status == b"A" {
            added.push(path);
        } else {
            deleted.insert(path);
        }
    }
    Ok((added, deleted))
}

fn in_the_way(root: &Path, added: &[&str], deleted: &HashSet<&str>) -> Vec<String> {
    let mut blocked = Vec::new();
    let mut folders = HashSet::new();
    for &path in added {
        // A file where the target needs a folder.
        for (at, _) in path.match_indices('/') {
            let folder = &path[..at];
            if folders.insert(folder) && is_file(root, folder) && occupied(root, folder, deleted) {
                blocked.push(folder.to_owned());
            }
        }
        let lost = match fs::symlink_metadata(root.join(path)) {
            Err(_) => false,
            Ok(meta) if meta.is_dir() => !holds_only(root, path, deleted),
            Ok(_) => occupied(root, path, deleted),
        };
        if lost {
            blocked.push(path.to_owned());
        }
    }
    blocked.sort();
    blocked.dedup();
    blocked
}

fn is_file(root: &Path, path: &str) -> bool {
    fs::symlink_metadata(root.join(path)).is_ok_and(|meta| !meta.is_dir())
}

/// Whether `path`, which is on disk, would be lost. On a disk that ignores case, a name that
/// matches only in case is the tracked file the restore deletes first.
fn occupied(root: &Path, path: &str, deleted: &HashSet<&str>) -> bool {
    if deleted.contains(path) {
        return false;
    }
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    let exact = match fs::read_dir(root.join(parent)) {
        Ok(entries) => entries
            .flatten()
            .any(|entry| entry.file_name().as_os_str() == OsStr::new(name)),
        Err(_) => true,
    };
    exact || !deleted.iter().any(|d| d.eq_ignore_ascii_case(path))
}

/// Whether every file under `folder` is one the restore deletes, so git can replace the folder
/// with a file and lose nothing.
fn holds_only(root: &Path, folder: &str, deleted: &HashSet<&str>) -> bool {
    let mut pending = vec![folder.to_owned()];
    let mut seen = 0;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(root.join(&dir)) else {
            return false;
        };
        for entry in entries {
            seen += 1;
            let Ok(entry) = entry else { return false };
            let file_name = entry.file_name();
            let (Ok(kind), Some(name)) = (entry.file_type(), file_name.to_str()) else {
                return false;
            };
            if seen > MAX_FOLDER_ENTRIES {
                return false;
            }
            let path = format!("{dir}/{name}");
            if kind.is_dir() {
                pending.push(path);
            } else if !deleted.contains(path.as_str()) {
                return false;
            }
        }
    }
    true
}

fn refusal(blocked: &[String]) -> String {
    let named: Vec<String> = blocked
        .iter()
        .take(NAMED)
        .map(|path| format!("{path:?}"))
        .collect();
    let more = match blocked.len().saturating_sub(NAMED) {
        0 => String::new(),
        n => format!(" and {n} more"),
    };
    format!(
        "nothing was restored: it would write over {}{more}, which no checkpoint holds, such as \
         an ignored file. Move it aside and try again",
        named.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_added_and_deleted_paths() {
        let output = b"A\0new.txt\0M\0kept.txt\0D\0dir/old.txt\0T\0link\0";
        let (added, deleted) = changes(output).unwrap();
        assert_eq!(added, ["new.txt"]);
        assert_eq!(deleted, HashSet::from(["dir/old.txt"]));
    }

    #[test]
    fn a_path_that_is_not_utf8_fails_closed() {
        assert!(changes(b"A\0bad\xff\0").is_err());
        assert!(
            changes(b"M\0bad\xff\0").is_ok(),
            "a modified path is never checked"
        );
    }

    #[test]
    fn names_a_few_paths_and_counts_the_rest() {
        let paths: Vec<String> = (1..=7).map(|n| format!("f{n}")).collect();
        let text = refusal(&paths);
        assert!(text.contains(r#""f5" and 2 more"#), "{text}");
        assert!(!text.contains("f6"), "{text}");
    }
}
