//! The usage file of one session: a JSON line per model turn, appended as turns end.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use ch_core::UsageRow;

pub struct Ledger {
    /// `None` when there is nowhere to write.
    path: Option<PathBuf>,
    written: usize,
    /// The row count of the last write, while writes keep failing.
    failed_at: Option<usize>,
    /// A problem found before the first sync, reported by it.
    pending: Option<String>,
}

impl Ledger {
    /// The file `<dir>/<session>.jsonl`. Without a `dir`, or with a session id that cannot name
    /// a file, rows are counted but not written.
    pub fn new(dir: Option<&Path>, session: &str) -> Self {
        let named = ch_runtime::validate_session(session).is_ok();
        Self {
            path: dir
                .filter(|_| named)
                .map(|dir| dir.join(format!("{session}.jsonl"))),
            written: 0,
            failed_at: None,
            pending: None,
        }
    }

    /// The ledger of a resumed session, with the rows already in its file. Lines that do not
    /// read as a row are skipped, and so is a turn a retry wrote again. A file that cannot be
    /// read is not written to either, so two runs never number turns over each other.
    pub fn resumed(dir: Option<&Path>, session: &str) -> (Self, Vec<UsageRow>) {
        let mut ledger = Self::new(dir, session);
        let Some(path) = ledger.path.as_deref() else {
            return (ledger, Vec::new());
        };
        let rows = match fs::read_to_string(path) {
            Ok(text) => {
                let mut seen = HashSet::new();
                text.lines()
                    .filter_map(|line| serde_json::from_str::<UsageRow>(line).ok())
                    .filter(|row| seen.insert(row.turn))
                    .collect()
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                ledger.path = None;
                ledger.pending = Some(format!(
                    "Could not read the saved token usage, so it is not saved this time: {e}"
                ));
                Vec::new()
            }
        };
        ledger.written = rows.len();
        (ledger, rows)
    }

    #[cfg(test)]
    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Appends the rows not written yet. After a failure the same rows are tried again once a
    /// new turn ends, not on every tick, and the failure is reported once until a write works.
    pub fn sync(&mut self, rows: &[UsageRow]) -> Option<String> {
        if let Some(problem) = self.pending.take() {
            return Some(problem);
        }
        if self.failed_at.is_some_and(|count| rows.len() <= count) {
            return None;
        }
        let first_failure = self.failed_at.is_none();
        self.write(rows).err().filter(|_| first_failure).map(|e| {
            format!("Could not save token usage. It is tried again after the next turn: {e}")
        })
    }

    /// Like `sync`, but tries a failed write again now and reports every failure. For the
    /// last write of a session.
    pub fn flush(&mut self, rows: &[UsageRow]) -> Option<String> {
        if let Some(problem) = self.pending.take() {
            return Some(problem);
        }
        self.write(rows)
            .err()
            .map(|e| format!("Could not save token usage before the session closed: {e}"))
    }

    fn write(&mut self, rows: &[UsageRow]) -> Result<(), String> {
        let Some(fresh) = rows.get(self.written..).filter(|fresh| !fresh.is_empty()) else {
            return Ok(());
        };
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let written = append(path, fresh);
        if written.is_ok() {
            self.written = rows.len();
            self.failed_at = None;
        } else {
            self.failed_at = Some(rows.len());
        }
        written
    }
}

