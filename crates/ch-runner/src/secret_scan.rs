//! The secret scan a land runs before it commits: every line the change adds, checked for the
//! shape of a credential.
//!
//! Fails closed. The whole diff is read or the land is refused: a change whose diff is over
//! [`DIFF_BUDGET`] is refused as too large to scan, and a diff this parser cannot follow is an
//! error, never a pass. Files git treats as binary, by their content or by `.gitattributes`, have
//! no lines in the diff and are not scanned.

use ch_core::{redact_secrets, secret_kind};
use ch_protocol::runner::{MAX_SECRET_FINDINGS, SecretFinding, SecretScan};

use crate::git::{self, Git};

/// The most diff a scan reads. A larger change is refused, not scanned in part.
pub const DIFF_BUDGET: usize = 32 * 1024 * 1024;

/// How many findings the refusal message names; the rest are counted.
const NAMED: usize = 5;

/// What the scan says when git's output is not a diff it can follow. Names no line: a line it
/// could not place may still hold a key.
const UNREADABLE: &str = "nothing was landed: the secret scan could not read the change";

/// Header lines git writes between `diff --git` and a file's first hunk.
const HEADER_LINES: [&str; 13] = [
    "--- ",
    "index ",
    "old mode ",
    "new mode ",
    "deleted file mode ",
    "new file mode ",
    "similarity index ",
    "dissimilarity index ",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "Binary files ",
];

/// Scans the lines tree `to` adds over tree `from`. `Ok(None)` when none looks like a secret;
/// `Ok(Some(scan))` when the land must be refused, for what it found or because the change was
/// too large to read whole.
pub fn scan(git: &Git, from: &str, to: &str) -> Result<Option<SecretScan>, String> {
    scan_within(git, from, to, DIFF_BUDGET)
}

fn scan_within(
    git: &Git,
    from: &str,
    to: &str,
    budget: usize,
) -> Result<Option<SecretScan>, String> {
    // Plumbing, so the user's `diff.*` settings cannot reshape what is parsed; no external
    // diff or text conversion, so what is scanned is what is committed.
    let mut command = git.command();
    command.args([
        "-c",
        "core.quotePath=false",
        "diff-tree",
        "-r",
        "-p",
        "--no-renames",
    ]);
    command.args([
        "--unified=0",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
    ]);
    command.args(["--src-prefix=a/", "--dst-prefix=b/", from, to, "--"]);
    let refused = |findings: Vec<SecretFinding>, more: u32, too_large: bool| SecretScan {
        tree: to.to_owned(),
        findings,
        more,
        too_large,
    };
    let Some(diff) = git::run_within(command, "diff-tree", budget)? else {
        return Ok(Some(refused(Vec::new(), 0, true)));
    };
    let mut findings = added_secrets(&diff)?;
    if findings.is_empty() {
        return Ok(None);
    }
    let more = findings.len().saturating_sub(MAX_SECRET_FINDINGS);
    findings.truncate(MAX_SECRET_FINDINGS);
    let more = u32::try_from(more).unwrap_or(u32::MAX);
    Ok(Some(refused(findings, more, false)))
}

/// The refusal a person reads: where each finding is and what kind it is, never what it says.
pub fn refusal(scan: &SecretScan) -> String {
    if scan.too_large {
        return "nothing was landed: the change is too large to scan for secrets".to_owned();
    }
    let named: Vec<String> = scan
        .findings
        .iter()
        .take(NAMED)
        .map(|f| format!("{}:{} ({})", f.path, f.line, f.kind.label()))
        .collect();
    let unnamed = u64::try_from(scan.findings.len().saturating_sub(NAMED)).unwrap_or(u64::MAX);
    let rest = unnamed.saturating_add(u64::from(scan.more));
    let named = named.join(", ");
    let lead = "nothing was landed: lines this change adds look like secrets";
    if rest == 0 {
        format!("{lead}: {named}")
    } else {
        format!("{lead}: {named} and {rest} more")
    }
}

