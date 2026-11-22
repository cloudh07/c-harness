//! Drives `serve` the way the app does, with this test binary run again as a fake omp.
//!
//! The binary has four roles, picked from its arguments and environment: a verification
//! command (`--helper`), the fake omp (`CH_FAKE_OMP` set by `ch_start`), the test run itself,
//! and, when started by cargo, a launcher that reruns it with git isolated from the user's
//! own config and with subscription tokens in its environment that must never reach omp.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ch_protocol::{MAX_FRAME_BYTES, NO_OAUTH_ACCOUNTS, SUBSCRIPTION_TOKEN_ENV};
use ch_runner::{Options, PrOpener, PrRequest, serve};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../testdata/omp/approval-session.jsonl"
);

const HERMETIC: &str = "CH_SERVE_TEST_HERMETIC";
const FAKE_OMP: &str = "CH_FAKE_OMP";
const FAKE_EXIT: &str = "CH_FAKE_OMP_EXIT";
const FAKE_SPOOF: &str = "CH_FAKE_OMP_SPOOF";
const MODEL_KEY_ENV: &str = "CH_TEST_MODEL_KEY";
const MODEL_KEY: &str = "test-model-key-not-real";
const OAUTH_VALUE: &str = "test-oauth-not-real";
const CLIENT_OVERLAY: &str = "theme: dark\n";
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const PR_URL: &str = "https://example.invalid/octo/demo/pull/1";

/// Generous, so a slow CI machine fails only on a real hang.
const WAIT: Duration = Duration::from_secs(60);

#[path = "serve/approval.rs"]
mod approval;
#[path = "serve/checkpoints.rs"]
mod checkpoints;
#[path = "serve/secret_scan.rs"]
mod secret_scan;

const SCENARIOS: [(&str, fn()); 12] = [
    ("full_session", full_session),
    (
        "project_config_and_provider_notice",
        project_config_and_provider_notice,
    ),
    ("start_refusals", start_refusals),
    (
        "cached_approval_needs_a_new_omp",
        approval::cached_approval_needs_a_new_omp,
    ),
    ("omp_exit_is_reported", omp_exit_is_reported),
    (
        "a_check_that_writes_blocks_land",
        a_check_that_writes_blocks_land,
    ),
    ("protocol_errors", protocol_errors),
    ("binary_cli", binary_cli),
    (
        "checkpoints_and_restore",
        checkpoints::checkpoints_and_restore,
    ),
    (
        "checkpoints_need_a_session_worktree",
        checkpoints::checkpoints_need_a_session_worktree,
    ),
    (
        "a_secret_blocks_land_until_its_tree_is_acknowledged",
        secret_scan::a_secret_blocks_land_until_its_tree_is_acknowledged,
    ),
    (
        "a_private_key_blocks_land_and_a_removed_key_does_not",
        secret_scan::a_private_key_blocks_land_and_a_removed_key_does_not,
    ),
];

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "--helper") {
        return helper(&args[2..]);
    }
    if std::env::var_os(FAKE_OMP).is_some() {
        return fake_omp(&args[1..]);
    }
    if std::env::var_os(HERMETIC).is_none() {
        return rerun_hermetic();
    }
    run_scenarios()
}