/// The rows start on a new line if a write cut off by a full disk or a crash left half a line.
fn append(path: &Path, rows: &[UsageRow]) -> Result<(), String> {
    let mut text = String::from(if ends_mid_line(path) { "\n" } else { "" });
    for row in rows {
        text.push_str(&serde_json::to_string(row).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| e.to_string())
}

fn ends_mid_line(path: &Path) -> bool {
    let mut last = [0];
    File::open(path)
        .and_then(|mut file| {
            file.seek(SeekFrom::End(-1))?;
            file.read_exact(&mut last)
        })
        .is_ok_and(|()| last[0] != b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use ch_core::UsageStatus;

    fn row(turn: u32) -> UsageRow {
        UsageRow {
            turn,
            at_ms: u64::from(turn) * 10,
            provider: Some("anthropic".into()),
            model: None,
            input: Some(5),
            output: None,
            cache_read: None,
            cache_write: None,
            cost_usd: None,
            status: UsageStatus::Partial,
        }
    }

    fn lines(path: &Path) -> Vec<UsageRow> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn appends_only_rows_not_written_yet() {
        let dir = tempfile::tempdir().unwrap();
        let usage_dir = dir.path().join("usage");
        let mut ledger = Ledger::new(Some(&usage_dir), "s1");
        let path = usage_dir.join("s1.jsonl");
        assert_eq!(ledger.path(), Some(path.as_path()));

        assert_eq!(ledger.sync(&[]), None);
        assert!(!path.exists());

        let mut rows = vec![row(1)];
        assert_eq!(ledger.sync(&rows), None);
        assert_eq!(ledger.sync(&rows), None);
        rows.push(row(2));
        rows.push(row(3));
        assert_eq!(ledger.sync(&rows), None);
        assert_eq!(lines(&path), rows);
    }

    #[test]
    fn keeps_rows_already_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&row(1)).unwrap()),
        )
        .unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        assert_eq!(ledger.sync(&[row(7)]), None);
        assert_eq!(lines(&path), vec![row(1), row(7)]);
    }

    #[test]
    fn a_failed_write_is_reported_once_and_tried_again_after_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        // A folder where the file should be makes the open fail.
        fs::create_dir(&path).unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        let problem = ledger.sync(&[row(1)]).expect("the write must fail");
        assert!(problem.contains("token usage"), "{problem}");
        assert_eq!(ledger.sync(&[row(1)]), None, "no retry on every tick");
        assert_eq!(ledger.sync(&[row(1), row(2)]), None, "still failing");

        fs::remove_dir(&path).unwrap();
        assert_eq!(ledger.sync(&[row(1), row(2)]), None, "waits for a new turn");
        assert!(!path.exists());
        let rows = [row(1), row(2), row(3)];
        assert_eq!(ledger.sync(&rows), None);
        assert_eq!(lines(&path), rows);
    }

    #[test]
    fn a_flush_tries_a_failed_write_again_without_a_new_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        fs::create_dir(&path).unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        assert!(ledger.sync(&[row(1)]).is_some());
        fs::remove_dir(&path).unwrap();
        assert_eq!(ledger.flush(&[row(1)]), None);
        assert_eq!(lines(&path), [row(1)]);
    }

    #[test]
    fn a_retry_starts_on_a_new_line_after_a_cut_off_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        fs::create_dir(&path).unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        assert!(ledger.sync(&[row(1)]).is_some());
        fs::remove_dir(&path).unwrap();
        // What a write cut off by a full disk leaves behind.
        fs::write(&path, "{\"turn\":1,\"at_").unwrap();
        assert_eq!(ledger.flush(&[row(1)]), None);
        let (_, rows) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(rows, [row(1)]);
    }

    #[test]
    fn a_turn_written_twice_by_a_retry_is_read_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        // Turns 1 and 2 failed after turn 1 landed whole, and the retry wrote both.
        let line = |turn| serde_json::to_string(&row(turn)).unwrap();
        fs::write(&path, format!("{}\n{}\n{}\n", line(1), line(1), line(2))).unwrap();
        let (_, rows) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(rows, [row(1), row(2)]);
    }

    #[test]
    fn a_resumed_ledger_starts_on_a_new_line_after_a_cut_off_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        // An earlier run stopped partway through turn 2.
        let first = serde_json::to_string(&row(1)).unwrap();
        fs::write(&path, format!("{first}\n{{\"turn\":2,\"at_")).unwrap();
        let (mut ledger, mut rows) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(rows, [row(1)]);
        rows.push(row(2));
        assert_eq!(ledger.sync(&rows), None);
        let (_, read_back) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(read_back, [row(1), row(2)]);
    }

    #[test]
    fn a_flush_that_still_fails_says_so() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("s1.jsonl")).unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        assert!(ledger.sync(&[row(1)]).is_some());
        let problem = ledger.flush(&[row(1)]).expect("the last write fails too");
        assert!(problem.contains("token usage"), "{problem}");
    }

    #[test]
    fn a_session_id_that_cannot_name_a_file_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../s1", "a/b", "", "CON"] {
            let mut ledger = Ledger::new(Some(dir.path()), bad);
            assert_eq!(ledger.path(), None, "{bad}");
            assert_eq!(ledger.sync(&[row(1)]), None);
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_resumed_ledger_reads_its_rows_back_and_appends_after_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let first = serde_json::to_string(&row(1)).unwrap();
        let second = serde_json::to_string(&row(2)).unwrap();
        fs::write(
            &path,
            format!(
                "{first}
not json
{{\"turn\":\"x\"}}
{second}
"
            ),
        )
        .unwrap();

        let (mut ledger, mut rows) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(rows, vec![row(1), row(2)]);
        assert_eq!(ledger.sync(&rows), None);
        rows.push(row(3));
        assert_eq!(ledger.sync(&rows), None);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(text.ends_with(&format!(
            "{}
",
            serde_json::to_string(&row(3)).unwrap()
        )));
    }

    #[test]
    fn a_resumed_ledger_without_a_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ledger, rows) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(rows, []);
        assert_eq!(ledger.sync(&[row(1)]), None);
        assert_eq!(lines(&dir.path().join("s1.jsonl")), vec![row(1)]);

        let (_, rows) = Ledger::resumed(None, "s1");
        assert_eq!(rows, []);
    }

    #[test]
    fn a_resumed_ledger_it_cannot_read_reports_once_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("s1.jsonl")).unwrap();
        let (mut ledger, rows) = Ledger::resumed(Some(dir.path()), "s1");
        assert_eq!(rows, []);
        let problem = ledger.sync(&[row(1)]).expect("the read problem");
        assert!(problem.contains("saved token usage"), "{problem}");
        assert_eq!(ledger.sync(&[row(1)]), None);
        assert_eq!(ledger.path(), None);
    }

    #[test]
    fn without_a_folder_nothing_is_written() {
        let mut ledger = Ledger::new(None, "s1");
        assert_eq!(ledger.sync(&[row(1)]), None);
        assert_eq!(ledger.path(), None);
    }
}
