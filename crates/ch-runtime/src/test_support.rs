//! Helpers for tests that need a real git repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::LocalProvider;
use crate::git::clear_git_location;

pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let mut git = Command::new("git");
    clear_git_location(&mut git);
    let output = git
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub(crate) fn repo_with_commit(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("README"), "hello\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "--no-verify", "-m", "init"]);
    repo
}

pub(crate) fn worktree_count(repo: &Path) -> usize {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

pub(crate) fn provider(root: &Path, repo: PathBuf) -> LocalProvider {
    LocalProvider {
        repo,
        runner: root.join("no-such-ch-runner"),
        worktrees_dir: root.join("worktrees"),
        sessions_dir: root.join("sessions"),
    }
}
