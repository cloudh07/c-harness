//! Checkpoints: snapshots of the working tree kept as `refs/c-harness/<session>/turn-<n>`.
//!
//! Only a working tree C-Harness made for the session keeps them: a linked worktree locked with
//! the reason `c-harness session <session>`. A restore rewrites the working tree, so it never
//! runs on a checkout someone else owns. The agent can rewrite these refs too, so they are a
//! convenience, never evidence.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use ch_protocol::runner::{Checkpoint, CheckpointKind, MAX_CHECKPOINTS};

use crate::git::{self, Base, Git};

mod overwrite;

const LOCK_PREFIX: &str = "c-harness session ";
const RUN_SUBJECT: &str = "C-Harness checkpoint";
const BEFORE_RESTORE_SUBJECT: &str = "C-Harness checkpoint before restore";
const IDENTITY_NAME: &str = "C-Harness";
const IDENTITY_EMAIL: &str = "checkpoint@c-harness.invalid";
/// The most bytes of a ref or worktree listing read. 1000 checkpoints fit many times over.
const LISTING_BUDGET: usize = 512 * 1024;
const MAX_SESSION_LEN: usize = 64;

/// Saves and restores one session's checkpoints. Turns count up from what the repository had.
#[derive(Debug)]
pub struct Store {
    session: String,
    next: u32,
}

impl Store {
    /// The store, with the checkpoints already saved, when this working tree keeps them.
    pub fn open(git: &Git, session: &str) -> Result<Option<(Self, Vec<Checkpoint>)>, String> {
        if !valid_session(session) || !locked_for(git, session)? {
            return Ok(None);
        }
        let (checkpoints, next) = list(git, session)?;
        let store = Self {
            session: session.to_owned(),
            next,
        };
        Ok(Some((store, checkpoints)))
    }

    /// Saves the working tree as the next checkpoint. `index` is scratch and removed after.
    pub fn save(&mut self, git: &Git, base: &Base, index: &Path) -> Result<Checkpoint, String> {
        let tree = git.worktree_tree(base, index)?;
        self.save_tree(git, base, &tree, CheckpointKind::Run)
    }

    /// The commit or tree id `turn` names: 0 is the base, any other number a checkpoint.
    /// Only a commit C-Harness wrote counts, as in the listing.
    pub fn resolve(&self, git: &Git, base: &Base, turn: u32) -> Result<String, String> {
        if turn == 0 {
            return Ok(base.id.clone());
        }
        let refs = turn_refs(git, &self.session, &self.ref_name(turn))?;
        refs.into_iter()
            .find_map(|(found, checkpoint)| checkpoint.filter(|_| found == turn))
            .map(|checkpoint| checkpoint.commit)
            .ok_or_else(|| format!("there is no checkpoint {turn}"))
    }

    /// Saves the working tree as a checkpoint, calls `saved` with it, then puts back `turn`.
    /// `index` is scratch and removed after. A restore that would write over a file the
    /// checkpoint leaves out, such as an ignored one, is refused before anything is saved.
    /// Files are written with the repository's line-ending settings, as `git checkout` does.
    pub fn restore(
        &mut self,
        git: &Git,
        base: &Base,
        index: &Path,
        turn: u32,
        saved: impl FnOnce(&Checkpoint),
    ) -> Result<(), String> {
        let target = self.resolve(git, base, turn)?;
        let result = self.restore_to(git, base, index, &target, saved);
        let _ = fs::remove_file(index);
        result
    }

    fn restore_to(
        &mut self,
        git: &Git,
        base: &Base,
        index: &Path,
        target: &str,
        saved: impl FnOnce(&Checkpoint),
    ) -> Result<(), String> {
        git.stage(base, index)?;
        let current = git.write_tree(index)?;
        // read-tree -m -u writes over ignored files, so look first.
        overwrite::check(git, &current, target)?;
        let checkpoint = self.save_tree(git, base, &current, CheckpointKind::BeforeRestore)?;
        saved(&checkpoint);
        let mut read = git.with_index(index);
        read.args(["read-tree", "-m", "-u", &current, target]);
        git::run(read, "read-tree").map(drop)
    }

