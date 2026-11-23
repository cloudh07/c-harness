//! `ch-runner preflight`, run where sessions will run.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use ch_protocol::preflight::{ModelTestRequest, PreflightReport, VERIFY_COMMANDS};

use crate::RuntimeError;
use crate::git::os_args;
use crate::process::stderr_tail;

/// The exit code of a runner that could not parse its arguments.
const USAGE_ERROR: i32 = 2;

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

/// Runs `command` to the end with `body` on its stdin, or with no stdin.
pub(crate) fn output(mut command: Command, body: Option<&[u8]>) -> io::Result<Output> {
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
        // A runner that exits before reading explains itself on stderr, which says more than
        // the broken pipe would.
        let _ = pipe.write_all(body);
    }
    child.wait_with_output()
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
        let old = finished(
            2,
            "",
            "ch-runner: unknown command preflight\nusage: ch-runner --version",
        );
        assert!(matches!(
            report("box", &old),
            Err(RuntimeError::RunnerTooOld { ref location }) if location == "box"
        ));
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
