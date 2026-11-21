//! The runner starts omp only under an approval mode the client names, and takes the mode
//! that remembers grants only from an omp that knows it.

use std::fs;

use serde_json::{Value, json};

use super::{Client, FakeOpener, assert_only_omp_lines, repo, start_frame};

const CACHED: &str = "always-ask-cached";

pub(super) fn cached_approval_needs_a_new_omp() {
    let repo = repo(true);
    let mut client = Client::start(repo.path(), Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");

    let mut unnamed = start_frame(&[]);
    unnamed["omp_args"] = json!(["--mode", "rpc"]);
    refuse(&mut client, &unnamed, "must hold --approval-mode");

    let mut twice = start_frame(&[]);
    twice["omp_args"] = json!(["--approval-mode", "write", "--approval-mode=write"]);
    refuse(&mut client, &twice, "only once");

    let mut cached = start_frame(&[]);
    cached["omp_args"] = json!(["--mode", "rpc", "--approval-mode", CACHED]);
    refuse(&mut client, &cached, "could not read");

    let version = repo.path().join(".git/fake-omp-version");
    fs::write(&version, "omp/18.12.2\n").expect("write");
    refuse(&mut client, &cached, "this omp is 18.12.2");

    fs::write(&version, "omp/18.12.3\n").expect("write");
    client.send(&cached);
    let (started, before) = client.until("fake_started");
    assert_eq!(before, Vec::<Value>::new());
    let args: Vec<&str> = started["args"]
        .as_array()
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(&args[..4], ["--mode", "rpc", "--approval-mode", CACHED]);
    assert_only_omp_lines(&client.finish());
}

/// Sends `frame` and expects a start refusal that names `reason`.
fn refuse(client: &mut Client, frame: &Value, reason: &str) {
    client.send(frame);
    let refused = client.expect("ch_error");
    assert_eq!(refused["stage"], "start");
    let message = refused["message"].as_str().expect("message");
    assert!(message.contains(reason), "{refused}");
}
