//! The secret scan between "the tree is ready" and "anything is committed or pushed".
//!
//! Planted keys are built with `concat!` so this repository's own scan never sees one whole.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{Client, FakeOpener, bare_remote, fixture_lines, git, land_frame, repo, start_frame};

const TOKEN: &str = concat!("gh", "p_", "abcdefghijklmnopqrstuvwxyz0123456789");
const PEM: &str = concat!("-----BEGIN RSA PRIV", "ATE KEY-----");

/// A key in a new file refuses the land with its place and kind, and an acknowledgement
/// counts only for the tree the person looked at.
pub(super) fn a_secret_blocks_land_until_its_tree_is_acknowledged() {
    let repo = repo(true);
    let dir = repo.path();
    let remote = bare_remote();
    let mut client = stopped_session(dir);

    fs::create_dir_all(dir.join("src/deep")).expect("mkdir");
    let config = format!("export const a = 1;\nexport const b = 2;\nconst key = \"{TOKEN}\";\n");
    fs::write(dir.join("src/deep/config.ts"), &config).expect("write");
    client.send(&land_frame("l1", &remote.path, false));
    let first = refused(&mut client, "l1");
    assert_eq!(
        first["secret_scan"]["findings"],
        json!([{ "path": "src/deep/config.ts", "line": 3, "kind": "provider_token" }]),
        "{first}"
    );
    let message = first["message"].as_str().unwrap();
    assert!(message.contains("src/deep/config.ts:3"), "{message}");
    assert_nothing_landed(dir, &remote.path);

    let tree_a = first["secret_scan"]["tree"].as_str().unwrap().to_owned();
    fs::write(
        dir.join("src/deep/config.ts"),
        format!("{config}// edited\n"),
    )
    .expect("write");
    client.send(&acknowledged(
        land_frame("l2", &remote.path, false),
        &tree_a,
    ));
    let second = refused(&mut client, "l2");
    let tree_b = second["secret_scan"]["tree"].as_str().unwrap().to_owned();
    assert_ne!(tree_a, tree_b, "a change after the look is a new tree");
    assert_nothing_landed(dir, &remote.path);

    client.send(&acknowledged(
        land_frame("l3", &remote.path, false),
        &tree_b,
    ));
    let landed = client.expect("ch_landed");
    assert_eq!(landed["id"], "l3");
    assert_eq!(
        git(&remote.path, &["rev-parse", "refs/heads/ch/test"]),
        landed["commit"].as_str().expect("a commit"),
        "the remote branch is the commit that landed"
    );
    assert_eq!(client.finish(), Vec::<Value>::new());
}

/// A PEM header refuses the land; taking a key out of the repository does not.
pub(super) fn a_private_key_blocks_land_and_a_removed_key_does_not() {
    let repo = repo(true);
    let dir = repo.path();
    fs::write(dir.join("a.txt"), format!("first\n{TOKEN}\n")).expect("write");
    git(dir, &["commit", "-q", "-am", "an old mistake"]);
    let remote = bare_remote();
    let mut client = stopped_session(dir);

    fs::write(dir.join("id_rsa"), format!("{PEM}\nMIIE\n")).expect("write");
    client.send(&land_frame("l1", &remote.path, false));
    let frame = refused(&mut client, "l1");
    assert_eq!(
        frame["secret_scan"]["findings"],
        json!([{ "path": "id_rsa", "line": 1, "kind": "private_key" }]),
        "{frame}"
    );
    assert_nothing_landed(dir, &remote.path);

    fs::remove_file(dir.join("id_rsa")).expect("remove");
    fs::write(dir.join("a.txt"), "first\n").expect("write");
    client.send(&land_frame("l2", &remote.path, false));
    assert_eq!(client.expect("ch_landed")["id"], "l2");
    assert_eq!(client.finish(), Vec::<Value>::new());
}

/// A session that ran and stopped, so its work can land.
fn stopped_session(dir: &Path) -> Client {
    let mut client = Client::start(dir, Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");
    client.send(&start_frame(&[]));
    client.until("fake_started");
    for expected in fixture_lines() {
        assert_eq!(client.next_line(), expected);
    }
    client.send(&json!({ "type": "ch_stop", "id": "s1" }));
    client.expect("ch_stopped");
    client
}

/// The land refusal for `id`, checked on the raw line so no part of a key is sent.
fn refused(client: &mut Client, id: &str) -> Value {
    let line = client.next_line();
    for leak in [TOKEN, &TOKEN[4..], "BEGIN RSA", "const key"] {
        assert!(!line.contains(leak), "the refusal carries {leak:?}: {line}");
    }
    let frame: Value = serde_json::from_str(&line).expect("JSON");
    assert_eq!(frame["type"], "ch_error", "{frame}");
    assert_eq!(frame["id"], id);
    assert_eq!(frame["stage"], "land");
    assert!(frame["secret_scan"]["tree"].is_string(), "{frame}");
    frame
}

fn acknowledged(mut frame: Value, tree: &str) -> Value {
    frame["acknowledged_tree"] = json!(tree);
    frame
}

fn assert_nothing_landed(dir: &Path, remote: &Path) {
    assert_eq!(git(remote, &["for-each-ref"]), "", "something was pushed");
    assert_eq!(
        git(dir, &["for-each-ref", "refs/heads/ch/"]),
        "",
        "a branch was made"
    );
}
