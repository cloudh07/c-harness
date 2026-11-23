//! `ch-runner preflight` run as the app runs it, with this test binary as the fake omp.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

use super::{FAKE_ANSWERS, FAKE_OMP, MODEL_KEY, MODEL_KEY_ENV, repo};

/// Runs the real binary against `workdir`, with `stdin` written when it is given.
fn preflight(workdir: &Path, answers: &Path, extra: &[&str], stdin: Option<&Value>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ch-runner"))
        .args(["preflight", "--workdir"])
        .arg(workdir)
        .arg("--omp")
        .arg(std::env::current_exe().expect("own path"))
        .args(extra)
        .env(FAKE_OMP, "1")
        .env(FAKE_ANSWERS, answers)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start ch-runner");
    if let Some(request) = stdin {
        let mut pipe = child.stdin.take().expect("stdin");
        pipe.write_all(request.to_string().as_bytes())
            .expect("write the request");
    }
    child.wait_with_output().expect("wait for ch-runner")
}

fn report(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "preflight failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).expect("utf-8");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    serde_json::from_str(&stdout).expect("one JSON report")
}

fn answers_with_version() -> tempfile::TempDir {
    let answers = tempfile::tempdir().expect("temp dir");
    fs::write(answers.path().join("fake-omp-version"), "omp/18.13.1\n").expect("write");
    answers
}

pub(super) fn preflight_reports_the_workdir() {
    let answers = answers_with_version();
    let repo = repo(true);
    fs::write(repo.path().join("Cargo.toml"), "[package]\n").expect("write");

    let found = report(&preflight(repo.path(), answers.path(), &[], None));
    assert_eq!(found["protocol"], 4);
    assert_eq!(found["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(found["omp_version"], "18.13.1");
    assert_eq!(found.get("git_problem"), None);
    assert_eq!(found["verify_commands"], json!(["cargo test"]));
    assert_eq!(found.get("model_test"), None);
    assert!(
        !answers.path().join("fake-omp-print-seen").exists(),
        "no model test without --test-model"
    );

    let empty = super::repo(false);
    let fresh = report(&preflight(empty.path(), answers.path(), &[], None));
    assert_eq!(fresh.get("git_problem"), None, "no commits yet is fine");
    assert_eq!(fresh["verify_commands"], json!([]));

    let plain = tempfile::tempdir().expect("temp dir");
    let not_git = report(&preflight(plain.path(), answers.path(), &[], None));
    assert!(
        not_git["git_problem"]
            .as_str()
            .expect("a git problem")
            .contains("not a git"),
        "{not_git}"
    );

    fs::create_dir(repo.path().join("sub")).expect("mkdir");
    let nested = report(&preflight(
        &repo.path().join("sub"),
        answers.path(),
        &[],
        None,
    ));
    assert!(
        nested["git_problem"]
            .as_str()
            .expect("a git problem")
            .contains("inside the repository"),
        "{nested}"
    );

    let gone = report(&preflight(
        &plain.path().join("gone"),
        answers.path(),
        &[],
        None,
    ));
    assert!(
        gone["git_problem"]
            .as_str()
            .expect("a git problem")
            .contains("not a folder"),
        "{gone}"
    );
    assert_eq!(gone["omp_version"], "18.13.1", "omp is still checked");

    let mut no_workdir = Command::new(env!("CARGO_BIN_EXE_ch-runner"));
    no_workdir
        .args(["preflight", "--omp"])
        .arg(std::env::current_exe().expect("own path"))
        .env(FAKE_OMP, "1")
        .env(FAKE_ANSWERS, answers.path())
        .stdin(Stdio::null());
    let omp_only = report(&no_workdir.output().expect("run ch-runner"));
    assert_eq!(omp_only.get("git_problem"), None);
    assert_eq!(omp_only["verify_commands"], json!([]));
    assert_eq!(omp_only["omp_version"], "18.13.1");

    let unreadable = tempfile::tempdir().expect("temp dir");
    let no_version = report(&preflight(repo.path(), unreadable.path(), &[], None));
    assert_eq!(no_version.get("omp_version"), None);

    let mut missing_omp = Command::new(env!("CARGO_BIN_EXE_ch-runner"));
    missing_omp
        .args(["preflight", "--workdir"])
        .arg(repo.path())
        .args(["--omp", "ch-runner-test-no-such-omp"])
        .stdin(Stdio::null());
    let no_omp = report(&missing_omp.output().expect("run ch-runner"));
    assert_eq!(no_omp.get("omp_version"), None);
}

pub(super) fn preflight_tests_the_model_key() {
    let answers = answers_with_version();
    let repo = repo(true);
    let request = json!({ "env": { MODEL_KEY_ENV: MODEL_KEY }, "model": "sonnet" });

    let answered = report(&preflight(
        repo.path(),
        answers.path(),
        &["--test-model"],
        Some(&request),
    ));
    assert_eq!(
        answered["model_test"],
        json!({ "answered": true, "detail": "ok" })
    );
    let seen: Value = serde_json::from_str(
        &fs::read_to_string(answers.path().join("fake-omp-print-seen")).expect("print was run"),
    )
    .expect("json");
    let args: Vec<&str> = seen["args"]
        .as_array()
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for wanted in [
        "-p",
        "--no-session",
        "--no-tools",
        "--max-cost=0.05",
        "--config",
    ] {
        assert!(args.contains(&wanted), "{wanted} missing from {args:?}");
    }
    let model = args
        .iter()
        .position(|arg| *arg == "--model")
        .expect("--model");
    assert_eq!(args[model + 1], "sonnet");
    assert_eq!(seen["model_key"], MODEL_KEY);
    assert_eq!(
        seen["oauth_env"],
        json!([]),
        "subscription logins never reach omp"
    );
    let cwd = Path::new(seen["cwd"].as_str().expect("cwd"));
    assert!(
        !cwd.starts_with(repo.path()),
        "omp must not run inside the repository: {}",
        cwd.display()
    );

    fs::write(answers.path().join("fake-omp-print-fail"), "").expect("write");
    let refused = report(&preflight(
        repo.path(),
        answers.path(),
        &["--test-model"],
        Some(&request),
    ));
    let test = &refused["model_test"];
    assert_eq!(test["answered"], false);
    let detail = test["detail"].as_str().expect("detail");
    assert!(detail.contains("401"), "{detail}");
    assert!(!detail.contains(MODEL_KEY), "the key leaked: {detail}");

    let bad = report(&preflight(
        repo.path(),
        answers.path(),
        &["--test-model"],
        Some(&json!({ "env": { "CLAUDE_CODE_OAUTH_TOKEN": "x" } })),
    ));
    assert_eq!(bad["model_test"]["answered"], false);
    assert!(
        bad["model_test"]["detail"]
            .as_str()
            .expect("detail")
            .contains("subscription"),
        "{bad}"
    );

    let garbage = Command::new(env!("CARGO_BIN_EXE_ch-runner"))
        .args(["preflight", "--test-model", "--workdir"])
        .arg(repo.path())
        .stdin(Stdio::null())
        .output()
        .expect("run ch-runner");
    assert_eq!(garbage.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&garbage.stderr).contains("not valid JSON"));
}
