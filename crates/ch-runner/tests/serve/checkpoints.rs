//! Checkpoints, ranged diffs and restores in a worktree locked for the session, the way
//! ch-runtime makes one, and their refusals in any other checkout.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{
    Client, FakeOpener, bare_remote, fixture_lines, git, helper_command, land_frame, repo,
    start_frame, verify,
};

/// The lock reason ch-runtime gives the worktree of session `s-test`.
const LOCK_REASON: &str = "c-harness session s-test";
const NO_CHECKPOINTS: &str = "this working tree keeps no checkpoints";

pub(super) fn checkpoints_and_restore() {
    let repo = repo(true);
    let trees = tempfile::tempdir().expect("temp dir");
    let tree = trees.path().join("wt");
    let tree_arg = tree.to_str().expect("a UTF-8 path");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "--lock",
            "--reason",
            LOCK_REASON,
            tree_arg,
        ],
    );

    let mut client = start(&tree);
    assert_eq!(listed_at_start(&mut client)["checkpoints"], json!([]));
    save_and_diff_a_turn(&mut client, &tree);
    restore_and_undo(&mut client, &tree);
    refuse_to_write_over_an_ignored_file(&mut client, &tree);
    client.finish();

    // A runner started later in the same worktree finds them and counts on.
    let mut client = start(&tree);
    let listed = listed_at_start(&mut client);
    let turns: Vec<(u64, &str)> = listed["checkpoints"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|c| {
            (
                c["turn"].as_u64().unwrap_or_default(),
                c["kind"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        turns,
        [
            (1, "run"),
            (2, "before_restore"),
            (3, "before_restore"),
            (4, "run")
        ]
    );
    client.send(&json!({ "type": "ch_checkpoint", "id": "c9" }));
    expect_checkpointed(&mut client, "c9", 5, "run");
    client.finish();
}

/// A checkout C-Harness did not make for the session keeps no checkpoints.
pub(super) fn checkpoints_need_a_session_worktree() {
    let repo = repo(true);
    let mut client = start(repo.path());
    let (_, before) = client.until("fake_started");
    assert_eq!(before, Vec::<Value>::new());
    for expected in fixture_lines() {
        assert_eq!(client.next_line(), expected, "no ch_checkpoints frame");
    }
    let refused = [
        (json!({ "type": "ch_checkpoint", "id": "c1" }), "checkpoint"),
        (
            json!({ "type": "ch_restore", "id": "r1", "turn": 1 }),
            "restore",
        ),
        (json!({ "type": "ch_diff", "id": "d1", "from": 1 }), "diff"),
    ];
    for (frame, stage) in refused {
        client.send(&frame);
        let error = client.expect("ch_error");
        assert_eq!(
            (error["stage"].as_str(), &error["id"]),
            (Some(stage), &frame["id"])
        );
        assert_eq!(error["message"], NO_CHECKPOINTS);
    }

    // The base is turn 0, so a range of it still works.
    client.send(&json!({ "type": "ch_diff", "id": "d2", "from": 0, "to": 0 }));
    let diff = client.expect("ch_diff_result");
    assert_eq!(
        (diff["id"].as_str(), diff["patch"].as_str()),
        (Some("d2"), Some(""))
    );
    client.finish();
}

fn start(workdir: &Path) -> Client {
    let mut client = Client::start(workdir, Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");
    client.send(&start_frame(&[]));
    client
}

/// The `ch_checkpoints` frame, which may come before, among or after omp's first lines.
fn listed_at_start(client: &mut Client) -> Value {
    let fixture = fixture_lines();
    let mut omp_lines = Vec::new();
    let mut listed = None;
    while omp_lines.len() <= fixture.len() || listed.is_none() {
        let line = client.next_line();
        let frame: Value = serde_json::from_str(&line).expect("JSON");
        if frame["type"] == "ch_checkpoints" {
            assert!(listed.is_none(), "listed twice: {line}");
            listed = Some(frame);
        } else {
            omp_lines.push(line);
        }
    }
    assert!(omp_lines[0].contains("fake_started"), "{}", omp_lines[0]);
    assert_eq!(omp_lines[1..], fixture[..]);
    listed.expect("read above")
}

/// Turn 1 edits a.txt and adds gen.txt; turn 2 goes on and passes a check.
fn save_and_diff_a_turn(client: &mut Client, tree: &Path) {
    fs::write(tree.join("a.txt"), "second\n").expect("write");
    fs::write(tree.join("gen.txt"), "kept\n").expect("write");
    client.send(&json!({ "type": "ch_checkpoint", "id": "c1" }));
    expect_checkpointed(client, "c1", 1, "run");

    client.send(&json!({ "type": "ch_diff", "id": "d1", "from": 0, "to": 1 }));
    let turn = client.expect("ch_diff_result");
    assert_eq!(
        (turn["from"].as_u64(), turn["to"].as_u64()),
        (Some(0), Some(1))
    );
    let patch = turn["patch"].as_str().expect("patch");
    assert!(
        patch.contains("+second") && patch.contains("+kept"),
        "{turn}"
    );

    fs::write(tree.join("a.txt"), "third\n").expect("write");
    fs::write(tree.join("b.txt"), "new\n").expect("write");
    fs::remove_file(tree.join("gen.txt")).expect("remove");
    client.send(&json!({ "type": "ch_diff", "id": "d2", "from": 1 }));
    let since = client.expect("ch_diff_result");
    assert_eq!(
        (since["from"].as_u64(), since["to"].as_u64()),
        (Some(1), None)
    );
    let patch = since["patch"].as_str().expect("patch");
    for change in ["-second", "+third", "+new", "-kept"] {
        assert!(patch.contains(change), "no {change}: {since}");
    }

    assert_eq!(verify(client, "v1", &helper_command(0, 0))["exit_code"], 0);
    client.send(&json!({ "type": "ch_stop", "id": "s1" }));
    client.expect("ch_stopped");
}

/// Going back to turn 1 leaves the last check behind; undoing the restore brings it back.
fn restore_and_undo(client: &mut Client, tree: &Path) {
    let remote = bare_remote();
    let land = land_frame("l1", &remote.path, false);

    restore(client, "r1", 1, 2);
    assert_eq!(read(tree, "a.txt").as_deref(), Some("second\n"));
    assert_eq!(read(tree, "gen.txt").as_deref(), Some("kept\n"));
    assert_eq!(read(tree, "b.txt"), None);
    client.send(&land);
    let stale = client.expect("ch_error");
    assert_eq!(stale["stage"], "land");
    let message = stale["message"].as_str().expect("message");
    assert!(message.contains("changed after the last check"), "{stale}");

    restore(client, "r2", 2, 3);
    assert_eq!(read(tree, "a.txt").as_deref(), Some("third\n"));
    assert_eq!(read(tree, "b.txt").as_deref(), Some("new\n"));
    assert_eq!(read(tree, "gen.txt"), None);
    client.send(&land);
    assert_eq!(client.expect("ch_landed")["id"], "l1");
}

/// gen.txt is ignored now, so going back to turn 1 would write over it unseen.
fn refuse_to_write_over_an_ignored_file(client: &mut Client, tree: &Path) {
    fs::write(tree.join(".gitignore"), "gen.txt\n").expect("write");
    fs::write(tree.join("gen.txt"), "local\n").expect("write");
    client.send(&json!({ "type": "ch_restore", "id": "r3", "turn": 1 }));
    let refused = client.expect("ch_error");
    assert_eq!(
        (refused["stage"].as_str(), refused["id"].as_str()),
        (Some("restore"), Some("r3"))
    );
    let message = refused["message"].as_str().expect("message");
    assert!(message.contains(r#"write over "gen.txt""#), "{refused}");
    assert_eq!(read(tree, "gen.txt").as_deref(), Some("local\n"));

    client.send(&json!({ "type": "ch_restore", "id": "r4", "turn": 99 }));
    let missing = client.expect("ch_error");
    assert_eq!(missing["message"], "there is no checkpoint 99");

    // Neither refusal saved a checkpoint.
    client.send(&json!({ "type": "ch_checkpoint", "id": "c2" }));
    expect_checkpointed(client, "c2", 4, "run");
}

fn restore(client: &mut Client, id: &str, turn: u64, saved_as: u64) {
    client.send(&json!({ "type": "ch_restore", "id": id, "turn": turn }));
    expect_checkpointed(client, id, saved_as, "before_restore");
    let restored = client.expect("ch_restored");
    assert_eq!(
        (restored["id"].as_str(), restored["turn"].as_u64()),
        (Some(id), Some(turn))
    );
}

fn expect_checkpointed(client: &mut Client, id: &str, turn: u64, kind: &str) {
    let saved = client.expect("ch_checkpointed");
    let checkpoint = &saved["checkpoint"];
    assert_eq!(
        (
            saved["id"].as_str(),
            checkpoint["turn"].as_u64(),
            checkpoint["kind"].as_str()
        ),
        (Some(id), Some(turn), Some(kind)),
        "{saved}"
    );
    let commit = checkpoint["commit"].as_str().unwrap_or_default();
    assert!(matches!(commit.len(), 40 | 64), "{saved}");
}

fn read(tree: &Path, name: &str) -> Option<String> {
    fs::read_to_string(tree.join(name)).ok()
}
