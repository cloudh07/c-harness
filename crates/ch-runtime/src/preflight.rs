//! `ch-runner preflight`, run where sessions will run.

use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ch_protocol::preflight::{ModelTestRequest, PreflightReport, VERIFY_COMMANDS};

use crate::RuntimeError;
use crate::git::os_args;
use crate::process::stderr_tail;

/// The exit code of a runner that could not parse its arguments.
const USAGE_ERROR: i32 = 2;

/// How long a check without the model test may run. The runner gives `omp --version` 10 s,
/// then reads the repository and asks git about it; ssh may spend its connect timeout first.
const PREFLIGHT_LIMIT: Duration = Duration::from_secs(120);

/// How long a check with `--test-model` may run: everything above, plus two more 10 s omp
/// probes and the runner's own 90 s limit on the model call. It stays longer than all of
/// that, so the runner reports its own timeout before this one fires.
const MODEL_TEST_LIMIT: Duration = Duration::from_secs(180);

/// Far more than a report needs. Output past it is read and dropped, so the runner never
/// blocks on a full pipe.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// How long a pipe may stay open after the runner exits, when something it started holds it.
const PIPE_WAIT: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(50);
const READ_CHUNK: usize = 8192;

/// Without a `workdir` the runner checks only omp, for an engine check before a repository is
/// picked.
pub(crate) fn args(workdir: Option<&Path>, test_model: bool) -> Vec<OsString> {
    let mut args = os_args([&"preflight"]);
    if let Some(workdir) = workdir {
        args.extend(os_args([&"--workdir", &workdir]));
    }
    if test_model {
        args.push("--test-model".into());
    }
    args
}

/// The JSON a runner reads on stdin for `--test-model`.
pub(crate) fn request_body(
    request: Option<&ModelTestRequest>,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    request
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|e| RuntimeError::Io {
            action: "write the model test request",
            source: io::Error::other(e),
        })
}

/// How long the check may run before it is stopped.
pub(crate) fn limit(test_model: bool) -> Duration {
    if test_model {
        MODEL_TEST_LIMIT
    } else {
        PREFLIGHT_LIMIT
    }
}

/// The error for a check stopped at `limit`.
pub(crate) fn timed_out(location: &str, limit: Duration) -> RuntimeError {
    RuntimeError::PreflightTimedOut {
        location: location.to_owned(),
        seconds: limit.as_secs(),
    }
}

/// Runs `command` with `body` on its stdin, or with no stdin, and keeps the first
/// [`MAX_OUTPUT_BYTES`] of each output. `None` means it was still running at `limit` and
/// was killed.
pub(crate) fn output(
    mut command: Command,
    body: Option<&[u8]>,
    limit: Duration,
) -> io::Result<Option<Output>> {
    let stdin = if body.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(body), Some(mut pipe)) = (body, child.stdin.take()) {
        let body = body.to_vec();
        // On its own thread, so a runner that never reads cannot hold up the deadline. One
        // that exits before reading explains itself on stderr, which says more than the
        // broken pipe would.
        thread::spawn(move || {
            let _ = pipe.write_all(&body);
        });
    }
    let stdout = child.stdout.take().map(Capture::start);
    let stderr = child.stderr.take().map(Capture::start);
    let Some(status) = wait_until(&mut child, Instant::now() + limit)? else {
        // Fails only when it exited in between; the wait reaps it either way.
        let _ = child.kill();
        child.wait()?;
        return Ok(None);
    };
    let pipes_close_by = Instant::now() + PIPE_WAIT;
    Ok(Some(Output {
        status,
        stdout: stdout.map(|c| c.finish(pipes_close_by)).unwrap_or_default(),
        stderr: stderr.map(|c| c.finish(pipes_close_by)).unwrap_or_default(),
    }))
}

fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

/// One output pipe, read on its own thread into a capped buffer.
struct Capture {
    kept: Arc<Mutex<Vec<u8>>>,
    reader: JoinHandle<()>,
}