    fn save_tree(
        &mut self,
        git: &Git,
        base: &Base,
        tree: &str,
        kind: CheckpointKind,
    ) -> Result<Checkpoint, String> {
        let turn = self.next;
        if !in_range(turn) {
            return Err(format!(
                "this session has {MAX_CHECKPOINTS} checkpoints, the most it keeps"
            ));
        }
        let subject = match kind {
            CheckpointKind::Run => RUN_SUBJECT,
            CheckpointKind::BeforeRestore => BEFORE_RESTORE_SUBJECT,
        };
        let message = format!("{subject}\n\nSession: {}\nTurn: {turn}\n", self.session);
        let mut commit_tree = git.command();
        commit_tree.args(["commit-tree", "--no-gpg-sign"]);
        if base.is_commit {
            commit_tree.args(["-p", &base.id]);
        }
        commit_tree.arg(tree);
        set_identity(&mut commit_tree);
        let commit = git::run_with_input(commit_tree, "commit-tree", message.as_bytes())?;
        let commit = String::from_utf8_lossy(&commit).trim().to_owned();

        // A failed save still uses up the number, so the next one does not collide with it.
        self.next = turn + 1;
        let mut update = git.command();
        update
            .args(["update-ref", "-m", "c-harness checkpoint"])
            .arg(self.ref_name(turn))
            .arg(&commit)
            // An empty old value: never overwrite a ref that is already there.
            .arg("");
        git::run(update, "update-ref")?;
        Ok(Checkpoint {
            turn,
            commit,
            kind,
            at: now_secs(),
        })
    }

    fn ref_name(&self, turn: u32) -> String {
        format!("{}turn-{turn}", prefix(&self.session))
    }
}

fn prefix(session: &str) -> String {
    format!("refs/c-harness/{session}/")
}

/// A session id that is safe in a ref name and a ref pattern. ch-runtime allows the same.
fn valid_session(session: &str) -> bool {
    (1..=MAX_SESSION_LEN).contains(&session.len())
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn in_range(turn: u32) -> bool {
    turn >= 1 && usize::try_from(turn).is_ok_and(|turn| turn <= MAX_CHECKPOINTS)
}

fn is_object_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn set_identity(command: &mut Command) {
    command
        .env("GIT_AUTHOR_NAME", IDENTITY_NAME)
        .env("GIT_AUTHOR_EMAIL", IDENTITY_EMAIL)
        .env("GIT_COMMITTER_NAME", IDENTITY_NAME)
        .env("GIT_COMMITTER_EMAIL", IDENTITY_EMAIL);
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Output cut at its budget, without the last line, which may be partial.
fn complete_lines((text, cut): (String, bool)) -> String {
    if !cut {
        return text;
    }
    text.rsplit_once('\n')
        .map(|(head, _)| head.to_owned())
        .unwrap_or_default()
}

/// Whether git lists the working tree as a linked worktree locked for `session`.
fn locked_for(git: &Git, session: &str) -> Result<bool, String> {
    let mut command = git.command();
    command.args(["worktree", "list", "--porcelain"]);
    let text = complete_lines(git::run_capped(command, "worktree list", LISTING_BUDGET)?);
    let reason = format!("{LOCK_PREFIX}{session}");
    Ok(text.split("\n\n").any(|record| {
        let mut path = None;
        let mut locked = None;
        for line in record.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                path = Some(rest);
            } else if let Some(rest) = line.strip_prefix("locked ") {
                locked = Some(rest);
            }
        }
        locked == Some(reason.as_str())
            && path.is_some_and(|path| git::same_dir(Path::new(path), git.workdir()))
    }))
}