/// Every line `diff` adds that holds a secret, in order. Bounded by [`DIFF_BUDGET`].
fn added_secrets(diff: &[u8]) -> Result<Vec<SecretFinding>, String> {
    let body = diff.strip_suffix(b"\n").unwrap_or(diff);
    let mut patch = Patch::default();
    if !body.is_empty() {
        for line in body.split(|&b| b == b'\n') {
            patch
                .read(&String::from_utf8_lossy(line))
                .ok_or_else(|| UNREADABLE.to_owned())?;
        }
    }
    patch.finish().ok_or_else(|| UNREADABLE.to_owned())
}

/// Reads `git diff-tree -p` output line by line. Every line must be one git writes; any other
/// stops the scan, since a line it cannot place may hold a key.
#[derive(Default)]
struct Patch {
    /// Set by the first `diff --git` line.
    started: bool,
    /// The current file in the new tree, masked. `None` before its `+++` line, or when the
    /// file is deleted.
    path: Option<String>,
    /// Whether the current file has had a hunk, after which no header line may follow.
    in_hunks: bool,
    /// Old and new lines the current hunk still holds; a line that starts with `+++` while
    /// these are not both zero is content, not a header.
    old: u32,
    new: u32,
    /// The new file's number for the next added line.
    line: u32,
    found: Vec<SecretFinding>,
}

impl Patch {
    fn read(&mut self, line: &str) -> Option<()> {
        if self.old > 0 || self.new > 0 {
            return self.hunk_line(line);
        }
        if line.starts_with("diff --git ") {
            self.started = true;
            self.path = None;
            self.in_hunks = false;
            return Some(());
        }
        if !self.started {
            return None;
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            return self.start_hunk(header);
        }
        if self.in_hunks {
            // Only "\ No newline at end of file" may follow a finished hunk.
            return line.starts_with('\\').then_some(());
        }
        if let Some(field) = line.strip_prefix("+++ ") {
            self.path = new_path(field);
            return Some(());
        }
        HEADER_LINES
            .iter()
            .any(|header| line.starts_with(header))
            .then_some(())
    }

    /// Reads `-a[,b] +c[,d] @@[ context]`.
    fn start_hunk(&mut self, header: &str) -> Option<()> {
        let (ranges, _) = header.split_once(" @@")?;
        let (old, new) = ranges.split_once(' ')?;
        let (_, old_count) = range(old.strip_prefix('-')?)?;
        let (start, new_count) = range(new.strip_prefix('+')?)?;
        self.old = old_count;
        self.new = new_count;
        self.line = start;
        self.in_hunks = true;
        Some(())
    }

    fn hunk_line(&mut self, line: &str) -> Option<()> {
        if let Some(added) = line.strip_prefix('+') {
            self.new = self.new.checked_sub(1)?;
            let path = self.path.as_ref()?;
            if let Some(kind) = secret_kind(added) {
                self.found.push(SecretFinding {
                    path: path.clone(),
                    line: self.line,
                    kind,
                });
            }
            self.line = self.line.checked_add(1)?;
        } else if line.starts_with('-') {
            self.old = self.old.checked_sub(1)?;
        } else if line.is_empty() || line.starts_with(' ') {
            self.old = self.old.checked_sub(1)?;
            self.new = self.new.checked_sub(1)?;
            self.line = self.line.checked_add(1)?;
        } else if !line.starts_with('\\') {
            return None;
        }
        Some(())
    }

    /// The findings, or `None` when the output ended inside a hunk.
    fn finish(self) -> Option<Vec<SecretFinding>> {
        (self.old == 0 && self.new == 0).then_some(self.found)
    }
}