/// `std::env::set_var` is unsafe, so the isolated environment comes from running again.
fn rerun_hermetic() -> ExitCode {
    let config = tempfile::NamedTempFile::new().expect("a temp file");
    let mut command = Command::new(std::env::current_exe().expect("own path"));
    command
        .args(std::env::args_os().skip(1))
        .env(HERMETIC, "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", config.path());
    for name in SUBSCRIPTION_TOKEN_ENV {
        command.env(name, OAUTH_VALUE);
    }
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        FAKE_OMP,
        FAKE_EXIT,
        FAKE_SPOOF,
        MODEL_KEY_ENV,
    ] {
        command.env_remove(name);
    }
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("could not rerun the tests: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_scenarios() -> ExitCode {
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let mut passed = 0;
    let mut failed = Vec::new();
    for (name, scenario) in SCENARIOS {
        if !filters.is_empty() && !filters.iter().any(|filter| name.contains(filter.as_str())) {
            continue;
        }
        let started = Instant::now();
        if panic::catch_unwind(scenario).is_ok() {
            passed += 1;
            println!("test {name} ... ok ({} ms)", started.elapsed().as_millis());
        } else {
            println!("test {name} ... FAILED");
            failed.push(name);
        }
    }
    println!("\nserve: {passed} passed; {} failed", failed.len());
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        println!("failed: {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// `--helper <exit code> <sleep secs>`: a verification command that reports what it inherited.
fn helper(args: &[OsString]) -> ExitCode {
    let number = |at: usize| {
        args.get(at)
            .and_then(|arg| arg.to_str())
            .and_then(|arg| arg.parse::<u8>().ok())
            .unwrap_or(0)
    };
    let present = |name: &str| {
        if std::env::var_os(name).is_some() {
            "present"
        } else {
            "absent"
        }
    };
    println!(
        "model_key={} fake_omp={}",
        present(MODEL_KEY_ENV),
        present(FAKE_OMP)
    );
    for name in SUBSCRIPTION_TOKEN_ENV {
        println!("{name}={}", present(name));
    }
    let _ = io::stdout().flush();
    thread::sleep(Duration::from_secs(number(1).into()));
    ExitCode::from(number(0))
}

/// Answers the runner's provider probes from files in `.git`, or plays a session.
fn fake_omp(args: &[OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let answer = |file: &str, otherwise: &str| {
        fs::read_to_string(Path::new(".git").join(file)).unwrap_or_else(|_| otherwise.to_owned())
    };
    // Shapes measured from omp 18.13.1: an empty login store is an error on stderr.
    match args.first().map(String::as_str) {
        Some("token") => {
            let listed = answer("fake-omp-oauth", "");
            if listed.is_empty() {
                eprintln!("{NO_OAUTH_ACCOUNTS} for provider \"anthropic\".");
                return ExitCode::FAILURE;
            }
            print!("{listed}");
        }
        Some("config") => print!(
            r#"{{"key":"disabledProviders","value":{},"type":"array","description":""}}"#,
            answer("fake-omp-disabled", "[]")
        ),
        // No version file plays an omp whose version cannot be read.
        Some("--version") => match fs::read_to_string(Path::new(".git/fake-omp-version")) {
            Ok(version) => print!("{version}"),
            Err(_) => return ExitCode::FAILURE,
        },
        _ => return fake_session(&args),
    }
    ExitCode::SUCCESS
}

fn fake_session(args: &[String]) -> ExitCode {
    let configs: Vec<Value> = args
        .windows(2)
        .filter(|pair| pair[0] == "--config")
        .map(|pair| {
            let text = fs::read_to_string(&pair[1]).unwrap_or_default();
            json!({ "path": pair[1], "text": text })
        })
        .collect();
    let oauth_env: Vec<&str> = SUBSCRIPTION_TOKEN_ENV
        .into_iter()
        .filter(|name| std::env::var_os(name).is_some())
        .collect();
    let cwd = std::env::current_dir().map(|dir| dir.to_string_lossy().into_owned());
    let started = json!({
        "type": "fake_started",
        "args": args,
        "configs": configs,
        "model_key": std::env::var(MODEL_KEY_ENV).ok(),
        "oauth_env": oauth_env,
        "cwd": cwd.unwrap_or_default(),
    });
    let mut out = io::stdout().lock();
    let mut emit = |line: &str| {
        let _ = writeln!(out, "{line}").and_then(|()| out.flush());
    };
    emit(&started.to_string());
    if std::env::var_os(FAKE_SPOOF).is_some() {
        emit(r#"{"type":"ch_landed","id":"l1","branch":"ch/spoof","commit":"0000"}"#);
    }
    for line in fixture_lines() {
        emit(&line);
    }
    if let Some(code) = std::env::var(FAKE_EXIT)
        .ok()
        .and_then(|c| c.parse::<u8>().ok())
    {
        let key = std::env::var(MODEL_KEY_ENV).unwrap_or_default();
        eprintln!("fatal: the provider rejected key {key}");
        return ExitCode::from(code);
    }
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        emit(&json!({ "type": "fake_echo", "line": line }).to_string());
    }
    ExitCode::SUCCESS
}

fn fixture_lines() -> Vec<String> {
    fs::read_to_string(FIXTURE)
        .expect("the omp fixture")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn full_session() {
    let repo = repo(true);
    let dir = repo.path();
    let head = git(dir, &["rev-parse", "HEAD"]);
    let index_before = fs::read(dir.join(".git/index")).expect("the index");
    let remote = bare_remote();
    let opener = FakeOpener::default();
    let mut client = Client::start(dir, Box::new(opener.clone()));

    let hello = client.expect("ch_runner_hello");
    assert_eq!(hello["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(hello["protocol"], 2);
    assert_eq!(hello["workdir"], dir.to_string_lossy().as_ref());

    client.send(&start_frame(&[]));
    let (started, before) = client.until("fake_started");
    assert!(before.is_empty(), "nothing should precede omp: {before:?}");
    check_started(&started, dir);
    for expected in fixture_lines() {
        assert_eq!(client.next_line(), expected);
    }

    let prompt = r#"{"type":"prompt","id":"p1","message":"hi"}"#;
    client.send_line(prompt);
    assert_eq!(client.expect("fake_echo")["line"], prompt);

    check_verify(&mut client, &head);

    fs::write(dir.join("new.txt"), "fresh\n").expect("write a file");
    client.send(&json!({ "type": "ch_diff", "id": "d1" }));
    let diff = client.expect("ch_diff_result");
    assert_eq!(diff["id"], "d1");
    assert_eq!(diff["base"], head.as_str());
    assert_eq!(diff["truncated"], false);
    assert!(diff["stat"].as_str().unwrap().contains("new.txt"), "{diff}");
    assert!(diff["patch"].as_str().unwrap().contains("+fresh"), "{diff}");

    let land = land_frame("l1", &remote.path, true);
    client.send(&land);
    let refused = client.expect("ch_error");
    assert_eq!(
        (refused["stage"].as_str(), refused["id"].as_str()),
        (Some("land"), Some("l1"))
    );

    client.send(&json!({ "type": "ch_stop", "id": "s1" }));
    let stopped = client.expect("ch_stopped");
    assert_eq!(
        (stopped["id"].as_str(), stopped["exit_code"].as_i64()),
        (Some("s1"), Some(0))
    );

    // The last check ran before new.txt existed, so it says nothing about the work now.
    client.send(&land);
    let stale = client.expect("ch_error");
    assert_eq!(stale["id"], "l1");
    assert!(
        stale["message"]
            .as_str()
            .unwrap()
            .contains("changed after the last check"),
        "{stale}"
    );
    assert_eq!(
        verify(&mut client, "v3", &helper_command(0, 0))["exit_code"],
        0
    );

    client.send(&land);
    let landed = client.expect("ch_landed");
    assert_eq!(landed["id"], "l1");
    assert_eq!(landed["branch"], "ch/test");
    assert_eq!(landed["pull_request_url"], PR_URL);
    let commit = landed["commit"].as_str().expect("a commit").to_owned();
    assert_eq!(
        *opener.requests.lock().unwrap(),
        ["octo/demo ch/test main Add new.txt"]
    );

    // A retry lands the same commit again rather than stacking a new one.
    client.send(&land);
    assert_eq!(client.expect("ch_landed")["commit"], commit.as_str());

    let rest = client.finish();
    assert!(rest.is_empty(), "{rest:?}");

    check_remote(&remote.path, &commit, &head);
    assert_eq!(
        fs::read(dir.join(".git/index")).expect("the index"),
        index_before,
        "the agent's index changed"
    );
    assert_eq!(git(dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(dir, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(git(dir, &["status", "--porcelain"]), "?? new.txt");
}

fn check_started(started: &Value, dir: &Path) {
    let args: Vec<&str> = started["args"]
        .as_array()
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(args.len(), 8, "{args:?}");
    assert_eq!(&args[..4], ["--mode", "rpc", "--approval-mode", "write"]);
    assert_eq!((args[4], args[6]), ("--config", "--config"));

    let configs = started["configs"].as_array().expect("configs");
    assert_eq!(configs[0]["text"], CLIENT_OVERLAY);
    let host = configs[1]["text"].as_str().expect("host overlay");
    assert!(
        host.starts_with("startup:\n  checkUpdate: false\nmemory:\n  backend: \"off\"\n"),
        "{host}"
    );
    assert_eq!(
        configs[1]["path"].as_str(),
        Some(args[7]),
        "host overlay last"
    );
    assert!(host.contains("google-antigravity"), "{host}");
    assert!(!host.contains("\"anthropic\""), "{host}");
    for config in configs {
        let path = Path::new(config["path"].as_str().expect("a path"));
        assert!(
            !path.starts_with(dir),
            "overlay inside the working tree: {}",
            path.display()
        );
    }

    assert_eq!(started["model_key"], MODEL_KEY);
    assert_eq!(started["oauth_env"], json!([]));
    let cwd = fs::canonicalize(started["cwd"].as_str().expect("cwd")).expect("cwd exists");
    assert_eq!(cwd, fs::canonicalize(dir).expect("workdir exists"));
}

fn check_verify(client: &mut Client, head: &str) {
    let evidence = verify(client, "v1", &helper_command(3, 0));
    assert_eq!(evidence["tree_before"], evidence["tree_after"]);
    assert_eq!(evidence["exit_code"], 3);
    assert_eq!(evidence["timed_out"], false);
    assert_eq!(evidence["head"], head);
    let output = evidence["output_tail"].as_str().expect("output");
    assert!(
        output.contains("model_key=absent fake_omp=absent"),
        "{output}"
    );
    for name in SUBSCRIPTION_TOKEN_ENV {
        assert!(output.contains(&format!("{name}=absent")), "{output}");
    }

    client.send(&json!({
        "type": "ch_verify",
        "id": "v2",
        "command": helper_command(0, 30),
        "timeout_secs": 1,
    }));
    let evidence = client.expect("ch_evidence");
    assert_eq!(evidence["id"], "v2");
    assert_eq!(evidence["timed_out"], true);
    assert_eq!(evidence["exit_code"], Value::Null);
}

/// Sends a `ch_verify` and returns its evidence, which must name the work it ran on.
fn verify(client: &mut Client, id: &str, command: &str) -> Value {
    client.send(&json!({ "type": "ch_verify", "id": id, "command": command }));
    let evidence = client.expect("ch_evidence");
    assert_eq!(evidence["id"], id);
    for tree in ["tree_before", "tree_after"] {
        assert!(evidence[tree].is_string(), "no {tree}: {evidence}");
    }
    evidence
}

/// A check that writes files ran on work that was moving, so land waits for a steady one.
fn a_check_that_writes_blocks_land() {
    let repo = repo(true);
    let dir = repo.path();
    let remote = bare_remote();
    let mut client = Client::start(dir, Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");
    client.send(&start_frame(&[]));
    client.until("fake_started");
    for expected in fixture_lines() {
        assert_eq!(client.next_line(), expected);
    }
    client.send(&json!({ "type": "ch_stop", "id": "s1" }));
    client.expect("ch_stopped");

    fs::write(dir.join("new.txt"), "fresh\n").expect("write a file");
    // `echo x> file` writes the file under both `sh` and `cmd`.
    let moving = verify(&mut client, "v1", "echo x> during.txt");
    assert_eq!(moving["exit_code"], 0, "{moving}");
    assert_ne!(moving["tree_before"], moving["tree_after"], "{moving}");
    client.send(&land_frame("l1", &remote.path, false));
    let refused = client.expect("ch_error");
    assert_eq!(refused["id"], "l1");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("changed while the last check ran"),
        "{refused}"
    );

    let steady = verify(&mut client, "v2", &helper_command(0, 0));
    assert_eq!(steady["tree_before"], steady["tree_after"]);
    client.send(&land_frame("l2", &remote.path, false));
    assert_eq!(client.expect("ch_landed")["id"], "l2");
    assert_eq!(client.finish(), Vec::<Value>::new());
}

fn check_remote(remote: &Path, commit: &str, parent: &str) {
    assert_eq!(git(remote, &["rev-parse", "refs/heads/ch/test"]), commit);
    let author = "C-Harness Test <bot@example.invalid>";
    assert_eq!(
        git(
            remote,
            &["show", "-s", "--format=%an <%ae>|%cn <%ce>|%P", commit]
        ),
        format!("{author}|{author}|{parent}")
    );
    assert_eq!(
        git(remote, &["log", "-1", "--format=%s", commit]),
        "Add new.txt"
    );
    assert_eq!(
        git(remote, &["ls-tree", "-r", "--name-only", commit]),
        "a.txt\nnew.txt"
    );
    assert_eq!(
        git(remote, &["show", &format!("{commit}:new.txt")]),
        "fresh"
    );
}

fn project_config_and_provider_notice() {
    let repo = repo(false);
    let dir = repo.path();
    fs::create_dir(dir.join(".omp")).expect("mkdir");
    fs::write(dir.join(".omp/settings.yml"), "x: 1\n").expect("write");
    // A project asking for full auto: the client's flag is what omp gets.
    fs::write(
        dir.join(".omp/config.yml"),
        "tools:\n  approvalMode: yolo\n",
    )
    .expect("write");
    fs::create_dir(dir.join(".claude")).expect("mkdir");
    fs::write(dir.join(".claude/settings.json"), "{}\n").expect("write");
    fs::write(
        dir.join(".git/fake-omp-oauth"),
        "1. someone@example.invalid\n",
    )
    .expect("write");
    fs::write(dir.join(".git/fake-omp-disabled"), r#"["openai"]"#).expect("write");
    let remote = bare_remote();
    let mut client = Client::start(dir, Box::new(FakeOpener::default()));

    client.expect("ch_runner_hello");
    let notice = client.expect("ch_notice");
    assert_eq!(notice["kind"], "project_omp_config");
    let message = notice["message"].as_str().expect("message");
    assert!(message.contains(".omp, .claude"), "{message}");
    assert!(!message.contains(".codex"), "{message}");

    let mut frame = start_frame(&[]);
    frame["omp_args"] = json!(["--mode", "rpc", "--approval-mode", "write"]);
    client.send(&frame);
    let (started, before) = client.until("fake_started");
    let args: Vec<&str> = started["args"]
        .as_array()
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(&args[..4], ["--mode", "rpc", "--approval-mode", "write"]);
    assert!(!args.iter().any(|a| a.contains("yolo")), "{args:?}");
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0]["kind"], "provider_disabled");
    assert!(
        before[0]["message"]
            .as_str()
            .unwrap()
            .contains("subscription login"),
        "{before:?}"
    );
    let host = started["configs"][1]["text"]
        .as_str()
        .expect("host overlay");
    assert_eq!(started["configs"][1]["path"].as_str(), args.last().copied());
    assert!(host.contains("memory:\n  backend: \"off\"\n"), "{host}");
    assert!(host.contains("- \"anthropic\""), "{host}");
    assert!(host.contains("- \"openai\""), "{host}");
    for _ in fixture_lines() {
        client.next_line();
    }

    client.send(&json!({ "type": "ch_diff", "id": "d1" }));
    let diff = client.expect("ch_diff_result");
    assert_eq!(diff["base"], EMPTY_TREE);
    let stat = diff["stat"].as_str().expect("stat");
    assert!(
        stat.contains(".omp/settings.yml") && stat.contains(".claude/settings.json"),
        "{stat}"
    );

    client.send(&json!({ "type": "ch_stop", "id": "s1" }));
    client.expect("ch_stopped");
    client.send(&land_frame("l1", &remote.path, false));
    let landed = client.expect("ch_landed");
    assert!(landed.get("pull_request_url").is_none(), "{landed}");
    let commit = landed["commit"].as_str().expect("a commit");
    assert_eq!(
        git(&remote.path, &["show", "-s", "--format=%P", commit]),
        ""
    );
    assert_eq!(client.finish(), Vec::<Value>::new());
}

fn start_refusals() {
    let repo = repo(true);
    let mut client = Client::start(repo.path(), Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");

    client.send_line(r#"{"type":"prompt","message":"too early"}"#);
    let early = client.expect("ch_error");
    assert_eq!(early["stage"], "protocol");
    assert!(early.get("id").is_none(), "{early}");

    // Names are matched without regard to case, as Windows does.
    client.send(&start_frame(&[("claude_code_oauth_token", OAUTH_VALUE)]));
    let refused = client.expect("ch_error");
    assert_eq!(refused["stage"], "start");
    let text = refused.to_string();
    assert!(
        !text.contains(OAUTH_VALUE) && !text.contains(MODEL_KEY),
        "{text}"
    );

    let mut yolo = start_frame(&[]);
    yolo["omp_args"] = json!(["--mode", "rpc", "--yolo"]);
    client.send(&yolo);
    let refused = client.expect("ch_error");
    assert_eq!(refused["stage"], "start");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("--approval-mode"),
        "{refused}"
    );

    client.send(&start_frame(&[]));
    let (started, before) = client.until("fake_started");
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(started["oauth_env"], json!([]));

    client.send(&start_frame(&[]));
    let (again, skipped) = client.until("ch_error");
    assert_eq!(again["stage"], "start");
    assert_only_omp_lines(&skipped);
    assert_only_omp_lines(&client.finish());

    let nested = repo.path().join("nested");
    std::fs::create_dir(&nested).expect("nested dir");
    let mut inner = Client::start(&nested, Box::new(FakeOpener::default()));
    inner.expect("ch_runner_hello");
    inner.send(&start_frame(&[]));
    let refused = inner.expect("ch_error");
    assert_eq!(
        refused["stage"], "repo",
        "the repository check names its own stage"
    );
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("inside the repository"),
        "{refused}"
    );
    assert_eq!(inner.finish(), Vec::<Value>::new());

    let missing = OsString::from("ch-test-omp-that-is-not-installed");
    let mut absent = Client::start_with(repo.path(), missing, Box::new(FakeOpener::default()));
    absent.expect("ch_runner_hello");
    absent.send(&start_frame(&[]));
    let (refused, notices) = absent.until("ch_error");
    assert!(
        notices.iter().all(|n| n["type"] == "ch_notice"),
        "{notices:?}"
    );
    assert_eq!(refused["stage"], "start");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("on the PATH that ch-runner sees"),
        "{refused}"
    );
    assert_eq!(absent.finish(), Vec::<Value>::new());
}

fn omp_exit_is_reported() {
    let repo = repo(true);
    let mut client = Client::start(repo.path(), Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");

    client.send(&start_frame(&[(FAKE_EXIT, "7"), (FAKE_SPOOF, "1")]));
    client.until("fake_started");
    let dropped = client.expect("ch_notice");
    assert_eq!(dropped["kind"], "omp_frame_dropped");
    for expected in fixture_lines() {
        assert_eq!(client.next_line(), expected);
    }
    let exited = client.expect("ch_omp_exited");
    assert_eq!(exited["exit_code"], 7);
    let tail = exited["stderr_tail"].to_string();
    assert!(
        tail.contains("<hidden>") && !tail.contains(MODEL_KEY),
        "{tail}"
    );

    client.send_line(r#"{"type":"prompt","message":"too late"}"#);
    assert_eq!(client.expect("ch_error")["stage"], "protocol");

    client.send(&json!({ "type": "ch_stop", "id": "s1" }));
    let stopped = client.expect("ch_stopped");
    assert_eq!(stopped["exit_code"], 7);

    let remote = bare_remote();
    client.send(&json!({ "type": "ch_verify", "id": "v1", "command": helper_command(0, 2) }));
    client.send(&land_frame("l0", &remote.path, false));
    let busy = client.expect("ch_error");
    assert_eq!(busy["id"], "l0");
    assert!(
        busy["message"].as_str().unwrap().contains("still running"),
        "{busy}"
    );
    assert_eq!(client.expect("ch_evidence")["id"], "v1");

    client.send(&land_frame("l1", &remote.path, false));
    let nothing = client.expect("ch_error");
    assert_eq!(
        (nothing["stage"].as_str(), nothing["id"].as_str()),
        (Some("land"), Some("l1"))
    );
    assert!(
        nothing["message"]
            .as_str()
            .unwrap()
            .contains("nothing to land"),
        "{nothing}"
    );
    assert_eq!(client.finish(), Vec::<Value>::new());
}

fn protocol_errors() {
    let repo = repo(true);
    let remote = bare_remote();
    let mut client = Client::start(repo.path(), Box::new(FakeOpener::default()));
    client.expect("ch_runner_hello");

    for line in ["not json", r#"{"no_type":true}"#, "[1,2]"] {
        client.send_line(line);
        let error = client.expect("ch_error");
        assert_eq!(error["stage"], "protocol", "{line}");
        assert!(error.get("id").is_none(), "{error}");
    }

    client.send_line(
        r#"{"type":"ch_land","id":"l9","token":"test-token-not-real","author":"test-token-not-real"}"#,
    );
    let bad = client.expect("ch_error");
    assert_eq!(
        (bad["stage"].as_str(), bad["id"].as_str()),
        (Some("protocol"), Some("l9"))
    );
    assert!(!bad.to_string().contains("test-token-not-real"), "{bad}");

    client.send_line(&format!(
        r#"{{"type":"prompt","pad":"{}"}}"#,
        "x".repeat(MAX_FRAME_BYTES)
    ));
    let long = client.expect("ch_error");
    assert_eq!(long["stage"], "protocol");

    // Reading goes on after the long line, and nothing works before ch_start.
    let early = [
        (
            json!({ "type": "ch_verify", "id": "v0", "command": "exit 0" }),
            "verify",
        ),
        (json!({ "type": "ch_diff", "id": "d0" }), "diff"),
        (json!({ "type": "ch_checkpoint", "id": "c0" }), "checkpoint"),
        (
            json!({ "type": "ch_restore", "id": "r0", "turn": 1 }),
            "restore",
        ),
        (json!({ "type": "ch_stop", "id": "s0" }), "stop"),
        (land_frame("l0", &remote.path, false), "land"),
    ];
    for (frame, stage) in early {
        client.send(&frame);
        let error = client.expect("ch_error");
        assert_eq!(error["stage"], stage);
        assert_eq!(error["id"], frame["id"]);
    }
    assert_eq!(client.finish(), Vec::<Value>::new());
}

fn binary_cli() {
    let exe = env!("CARGO_BIN_EXE_ch-runner");
    let version = Command::new(exe)
        .arg("--version")
        .output()
        .expect("run ch-runner");
    assert!(version.status.success());
    let stdout = String::from_utf8(version.stdout).expect("utf-8");
    let expected = format!("ch-runner {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(stdout.lines().next(), Some(expected.as_str()));

    let usage = Command::new(exe)
        .arg("serve")
        .output()
        .expect("run ch-runner");
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("--workdir"));

    let gone = tempfile::tempdir().expect("temp dir");
    let missing = Command::new(exe)
        .args(["serve", "--workdir"])
        .arg(gone.path().join("missing"))
        .stdin(Stdio::null())
        .output()
        .expect("run ch-runner");
    assert_eq!(missing.status.code(), Some(1));

    let repo = repo(true);
    let mut child = Command::new(exe)
        .args(["serve", "--workdir"])
        .arg(repo.path())
        .arg("--omp")
        .arg(std::env::current_exe().expect("own path"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start ch-runner");
    let lines = read_lines(child.stdout.take().expect("stdout"));
    let mut stdin = child.stdin.take().expect("stdin");
    let next = || -> Value {
        let line = lines.recv_timeout(WAIT).expect("a line from ch-runner");
        serde_json::from_str(&line).expect("JSON")
    };
    assert_eq!(next()["type"], "ch_runner_hello");
    writeln!(stdin, "{}", start_frame(&[])).expect("write to ch-runner");
    while next()["type"] != "fake_started" {}
    drop(stdin);
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "ch-runner did not exit after stdin closed"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
}

struct Client {
    input: Option<io::PipeWriter>,
    lines: Receiver<String>,
    server: Option<JoinHandle<io::Result<()>>>,
}

impl Client {
    fn start(workdir: &Path, opener: Box<dyn PrOpener>) -> Self {
        let own = std::env::current_exe().expect("own path");
        Self::start_with(workdir, own.into_os_string(), opener)
    }

    fn start_with(workdir: &Path, omp_program: OsString, opener: Box<dyn PrOpener>) -> Self {
        let (reader, writer) = io::pipe().expect("a pipe");
        let (sender, lines) = mpsc::channel();
        let options = Options {
            workdir: workdir.to_owned(),
            omp_program,
            pr_opener: opener,
        };
        let output = LineSink {
            lines: sender,
            pending: Vec::new(),
        };
        let server = thread::spawn(move || serve(reader, output, options));
        Self {
            input: Some(writer),
            lines,
            server: Some(server),
        }
    }

    fn send(&mut self, frame: &Value) {
        self.send_line(&frame.to_string());
    }

    fn send_line(&mut self, line: &str) {
        let input = self.input.as_mut().expect("input still open");
        input
            .write_all(format!("{line}\n").as_bytes())
            .expect("write to the runner");
    }

    fn next_line(&mut self) -> String {
        self.lines
            .recv_timeout(WAIT)
            .unwrap_or_else(|e| panic!("no line from the runner: {e}"))
    }

    fn next(&mut self) -> Value {
        let line = self.next_line();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    fn expect(&mut self, kind: &str) -> Value {
        let frame = self.next();
        assert_eq!(frame["type"], kind, "{frame}");
        frame
    }

    /// The first frame of `kind`, and every frame before it.
    fn until(&mut self, kind: &str) -> (Value, Vec<Value>) {
        let mut before = Vec::new();
        loop {
            let frame = self.next();
            if frame["type"] == kind {
                return (frame, before);
            }
            before.push(frame);
        }
    }

    /// Closes the runner's input, waits for it to end, and returns what it wrote meanwhile.
    fn finish(mut self) -> Vec<Value> {
        drop(self.input.take());
        let server = self.server.take().expect("server");
        let deadline = Instant::now() + WAIT;
        while !server.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the runner did not end after its input closed"
            );
            thread::sleep(Duration::from_millis(20));
        }
        server
            .join()
            .expect("the runner did not panic")
            .expect("the runner ended cleanly");
        self.lines
            .try_iter()
            .map(|line| serde_json::from_str(&line).expect("JSON"))
            .collect()
    }
}

/// The runner's stdout, split into lines for the test to read.
struct LineSink {
    lines: Sender<String>,
    pending: Vec<u8>,
}

impl Write for LineSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8(line[..end].to_vec())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.lines
                .send(line)
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FakeOpener {
    requests: Arc<Mutex<Vec<String>>>,
}

impl PrOpener for FakeOpener {
    fn requires_token(&self) -> bool {
        false
    }

    fn open(&self, request: &PrRequest<'_>, scratch: &Path) -> Result<String, String> {
        assert!(scratch.is_dir());
        self.requests.lock().unwrap().push(format!(
            "{} {} {} {}",
            request.repo, request.head, request.base, request.title
        ));
        Ok(PR_URL.to_owned())
    }
}

fn read_lines(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { return };
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    receiver
}

fn start_frame(extra_env: &[(&str, &str)]) -> Value {
    let mut env = BTreeMap::from([
        (FAKE_OMP.to_owned(), "1".to_owned()),
        (MODEL_KEY_ENV.to_owned(), MODEL_KEY.to_owned()),
    ]);
    env.extend(
        extra_env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    json!({
        "type": "ch_start",
        "session": "s-test",
        "omp_args": ["--mode", "rpc", "--approval-mode", "write"],
        "config_overlay": CLIENT_OVERLAY,
        "env": env,
    })
}

fn land_frame(id: &str, remote: &Path, pull_request: bool) -> Value {
    let mut frame = json!({
        "type": "ch_land",
        "id": id,
        "branch": "ch/test",
        "message": "Add new.txt\n\nWritten by the agent.",
        "author": { "name": "C-Harness Test", "email": "bot@example.invalid" },
        "remote_url": remote.to_string_lossy(),
    });
    if pull_request {
        frame["pull_request"] = json!({
            "repo": "octo/demo",
            "base": "main",
            "title": "Add new.txt",
            "body": "From a test.",
        });
    }
    frame
}

/// A verification command that runs this binary as `--helper`, quoted for `sh` and `cmd`.
fn helper_command(exit_code: u8, sleep_secs: u8) -> String {
    let exe = std::env::current_exe().expect("own path");
    format!("\"{}\" --helper {exit_code} {sleep_secs}", exe.display())
}

fn assert_only_omp_lines(frames: &[Value]) {
    for frame in frames {
        let kind = frame["type"].as_str().unwrap_or_default();
        assert!(!kind.starts_with("ch_"), "unexpected runner frame {frame}");
    }
}

struct Remote {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn bare_remote() -> Remote {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("remote.git");
    git(dir.path(), &["init", "--bare", "-q", "remote.git"]);
    Remote { _dir: dir, path }
}

fn repo(with_commit: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    git(dir.path(), &["init", "-q"]);
    if with_commit {
        fs::write(dir.path().join("a.txt"), "first\n").expect("write");
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
    }
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Setup",
            "-c",
            "user.email=setup@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