/// The session's checkpoints, oldest first, and the turn the next one gets. Every
/// `turn-<n>` counts toward the next turn; only commits C-Harness wrote are listed.
fn list(git: &Git, session: &str) -> Result<(Vec<Checkpoint>, u32), String> {
    let refs = turn_refs(git, session, &prefix(session))?;
    let last = refs.iter().map(|(turn, _)| *turn).max().unwrap_or(0);
    let mut checkpoints: Vec<Checkpoint> = refs.into_iter().filter_map(|(_, c)| c).collect();
    checkpoints.sort_by_key(|c| c.turn);
    Ok((checkpoints, last + 1))
}

/// The session's `turn-<n>` refs that `pattern` matches, each with its checkpoint when it is a
/// commit C-Harness wrote.
fn turn_refs(
    git: &Git,
    session: &str,
    pattern: &str,
) -> Result<Vec<(u32, Option<Checkpoint>)>, String> {
    let prefix = prefix(session);
    let mut command = git.command();
    command
        .arg("for-each-ref")
        .arg(
            "--format=%(refname)%00%(objectname)%00%(objecttype)%00\
             %(creatordate:unix)%00%(contents:subject)",
        )
        .arg(pattern);
    let text = complete_lines(git::run_capped(command, "for-each-ref", LISTING_BUDGET)?);
    Ok(text
        .lines()
        .filter_map(|line| turn_ref(line, &prefix))
        .collect())
}

/// One `for-each-ref` line: the turn, and the checkpoint unless the ref points elsewhere.
fn turn_ref(line: &str, prefix: &str) -> Option<(u32, Option<Checkpoint>)> {
    let fields: Vec<&str> = line.split('\0').collect();
    let &[name, commit, object_type, at, subject] = fields.as_slice() else {
        return None;
    };
    let turn = name.strip_prefix(prefix).and_then(turn_of)?;
    let kind = match subject {
        RUN_SUBJECT => Some(CheckpointKind::Run),
        BEFORE_RESTORE_SUBJECT => Some(CheckpointKind::BeforeRestore),
        _ => None,
    };
    let checkpoint = kind
        .filter(|_| object_type == "commit" && is_object_id(commit))
        .map(|kind| Checkpoint {
            turn,
            commit: commit.to_owned(),
            kind,
            at: at.parse().unwrap_or(0),
        });
    Some((turn, checkpoint))
}

