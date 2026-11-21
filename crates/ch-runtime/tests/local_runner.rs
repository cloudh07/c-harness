//! `LocalProvider` against a real child process. This binary is also the stand-in `ch-runner`:
//! run with `--version` or `serve` it behaves like the runner, otherwise it runs the checks.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ch_runtime::{LocalProvider, SandboxProvider};

const STUB_VERSION: &str = "0.0.0-stub";
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("ch-runner {STUB_VERSION}");
            ExitCode::SUCCESS
        }
        Some("serve") => stub_serve(&args),
        _ => run_checks(),
    }
}

fn stub_serve(args: &[String]) -> ExitCode {
    let mut out = std::io::stdout().lock();
    let workdir = match args {
        [_, flag, dir] if flag == "--workdir" => dir,
        _ => return ExitCode::from(2),
    };
    let cwd = std::env::current_dir().expect("current dir");
    writeln!(out, "workdir={workdir}").unwrap();
    writeln!(out, "cwd={}", cwd.display()).unwrap();
    out.flush().unwrap();
    for line in std::io::stdin().lines() {
        writeln!(out, "echo={}", line.unwrap()).unwrap();
        out.flush().unwrap();
    }
    ExitCode::SUCCESS
}

fn run_checks() -> ExitCode {
    let checks: [(&str, fn()); 4] = [
        ("check_reads_the_stub_version", check_reads_the_stub_version),
        (
            "start_runs_in_a_fresh_worktree",
            start_runs_in_a_fresh_worktree,
        ),
        (
            "dropping_the_handle_stops_the_runner",
            dropping_the_handle_stops_the_runner,
        ),
        (
            "kill_then_wait_reports_failure",
            kill_then_wait_reports_failure,
        ),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        let passed = panic::catch_unwind(AssertUnwindSafe(check)).is_ok();
        println!("test {name} ... {}", if passed { "ok" } else { "FAILED" });
        failed += usize::from(!passed);
    }
    println!(
        "test result: {} passed; {failed} failed",
        checks.len() - failed
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    provider: LocalProvider,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "--no-verify", "-m", "init"],
    );
    let provider = LocalProvider {
        repo,
        runner: std::env::current_exe().unwrap(),
        worktrees_dir: root.path().join("worktrees"),
    };
    Fixture {
        _root: root,
        provider,
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn same_dir(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
}

fn read_line(reader: &mut impl BufRead) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    line.trim_end().to_owned()
}

fn check_reads_the_stub_version() {
    let fixture = fixture();
    assert_eq!(fixture.provider.check().unwrap().version, STUB_VERSION);
}

fn start_runs_in_a_fresh_worktree() {
    let fixture = fixture();
    let provider = &fixture.provider;
    let worktree = provider.worktree_path("s1").unwrap();

    let mut runner = provider.start("s1").unwrap();
    let mut stdin = runner.take_stdin().unwrap();
    let mut stdout = BufReader::new(runner.take_stdout().unwrap());
    let workdir = PathBuf::from(read_line(&mut stdout).strip_prefix("workdir=").unwrap());
    let cwd = PathBuf::from(read_line(&mut stdout).strip_prefix("cwd=").unwrap());
    assert!(same_dir(&workdir, &worktree));
    assert!(same_dir(&cwd, &worktree));
    assert!(worktree.join(".git").is_file());

    writeln!(stdin, "hello").unwrap();
    assert_eq!(read_line(&mut stdout), "echo=hello");
    drop(stdin);
    assert!(runner.wait().unwrap().success());

    drop(runner);
    provider.remove_worktree("s1").unwrap();
    assert!(!worktree.exists());
}

fn dropping_the_handle_stops_the_runner() {
    let fixture = fixture();
    let mut runner = fixture.provider.start("s2").unwrap();
    // Holding stdin open means the stub would run forever if nothing killed it.
    let _stdin = runner.take_stdin().unwrap();
    let mut stdout = runner.take_stdout().unwrap();
    drop(runner);

    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut rest = Vec::new();
        let _ = stdout.read_to_end(&mut rest);
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(EXIT_TIMEOUT).is_ok(),
        "runner still running after its handle was dropped"
    );
    fixture.provider.remove_worktree("s2").unwrap();
}

fn kill_then_wait_reports_failure() {
    let fixture = fixture();
    let mut runner = fixture.provider.start("s3").unwrap();
    let _stdin = runner.take_stdin().unwrap();
    runner.kill().unwrap();
    assert!(!runner.wait().unwrap().success());
    drop(runner);
    fixture.provider.remove_worktree("s3").unwrap();
}