/// `start[,count]`, where a missing count is 1.
fn range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// The path a `+++` line names, without git's markup, masked in case it holds a key. `None`
/// for a deleted file, or a field this parser does not know, so an added line under it stops
/// the scan.
fn new_path(field: &str) -> Option<String> {
    // Git ends the field with a tab when the path holds a space; a quoted path keeps its
    // escapes, as `git status` shows it.
    let field = field.strip_suffix('\t').unwrap_or(field);
    let field = field
        .strip_prefix('"')
        .and_then(|quoted| quoted.strip_suffix('"'))
        .unwrap_or(field);
    field.strip_prefix("b/").map(redact_secrets)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use ch_protocol::runner::SecretKind;

    use super::*;
    use crate::git::Base;

    /// Built from parts so this file holds no key a scanner would flag.
    const TOKEN: &str = concat!("gh", "p_", "abcdefghijklmnopqrstuvwxyz0123456789");
    const PEM: &str = concat!("-----BEGIN RSA PRIV", "ATE KEY-----");

    fn finding(path: &str, line: u32, kind: SecretKind) -> SecretFinding {
        SecretFinding {
            path: path.to_owned(),
            line,
            kind,
        }
    }

    fn diff(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    #[test]
    fn a_key_in_an_added_line_is_found_at_its_line_in_the_new_file() {
        let patch = format!(
            "diff --git a/src/a.txt b/src/a.txt\n\
             index 1111111..2222222 100644\n\
             --- a/src/a.txt\n\
             +++ b/src/a.txt\n\
             @@ -4,0 +5,2 @@ fn main() {{\n\
             +let fine = 1;\n\
             +let key = \"{TOKEN}\";\n"
        );
        assert_eq!(
            added_secrets(&diff(&patch)),
            Ok(vec![finding("src/a.txt", 6, SecretKind::ProviderToken)])
        );
    }

    #[test]
    fn line_numbers_follow_each_hunk_and_each_file() {
        let patch = format!(
            "diff --git a/a b/a\n\
             --- a/a\n\
             +++ b/a\n\
             @@ -1 +1 @@\n\
             -old\n\
             +{TOKEN}\n\
             @@ -10,2 +10,0 @@\n\
             -gone\n\
             -gone\n\
             @@ -20,0 +19 @@\n\
             +{PEM}\n\
             \\ No newline at end of file\n\
             diff --git a/key.pem b/key.pem\n\
             new file mode 100600\n\
             index 0000000..3333333\n\
             --- /dev/null\n\
             +++ b/key.pem\n\
             @@ -0,0 +1,2 @@\n\
             +{PEM}\n\
             +MIIEpAIBAAKCAQEA\n"
        );
        assert_eq!(
            added_secrets(&diff(&patch)),
            Ok(vec![
                finding("a", 1, SecretKind::ProviderToken),
                finding("a", 19, SecretKind::PrivateKey),
                finding("key.pem", 1, SecretKind::PrivateKey),
            ])
        );
    }

    #[test]
    fn a_removed_line_holding_a_key_is_not_a_finding() {
        let patch = format!(
            "diff --git a/a b/a\n\
             deleted file mode 100644\n\
             index 2222222..0000000\n\
             --- a/a\n\
             +++ /dev/null\n\
             @@ -1,2 +0,0 @@\n\
             -{TOKEN}\n\
             -{PEM}\n"
        );
        assert_eq!(added_secrets(&diff(&patch)), Ok(Vec::new()));
    }

    #[test]
    fn an_added_line_that_looks_like_a_file_header_is_still_scanned() {
        let patch = format!(
            "diff --git a/a b/a\n\
             --- a/a\n\
             +++ b/a\n\
             @@ -0,0 +1,2 @@\n\
             +++ {TOKEN}\n\
             +--- {TOKEN}\n"
        );
        assert_eq!(
            added_secrets(&diff(&patch)),
            Ok(vec![
                finding("a", 1, SecretKind::ProviderToken),
                finding("a", 2, SecretKind::ProviderToken),
            ])
        );
    }

    #[test]
    fn a_path_with_a_space_or_quotes_is_named_without_the_diff_markup() {
        let patch = format!(
            "diff --git a/my notes.txt b/my notes.txt\n\
             --- a/my notes.txt\t\n\
             +++ b/my notes.txt\t\n\
             @@ -0,0 +1 @@\n\
             +{TOKEN}\n\
             diff --git \"a/tab\\there\" \"b/tab\\there\"\n\
             --- \"a/tab\\there\"\n\
             +++ \"b/tab\\there\"\n\
             @@ -0,0 +1 @@\n\
             +{TOKEN}\n"
        );
        assert_eq!(
            added_secrets(&diff(&patch)),
            Ok(vec![
                finding("my notes.txt", 1, SecretKind::ProviderToken),
                finding("tab\\there", 1, SecretKind::ProviderToken),
            ])
        );
    }

    #[test]
    fn a_path_that_looks_like_a_key_is_named_masked() {
        let patch = format!(
            "diff --git a/{TOKEN}.txt b/{TOKEN}.txt\n\
             --- /dev/null\n\
             +++ b/{TOKEN}.txt\n\
             @@ -0,0 +1 @@\n\
             +{PEM}\n"
        );
        let found = added_secrets(&diff(&patch)).unwrap();
        assert_eq!(found.len(), 1);
        assert!(!found[0].path.contains(TOKEN), "{}", found[0].path);
    }

    #[test]
    fn a_binary_file_has_no_lines_to_scan() {
        let patch = "diff --git a/k.bin b/k.bin\n\
                     new file mode 100644\n\
                     index 0000000..4444444\n\
                     Binary files /dev/null and b/k.bin differ\n";
        assert_eq!(added_secrets(&diff(patch)), Ok(Vec::new()));
    }

    #[test]
    fn output_that_is_not_a_diff_is_an_error_that_shows_none_of_it() {
        for patch in [
            format!("{TOKEN}\n"),
            format!("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -0,0 +1 @@\n+x\n{TOKEN}\n"),
            format!("diff --git a/a b/a\n+++ b/a\n@@ nonsense @@\n+{TOKEN}\n"),
            format!("diff --git a/a b/a\n+++ b/a\n+{TOKEN}\n"),
        ] {
            let error = added_secrets(&diff(&patch)).expect_err(&patch);
            assert_eq!(error, UNREADABLE);
        }
    }

    #[test]
    fn a_hunk_cut_short_is_an_error() {
        let patch = format!(
            "diff --git a/a b/a\n\
             --- a/a\n\
             +++ b/a\n\
             @@ -0,0 +1,3 @@\n\
             +{TOKEN}\n"
        );
        assert_eq!(added_secrets(&diff(&patch)), Err(UNREADABLE.to_owned()));
    }

    #[test]
    fn the_refusal_names_where_and_what_and_counts_the_rest() {
        let scan = SecretScan {
            tree: "abc".into(),
            findings: (1..=7)
                .map(|line| finding("src/a.rs", line, SecretKind::ProviderToken))
                .chain([finding("id_rsa", 1, SecretKind::PrivateKey)])
                .collect(),
            more: 3,
            too_large: false,
        };
        let text = refusal(&scan);
        assert!(text.starts_with("nothing was landed: "), "{text}");
        assert!(
            text.contains("src/a.rs:1 (provider token), src/a.rs:2 (provider token)"),
            "{text}"
        );
        assert!(text.contains("src/a.rs:5 (provider token)"), "{text}");
        assert!(!text.contains("src/a.rs:6"), "{text}");
        assert!(text.contains("and 6 more"), "{text}");
    }

    #[test]
    fn a_refusal_for_size_says_so() {
        let scan = SecretScan {
            tree: "abc".into(),
            findings: Vec::new(),
            more: 0,
            too_large: true,
        };
        assert_eq!(
            refusal(&scan),
            "nothing was landed: the change is too large to scan for secrets"
        );
    }

    struct Fixture {
        _root: tempfile::TempDir,
        repo: PathBuf,
        index: PathBuf,
        git: Git,
        base: Base,
    }

    impl Fixture {
        /// A repository whose one commit holds `a.txt` with a key on its second line.
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            let hooks = root.path().join("no-hooks");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&hooks).unwrap();
            setup(&repo, &["init", "-q"]);
            setup(&repo, &["config", "core.autocrlf", "false"]);
            fs::write(repo.join("a.txt"), format!("first\n{TOKEN}\n")).unwrap();
            setup(&repo, &["add", "a.txt"]);
            setup(&repo, &["commit", "-q", "-m", "first"]);
            let mut git = Git::new(&repo, hooks);
            git.pin().unwrap();
            let base = git.base().unwrap();
            Self {
                index: root.path().join("index"),
                _root: root,
                repo,
                git,
                base,
            }
        }

        fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
            let path = self.repo.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        /// The base tree and the working tree's.
        fn trees(&self) -> (String, String) {
            let from = self.git.base_tree(&self.base).unwrap();
            let to = self.git.worktree_tree(&self.base, &self.index).unwrap();
            (from, to)
        }

        fn scan(&self) -> Result<Option<SecretScan>, String> {
            let (from, to) = self.trees();
            scan(&self.git, &from, &to)
        }
    }

    fn setup(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Setup",
                "-c",
                "user.email=setup@example.invalid",
            ])
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_key_added_in_a_nested_file_is_found_and_the_scan_names_its_tree() {
        let fixture = Fixture::new();
        fixture.write(
            "src/deep/config.ts",
            format!("a\nb\nconst k = '{TOKEN}';\n"),
        );
        let (_, to) = fixture.trees();
        let scan = fixture.scan().unwrap().expect("the key is found");
        assert_eq!(scan.tree, to);
        assert_eq!(
            scan.findings,
            vec![finding("src/deep/config.ts", 3, SecretKind::ProviderToken)]
        );
        assert!(!scan.too_large);
    }

    #[test]
    fn a_pem_header_added_is_found() {
        let fixture = Fixture::new();
        fixture.write("id_rsa", format!("{PEM}\nMIIEpAIBAAKCAQEA\n"));
        let scan = fixture.scan().unwrap().expect("the key is found");
        assert_eq!(
            scan.findings,
            vec![finding("id_rsa", 1, SecretKind::PrivateKey)]
        );
    }

    #[test]
    fn a_clean_change_and_a_removed_key_pass() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "first\n");
        fixture.write("b.txt", "password: string;\n");
        assert_eq!(fixture.scan(), Ok(None));
    }

    #[test]
    fn a_binary_file_is_not_scanned() {
        let fixture = Fixture::new();
        fixture.write("k.bin", [b"\0\0".as_slice(), TOKEN.as_bytes()].concat());
        assert_eq!(fixture.scan(), Ok(None));
    }

    #[test]
    fn findings_past_the_limit_are_counted_not_listed() {
        let fixture = Fixture::new();
        let lines = format!("{TOKEN}\n").repeat(MAX_SECRET_FINDINGS + 4);
        fixture.write("many.txt", lines);
        let scan = fixture.scan().unwrap().expect("the keys are found");
        assert_eq!(scan.findings.len(), MAX_SECRET_FINDINGS);
        assert_eq!(scan.more, 4);
    }

    #[test]
    fn a_change_over_the_budget_is_refused_as_too_large_not_scanned_in_part() {
        let fixture = Fixture::new();
        fixture.write("big.txt", "x\n".repeat(64));
        let (from, to) = fixture.trees();
        let scan = scan_within(&fixture.git, &from, &to, 32)
            .unwrap()
            .expect("too large is a refusal");
        assert!(scan.too_large);
        assert_eq!(scan.findings, []);
        assert_eq!(scan.tree, to);
    }

    #[test]
    fn a_tree_git_cannot_read_is_an_error() {
        let fixture = Fixture::new();
        let (from, _) = fixture.trees();
        let missing = "0123456789abcdef0123456789abcdef01234567";
        assert!(scan(&fixture.git, &from, missing).is_err());
    }
}
