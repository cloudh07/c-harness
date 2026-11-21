//! Git plumbing against the working tree, through a private index so the agent's own index,
//! `HEAD` and current branch are never touched.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;

use crate::proc_tree;
use crate::tail::{TailBuffer, cut_head, cut_tail};

/// How much of a failing git command's stderr goes into the error message.
const STDERR_TAIL_BYTES: usize = 2048;

/// Inherited variables that would point git at another repository or index.
const REPOSITORY_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
];

/// The commit (or, in a repository with no commits yet, the empty tree) a session started from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    pub id: String,
    pub is_commit: bool,
}

#[derive(Clone, Debug)]
pub struct Git {
    workdir: PathBuf,
    no_hooks: PathBuf,
    pinned: Option<Pinned>,
}

/// Where the repository was found before the agent ran.
#[derive(Clone, Debug)]
struct Pinned {
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl Git {
    /// `no_hooks` must be an empty directory outside the working tree.
    pub fn new(workdir: &Path, no_hooks: PathBuf) -> Self {
        Self {
            workdir: workdir.to_owned(),
            no_hooks,
            pinned: None,
        }
    }

    /// Finds the repository once, before the agent runs. Every later command names that git
    /// directory and working tree outright, so a rewritten `.git` file or `core.worktree`
    /// cannot point the land at another repository or directory.
    pub fn pin(&mut self) -> Result<(), String> {
        if self.pinned.is_some() {
            return Ok(());
        }
        let mut command = self.command();
        command.args([
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ]);
        let text = run_text(command, "rev-parse")
            .map_err(|e| format!("{} is not a git working tree: {e}", self.workdir.display()))?;
        let mut lines = text.lines().map(str::trim);
        let (Some(top), Some(git_dir), Some(common_dir), None) =
            (lines.next(), lines.next(), lines.next(), lines.next())
        else {
            return Err(format!("git rev-parse gave an unexpected answer: {text}"));
        };
        if !same_dir(Path::new(top), &self.workdir) {
            return Err(format!(
                "{} is inside the repository at {top}; give the top of a working tree",
                self.workdir.display()
            ));
        }
        self.pinned = Some(Pinned {
            git_dir: PathBuf::from(git_dir),
            // Older git prints this relative to the directory it ran in.
            common_dir: self.workdir.join(common_dir),
        });
        Ok(())
    }

    /// The repository a push fetches the landed branch from.
    pub fn fetch_source(&self) -> &Path {
        self.pinned
            .as_ref()
            .map_or(self.workdir.as_path(), |pinned| pinned.common_dir.as_path())
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// A git command in the working tree that runs none of the repository's hooks or
    /// file-system monitor, both of which the agent could have written.
    pub fn command(&self) -> Command {
        let mut command = proc_tree::command("git");
        if let Some(pinned) = &self.pinned {
            command
                .arg(prefixed("--git-dir=", &pinned.git_dir))
                .arg(prefixed("--work-tree=", &self.workdir));
        }
        command
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg(format!("core.hooksPath={}", self.no_hooks.display()))
            .current_dir(&self.workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        for name in REPOSITORY_ENV {
            command.env_remove(name);
        }
        command
    }

    pub(crate) fn with_index(&self, index: &Path) -> Command {
        let mut command = self.command();
        command.env("GIT_INDEX_FILE", index);
        command
    }

    /// `HEAD` as a commit id, or the empty tree when the repository has no commits yet.
    pub fn base(&self) -> Result<Base, String> {
        let mut check = self.command();
        check.args(["rev-parse", "--git-dir"]);
        run(check, "rev-parse --git-dir")
            .map_err(|e| format!("{} is not a git repository: {e}", self.workdir.display()))?;
        if let Some(id) = self.head() {
            return Ok(Base {
                id,
                is_commit: true,
            });
        }
        // Asking git keeps this right in a SHA-256 repository.
        let mut empty = self.command();
        empty.args(["hash-object", "-t", "tree", "--stdin"]);
        let id = run_text(empty, "hash-object")?;
        Ok(Base {
            id,
            is_commit: false,
        })
    }

    /// The commit `HEAD` points at, if there is one.
    pub fn head(&self) -> Option<String> {
        let mut command = self.command();
        command.args(["rev-parse", "--verify", "-q", "HEAD^{commit}"]);
        run_text(command, "rev-parse HEAD").ok()
    }

    /// The tree a base stands for.
    pub fn base_tree(&self, base: &Base) -> Result<String, String> {
        if !base.is_commit {
            return Ok(base.id.clone());
        }
        let mut command = self.command();
        command
            .args(["rev-parse", "--verify", "-q"])
            .arg(format!("{}^{{tree}}", base.id));
        run_text(command, "rev-parse tree")
    }

    /// Fills `index` with the base and then everything in the working tree that is not ignored.
    pub fn stage(&self, base: &Base, index: &Path) -> Result<(), String> {
        let mut read = self.with_index(index);
        read.args(["read-tree", &base.id]);
        run(read, "read-tree")?;
        let mut add = self.with_index(index);
        add.args(["add", "-A"]);
        run(add, "add -A")?;
        Ok(())
    }

    /// The tree of a staged `index`.
    pub fn write_tree(&self, index: &Path) -> Result<String, String> {
        let mut command = self.with_index(index);
        command.arg("write-tree");
        run_text(command, "write-tree")
    }

    /// The tree the working tree would commit as on top of `base`. `index` is scratch and is
    /// removed afterwards.
    pub fn worktree_tree(&self, base: &Base, index: &Path) -> Result<String, String> {
        let tree = self
            .stage(base, index)
            .and_then(|()| self.write_tree(index));
        // A scratch index left behind only wastes the temp dir.
        let _ = fs::remove_file(index);
        tree
    }

    /// The working tree's changes against `base`, each part cut to its budget.
    pub fn diff(
        &self,
        base: &Base,
        index: &Path,
        stat_budget: usize,
        patch_budget: usize,
    ) -> Result<Diff, String> {
        self.diff_since(base, &base.id, index, (stat_budget, patch_budget))
    }

    /// The working tree's changes against `since`, a commit or tree id. `index` is scratch,
    /// filled from `base`, and removed afterwards.
    pub fn diff_since(
        &self,
        base: &Base,
        since: &str,
        index: &Path,
        budgets: (usize, usize),
    ) -> Result<Diff, String> {
        let result = self.stage(base, index).and_then(|()| {
            capped_diff(budgets, |stat| {
                let mut command = self.with_index(index);
                diff_args(&mut command, stat);
                command.arg("--cached").arg(since).arg("--");
                command
            })
        });
        // The index is scratch; leaving it would only waste the temp dir until the session ends.
        let _ = fs::remove_file(index);
        result
    }

    /// The changes from one commit or tree to another.
    pub fn diff_between(
        &self,
        from: &str,
        to: &str,
        budgets: (usize, usize),
    ) -> Result<Diff, String> {
        capped_diff(budgets, |stat| {
            let mut command = self.command();
            diff_args(&mut command, stat);
            command.arg(from).arg(to).arg("--");
            command
        })
    }
}

/// Runs the stat and the patch form of one diff, each cut to its budget.
fn capped_diff(
    (stat_budget, patch_budget): (usize, usize),
    diff_command: impl Fn(bool) -> Command,
) -> Result<Diff, String> {
    let (stat, stat_cut) = run_capped(diff_command(true), "diff --stat", stat_budget)?;
    let (patch, patch_cut) = run_capped(diff_command(false), "diff", patch_budget)?;
    Ok(Diff {
        stat,
        patch,
        truncated: stat_cut || patch_cut,
    })
}

/// Plain diff output: no colour, external diff tools or text conversion the repository set up.
fn diff_args(command: &mut Command, stat: bool) {
    command.args(["diff", "--no-color", "--no-ext-diff", "--no-textconv"]);
    if stat {
        command.arg("--stat");
    }
}

fn prefixed(option: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(option);
    arg.push(path);
    arg
}

pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[derive(Debug)]
pub struct Diff {
    pub stat: String,
    pub patch: String,
    pub truncated: bool,
}

/// Runs a git command to completion and returns its stdout.
pub fn run(mut command: Command, what: &str) -> Result<Vec<u8>, String> {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run git {what}: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failure(what, &output.stderr))
    }
}

/// Like [`run`], with stdout as one trimmed line of text.
pub fn run_text(command: Command, what: &str) -> Result<String, String> {
    let stdout = run(command, what)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
}

/// Runs a git command with `input` on its stdin.
pub fn run_with_input(mut command: Command, what: &str, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git {what}: {e}"))?;
    let written = child.stdin.take().map(|mut stdin| stdin.write_all(input));
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git {what} did not finish: {e}"))?;
    if !output.status.success() {
        return Err(failure(what, &output.stderr));
    }
    match written {
        Some(Err(e)) => Err(format!("could not write to git {what}: {e}")),
        _ => Ok(output.stdout),
    }
}

fn failure(what: &str, stderr: &[u8]) -> String {
    let (tail, _) = cut_tail(stderr, STDERR_TAIL_BYTES);
    let tail = tail.trim();
    if tail.is_empty() {
        format!("git {what} failed")
    } else {
        format!("git {what} failed: {tail}")
    }
}

/// Runs a git command whose output may be far larger than what the client should get, reading
/// no more than `budget` bytes of it.
pub(crate) fn run_capped(
    command: Command,
    what: &str,
    budget: usize,
) -> Result<(String, bool), String> {
    read_capped(command, what, budget).map(|stdout| cut_head(&stdout, budget))
}

/// Like [`run_capped`], as bytes, or `None` when the output is over `budget`.
pub(crate) fn run_within(
    command: Command,
    what: &str,
    budget: usize,
) -> Result<Option<Vec<u8>>, String> {
    let stdout = read_capped(command, what, budget)?;
    Ok((stdout.len() <= budget).then_some(stdout))
}

/// Up to `budget` + 1 bytes of stdout; git is stopped once it prints more than `budget`.
fn read_capped(mut command: Command, what: &str, budget: usize) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git {what}: {e}"))?;
    let stderr = child.stderr.take().map(|stderr| {
        thread::spawn(move || {
            let mut tail = TailBuffer::new(STDERR_TAIL_BYTES);
            drain(stderr, &mut tail);
            tail.text().0
        })
    });
    let mut stdout = Vec::new();
    let limit = u64::try_from(budget).unwrap_or(u64::MAX).saturating_add(1);
    let read = child
        .stdout
        .take()
        .map(|out| out.take(limit).read_to_end(&mut stdout));
    let over = stdout.len() > budget;
    if over {
        // The rest of the output is not wanted; stopping git is cheaper than reading it.
        let _ = child.kill();
    }
    let status = finish(&mut child, what)?;
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    if let Some(Err(e)) = read {
        return Err(format!("could not read git {what}: {e}"));
    }
    if !over && !status.success() {
        return Err(failure(what, stderr.as_bytes()));
    }
    Ok(stdout)
}

fn finish(child: &mut Child, what: &str) -> Result<std::process::ExitStatus, String> {
    child
        .wait()
        .map_err(|e| format!("git {what} did not finish: {e}"))
}

fn drain(mut reader: impl Read, tail: &mut TailBuffer) {
    let mut buf = [0_u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => tail.push(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}