/// The number in `turn-<n>`, written without leading zeros and within the cap.
fn turn_of(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("turn-")?;
    let turn: u32 = digits.parse().ok()?;
    (turn.to_string() == digits && in_range(turn)).then_some(turn)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Stdio;

    use super::*;

    const LOCKED: Option<&str> = Some("c-harness session s1");

    struct Fixture {
        root: tempfile::TempDir,
        repo: PathBuf,
        tree: PathBuf,
        git: Git,
        base: Base,
    }

    impl Fixture {
        /// A repository with one commit and a linked worktree, locked with `reason` if any.
        fn new(reason: Option<&str>) -> Self {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            let tree = root.path().join("wt");
            let hooks = root.path().join("no-hooks");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&hooks).unwrap();
            setup(&repo, &["init", "-q"]);
            // Windows git converts LF to CRLF on checkout by default.
            setup(&repo, &["config", "core.autocrlf", "false"]);
            fs::write(repo.join("a.txt"), "first\n").unwrap();
            setup(&repo, &["add", "a.txt"]);
            setup(&repo, &["commit", "-q", "-m", "first"]);
            let tree_arg = tree.to_str().unwrap();
            let mut add = vec!["worktree", "add", "-q", "--detach"];
            if let Some(reason) = reason {
                add.extend(["--lock", "--reason", reason]);
            }
            add.push(tree_arg);
            setup(&repo, &add);
            let mut git = Git::new(&tree, hooks);
            git.pin().unwrap();
            let base = git.base().unwrap();
            Self {
                root,
                repo,
                tree,
                git,
                base,
            }
        }

        fn index(&self) -> PathBuf {
            self.root.path().join("index")
        }

        fn open(&self) -> (Store, Vec<Checkpoint>) {
            Store::open(&self.git, "s1")
                .unwrap()
                .expect("checkpoints are on")
        }

        fn listed(&self) -> Vec<(u32, CheckpointKind)> {
            self.open().1.iter().map(|c| (c.turn, c.kind)).collect()
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.tree.join(name), text).unwrap();
        }

        fn read(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.tree.join(name)).ok()
        }
    }

    fn setup(dir: &Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn only_a_worktree_locked_for_the_session_keeps_checkpoints() {
        let locked = Fixture::new(LOCKED);
        assert!(Store::open(&locked.git, "s1").unwrap().is_some());
        assert!(Store::open(&locked.git, "s2").unwrap().is_none());
        assert!(Store::open(&locked.git, "s1/../x").unwrap().is_none());

        let mut main = Git::new(&locked.repo, locked.root.path().join("no-hooks"));
        main.pin().unwrap();
        assert!(Store::open(&main, "s1").unwrap().is_none());

        let other = Fixture::new(Some("someone else"));
        assert!(Store::open(&other.git, "s1").unwrap().is_none());
        let unlocked = Fixture::new(None);
        assert!(Store::open(&unlocked.git, "s1").unwrap().is_none());
    }

    #[test]
    fn saves_and_lists_checkpoints_in_turn_order() {
        let f = Fixture::new(LOCKED);
        let (mut store, listed) = f.open();
        assert_eq!(listed, []);
        f.write("new.txt", "one\n");
        let first = store.save(&f.git, &f.base, &f.index()).unwrap();
        assert_eq!((first.turn, first.kind), (1, CheckpointKind::Run));
        assert!(!f.index().exists(), "the scratch index was left behind");
        f.write("new.txt", "two\n");
        let second = store.save(&f.git, &f.base, &f.index()).unwrap();
        assert_eq!(second.turn, 2);

        let (mut reopened, listed) = f.open();
        let turns: Vec<_> = listed.iter().map(|c| (c.turn, c.commit.as_str())).collect();
        assert_eq!(
            turns,
            [(1, first.commit.as_str()), (2, second.commit.as_str())]
        );
        let third = reopened.save(&f.git, &f.base, &f.index()).unwrap();
        assert_eq!(third.turn, 3);
        let parent = setup(&f.repo, &["rev-parse", &format!("{}^", third.commit)]);
        assert_eq!(parent, f.base.id);
    }

    #[test]
    fn ignores_refs_it_did_not_write_but_never_reuses_their_turn() {
        let f = Fixture::new(LOCKED);
        let head = f.base.id.as_str();
        for name in [
            "turn-4294967295",
            "turn-007",
            "turn-x",
            "sub/turn-2",
            "turn-0",
        ] {
            setup(
                &f.repo,
                &["update-ref", &format!("refs/c-harness/s1/{name}"), head],
            );
        }
        let blob = setup(&f.repo, &["hash-object", "-w", "a.txt"]);
        setup(&f.repo, &["update-ref", "refs/c-harness/s1/turn-3", &blob]);
        setup(&f.repo, &["update-ref", "refs/c-harness/s1/turn-5", head]);
        setup(&f.repo, &["update-ref", "refs/c-harness/s10/turn-9", head]);

        let (mut store, listed) = f.open();
        assert_eq!(listed, [], "a ref C-Harness did not write was listed");
        assert_eq!(store.save(&f.git, &f.base, &f.index()).unwrap().turn, 6);
        assert!(
            store.resolve(&f.git, &f.base, 3).is_err(),
            "a blob resolved"
        );
    }

    #[test]
    fn resolves_only_a_commit_it_wrote() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        let saved = store.save(&f.git, &f.base, &f.index()).unwrap();
        // A commit with another subject, and a tag that wraps a real checkpoint.
        setup(
            &f.repo,
            &["update-ref", "refs/c-harness/s1/turn-5", &f.base.id],
        );
        setup(
            &f.repo,
            &[
                "-c",
                "tag.gpgSign=false",
                "tag",
                "-a",
                "-m",
                RUN_SUBJECT,
                "wrapped",
                &saved.commit,
            ],
        );
        let tag = setup(&f.repo, &["rev-parse", "refs/tags/wrapped"]);
        setup(&f.repo, &["update-ref", "refs/c-harness/s1/turn-6", &tag]);

        assert_eq!(store.resolve(&f.git, &f.base, 1).unwrap(), saved.commit);
        for turn in [5, 6, 7] {
            let error = store.resolve(&f.git, &f.base, turn).unwrap_err();
            assert_eq!(error, format!("there is no checkpoint {turn}"));
        }
    }

    #[test]
    fn a_restore_too_large_to_check_is_refused() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        f.write("new.txt", "one\n");
        let saved = store.save(&f.git, &f.base, &f.index()).unwrap();

        let error = overwrite::check_within(&f.git, &saved.commit, &f.base.id, 4).unwrap_err();
        assert!(error.contains("too many"), "{error}");
        overwrite::check_within(&f.git, &saved.commit, &f.base.id, 4096).unwrap();
    }

    #[test]
    fn diffs_between_checkpoints_and_against_the_working_tree() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        f.write("new.txt", "one\n");
        store.save(&f.git, &f.base, &f.index()).unwrap();
        f.write("a.txt", "changed\n");
        store.save(&f.git, &f.base, &f.index()).unwrap();

        let one = store.resolve(&f.git, &f.base, 1).unwrap();
        let two = store.resolve(&f.git, &f.base, 2).unwrap();
        let budgets = (4096, 4096);
        let step = f.git.diff_between(&one, &two, budgets).unwrap();
        assert!(step.patch.contains("+changed"), "{}", step.patch);
        assert!(!step.patch.contains("new.txt"), "{}", step.patch);

        f.write("later.txt", "x\n");
        let since = f
            .git
            .diff_since(&f.base, &two, &f.index(), budgets)
            .unwrap();
        assert!(since.stat.contains("later.txt"), "{}", since.stat);
        assert!(!since.stat.contains("a.txt"), "{}", since.stat);
        assert!(!f.index().exists(), "the scratch index was left behind");
        assert_eq!(store.resolve(&f.git, &f.base, 0).unwrap(), f.base.id);
    }

    #[test]
    fn a_restore_puts_back_the_files_and_can_be_undone() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        f.write("a.txt", "second\n");
        f.write("new.txt", "new\n");
        store.save(&f.git, &f.base, &f.index()).unwrap();
        f.write("a.txt", "third\n");

        let mut saved = None;
        store
            .restore(&f.git, &f.base, &f.index(), 0, |c| saved = Some(c.clone()))
            .unwrap();
        let saved = saved.expect("no checkpoint before the restore");
        assert_eq!((saved.turn, saved.kind), (2, CheckpointKind::BeforeRestore));
        assert_eq!(f.read("a.txt").as_deref(), Some("first\n"));
        assert_eq!(f.read("new.txt"), None);
        assert!(!f.index().exists(), "the scratch index was left behind");

        store
            .restore(&f.git, &f.base, &f.index(), 2, |_| {})
            .unwrap();
        assert_eq!(f.read("a.txt").as_deref(), Some("third\n"));
        assert_eq!(f.read("new.txt").as_deref(), Some("new\n"));
        assert_eq!(
            f.listed(),
            [
                (1, CheckpointKind::Run),
                (2, CheckpointKind::BeforeRestore),
                (3, CheckpointKind::BeforeRestore)
            ]
        );
    }

    #[test]
    fn a_restore_never_overwrites_an_ignored_file() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        f.write("gen.txt", "kept in the checkpoint\n");
        store.save(&f.git, &f.base, &f.index()).unwrap();
        f.write(".gitignore", "gen.txt\n");
        f.write("gen.txt", "ignored and precious\n");

        let error = store
            .restore(&f.git, &f.base, &f.index(), 1, |_| panic!("saved"))
            .unwrap_err();
        assert!(error.contains(r#"write over "gen.txt""#), "{error}");
        assert_eq!(f.read("gen.txt").as_deref(), Some("ignored and precious\n"));
        assert!(f.read(".gitignore").is_some());
        assert!(!f.index().exists(), "the scratch index was left behind");
        assert_eq!(f.listed(), [(1, CheckpointKind::Run)]);
    }

    #[test]
    fn a_restore_never_swaps_an_ignored_file_and_a_folder() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        fs::create_dir(f.tree.join("gen")).unwrap();
        f.write("gen/out.txt", "a file in a folder\n");
        f.write("log", "a file\n");
        store.save(&f.git, &f.base, &f.index()).unwrap();
        fs::remove_dir_all(f.tree.join("gen")).unwrap();
        fs::remove_file(f.tree.join("log")).unwrap();
        f.write(".gitignore", "gen\nlog/\n");
        f.write("gen", "ignored where a folder was\n");
        fs::create_dir(f.tree.join("log")).unwrap();
        f.write("log/today.txt", "ignored where a file was\n");

        let error = store
            .restore(&f.git, &f.base, &f.index(), 1, |_| panic!("saved"))
            .unwrap_err();
        assert!(error.contains(r#""gen", "log""#), "{error}");
        assert_eq!(
            f.read("gen").as_deref(),
            Some("ignored where a folder was\n")
        );
        assert!(f.read("log/today.txt").is_some());
    }

    #[test]
    fn a_restore_replaces_a_folder_the_checkpoint_holds() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        f.write("log", "a file\n");
        store.save(&f.git, &f.base, &f.index()).unwrap();
        fs::remove_file(f.tree.join("log")).unwrap();
        fs::create_dir(f.tree.join("log")).unwrap();
        f.write("log/today.txt", "saved before the restore\n");

        store
            .restore(&f.git, &f.base, &f.index(), 1, |_| {})
            .unwrap();
        assert_eq!(f.read("log").as_deref(), Some("a file\n"));
        store
            .restore(&f.git, &f.base, &f.index(), 2, |_| {})
            .unwrap();
        assert_eq!(
            f.read("log/today.txt").as_deref(),
            Some("saved before the restore\n")
        );
    }

    #[test]
    fn a_restore_to_a_missing_checkpoint_saves_nothing() {
        let f = Fixture::new(LOCKED);
        let (mut store, _) = f.open();
        let error = store
            .restore(&f.git, &f.base, &f.index(), 7, |_| panic!("saved"))
            .unwrap_err();
        assert_eq!(error, "there is no checkpoint 7");
        assert_eq!(f.listed(), []);
    }

    #[test]
    fn stops_at_the_cap() {
        let f = Fixture::new(LOCKED);
        let name = format!("refs/c-harness/s1/turn-{MAX_CHECKPOINTS}");
        setup(&f.repo, &["update-ref", &name, &f.base.id]);
        let (mut store, _) = f.open();
        let error = store.save(&f.git, &f.base, &f.index()).unwrap_err();
        assert!(error.contains("the most it keeps"), "{error}");
    }

    #[test]
    fn reads_turn_numbers_strictly() {
        assert_eq!(turn_of("turn-1"), Some(1));
        assert_eq!(turn_of("turn-1000"), Some(1000));
        for name in [
            "turn-0",
            "turn-01",
            "turn-1001",
            "turn-+1",
            "turn-",
            "x-1",
            "turn-1 ",
        ] {
            assert_eq!(turn_of(name), None, "{name}");
        }
        assert!(valid_session("s-1_A"));
        assert!(!valid_session(""));
        assert!(!valid_session("a*"));
        assert!(!valid_session(&"a".repeat(65)));
    }
}
