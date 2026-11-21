//! The usage file of one session: a JSON line per model turn, appended as turns end.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use ch_core::UsageRow;

pub struct Ledger {
    /// `None` when there is nowhere to write, or after a write failed.
    path: Option<PathBuf>,
    written: usize,
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
            pending: None,
        }
    }

    /// The ledger of a resumed session, with the rows already in its file. Lines that do not
    /// read as a row are skipped. A file that cannot be read is not written to either, so two
    /// runs never number turns over each other.
    pub fn resumed(dir: Option<&Path>, session: &str) -> (Self, Vec<UsageRow>) {
        let mut ledger = Self::new(dir, session);
        let Some(path) = ledger.path.as_deref() else {
            return (ledger, Vec::new());
        };
        let rows = match fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<UsageRow>>(),
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

    /// Appends the rows not written yet. The first failure is returned and writing stops, so a
    /// full disk is reported once, not on every tick.
    pub fn sync(&mut self, rows: &[UsageRow]) -> Option<String> {
        if let Some(problem) = self.pending.take() {
            return Some(problem);
        }
        let fresh = rows.get(self.written..).filter(|fresh| !fresh.is_empty())?;
        let path = self.path.as_deref()?;
        match append(path, fresh) {
            Ok(()) => {
                self.written = rows.len();
                None
            }
            Err(e) => {
                self.path = None;
                Some(format!(
                    "Could not save token usage, so it is no longer saved: {e}"
                ))
            }
        }
    }
}

fn append(path: &Path, rows: &[UsageRow]) -> Result<(), String> {
    let mut text = String::new();
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
    fn a_failed_write_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the file should be makes the open fail.
        fs::create_dir(dir.path().join("s1.jsonl")).unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        let problem = ledger.sync(&[row(1)]).expect("the write must fail");
        assert!(problem.contains("token usage"), "{problem}");
        assert_eq!(ledger.sync(&[row(1), row(2)]), None);
        assert_eq!(ledger.path(), None);
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