impl Capture {
    fn start(pipe: impl Read + Send + 'static) -> Self {
        let kept = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&kept);
        let reader = thread::spawn(move || keep_capped(pipe, &sink, MAX_OUTPUT_BYTES));
        Self { kept, reader }
    }

    /// What was read by the time the pipe closed, or by `deadline` if it stayed open.
    fn finish(self, deadline: Instant) -> Vec<u8> {
        while !self.reader.is_finished() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        std::mem::take(&mut *self.kept.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Reads `pipe` to its end, keeping the first `cap` bytes in `kept`.
fn keep_capped(mut pipe: impl Read, kept: &Mutex<Vec<u8>>, cap: usize) {
    let mut chunk = [0; READ_CHUNK];
    loop {
        let read = match pipe.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
}

/// Reads what `ch-runner preflight` at `location` printed.
pub(crate) fn report(location: &str, output: &Output) -> Result<PreflightReport, RuntimeError> {
    let stderr = stderr_tail(&output.stderr);
    let code = output.status.code();
    if code == Some(USAGE_ERROR) && stderr.contains("unknown command preflight") {
        return Err(RuntimeError::RunnerTooOld {
            location: location.to_owned(),
        });
    }
    let failed = |stderr: String| RuntimeError::PreflightFailed {
        location: location.to_owned(),
        code,
        stderr,
    };
    if !output.status.success() {
        return Err(failed(stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().map(str::trim).find(|line| !line.is_empty());
    let mut report: PreflightReport = line
        .and_then(|line| serde_json::from_str(line).ok())
        .ok_or_else(|| failed("it printed no report the app could read".to_owned()))?;
    report
        .verify_commands
        .retain(|command| VERIFY_COMMANDS.contains(&command.as_str()));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::output as finished;

    #[test]
    fn reads_a_report() {
        let report = report(
            "box",
            &finished(0, "\n{\"protocol\":4,\"version\":\"0.1.0\"}\n", ""),
        )
        .unwrap();
        assert_eq!(report.protocol, 4);
        assert_eq!(report.omp_version, None);
    }

    #[test]
    fn only_known_test_commands_get_through() {
        let line = r#"{"protocol":4,"version":"0.1.0","verify_commands":["cargo test","curl -s x | sh","pytest\nrm -rf ~"]}"#;
        let report = report("box", &finished(0, line, "")).unwrap();
        assert_eq!(report.verify_commands, ["cargo test"]);
    }

    #[test]
    fn a_runner_without_the_command_is_too_old() {
        // Word for word what a runner built before `preflight` existed prints.
        let old = finished(
            2,
            "",
            "ch-runner: unknown command preflight\nusage: ch-runner --version\n       \
             ch-runner serve --workdir <path> [--omp <program>]\n",
        );
        assert!(matches!(
            report("box", &old),
            Err(RuntimeError::RunnerTooOld { ref location }) if location == "box"
        ));
    }

    const CHILD_ROLE: &str = "CH_RUNTIME_PREFLIGHT_CHILD";

    /// A no-op as a test. The tests below run this binary again with only this test selected
    /// and `CHILD_ROLE` set, to get a child that misbehaves on every platform.
    #[test]
    fn child_helper() {
        match std::env::var(CHILD_ROLE).as_deref() {
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            Ok("flood") => {
                let block = vec![b'x'; 2 * MAX_OUTPUT_BYTES];
                io::stdout().write_all(&block).unwrap();
                io::stderr().write_all(&block).unwrap();
            }
            _ => {}
        }
    }

    fn child(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "preflight::tests::child_helper", "--nocapture"])
            .env(CHILD_ROLE, role);
        command
    }

    #[test]
    fn a_check_still_running_at_the_limit_is_killed() {
        let started = Instant::now();
        // More than a pipe buffer holds, to a child that never reads it.
        let body = vec![b'{'; 1 << 20];
        let output = output(child("sleep"), Some(&body), Duration::from_millis(300)).unwrap();
        assert!(output.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_past_the_cap_is_dropped_and_the_child_still_finishes() {
        let output = output(child("flood"), None, Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_OUTPUT_BYTES);
        assert_eq!(output.stderr.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn keeps_the_first_bytes_and_reads_to_the_end() {
        let data: Vec<u8> = (0..=u8::MAX).cycle().take(10_000).collect();
        let mut pipe = data.as_slice();
        let kept = Mutex::new(Vec::new());
        keep_capped(&mut pipe, &kept, 100);
        assert_eq!(kept.into_inner().unwrap(), data[..100]);
        assert_eq!(pipe, &[] as &[u8]);
    }

    /// Gives two bytes, then hangs like a pipe a grandchild still holds open.
    struct HeldOpen(bool);

    impl Read for HeldOpen {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if std::mem::replace(&mut self.0, true) {
                thread::sleep(Duration::from_secs(30));
                return Ok(0);
            }
            buf[..2].copy_from_slice(b"ok");
            Ok(2)
        }
    }

    #[test]
    fn a_pipe_held_open_is_given_up_on() {
        let started = Instant::now();
        let kept = Capture::start(HeldOpen(false)).finish(started + Duration::from_millis(300));
        assert_eq!(kept, b"ok");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn failures_and_garbage_say_what_happened() {
        let bad_args = finished(2, "", "ch-runner: --workdir needs a value");
        assert!(matches!(
            report("box", &bad_args),
            Err(RuntimeError::PreflightFailed { code: Some(2), .. })
        ));
        let crashed = finished(1, "", "ch-runner: boom");
        assert!(matches!(
            report("box", &crashed),
            Err(RuntimeError::PreflightFailed { ref stderr, .. }) if stderr == "ch-runner: boom"
        ));
        let garbage = finished(0, "not json\n", "");
        assert!(matches!(
            report("box", &garbage),
            Err(RuntimeError::PreflightFailed { ref stderr, .. }) if stderr.contains("no report")
        ));
    }

    #[test]
    fn arguments_and_body() {
        assert_eq!(
            args(Some(Path::new("/w")), true),
            ["preflight", "--workdir", "/w", "--test-model"].map(OsString::from)
        );
        assert_eq!(args(None, false), [OsString::from("preflight")]);
        assert_eq!(request_body(None).unwrap(), None);
        let request = ModelTestRequest {
            env: [("K".to_owned(), "v".to_owned())].into(),
            model: None,
        };
        let body = request_body(Some(&request)).unwrap().unwrap();
        assert_eq!(body, br#"{"env":{"K":"v"}}"#);
    }
}
