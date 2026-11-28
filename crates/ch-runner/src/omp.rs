//! Starting omp, feeding it, forwarding what it writes, and ending it.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ch_protocol::preflight::{MODEL_TEST_MAX_COST, ModelTest};
use ch_protocol::runner::{RUNNER_FRAME_PREFIX, RunnerFrame, StartSession};
use ch_protocol::{
    ANTHROPIC_PROVIDER, LineSplitter, SUBSCRIPTION_TOKEN_ENV, parse_disabled_providers,
    parse_oauth_account_count, validate_approval_mode,
};
use serde_json::Value;

use crate::out::Out;
use crate::scrub::Scrubber;
use crate::tail::cut_head;
use ch_proc as proc_tree;

const STDERR_LINES: usize = 50;
const STDERR_LINE_BYTES: usize = 2048;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// A model's first reply can take a while when the provider is busy.
const MODEL_TEST_TIMEOUT: Duration = Duration::from_secs(90);
const MODEL_TEST_PROMPT: &str = "Reply with the single word ok.";
const MODEL_TEST_DETAIL_CHARS: usize = 300;
const PROBE_OUTPUT_BYTES: u64 = 64 * 1024;
const JOIN_WAIT: Duration = Duration::from_secs(2);
const READ_BUFFER: usize = 8192;

/// How long a stopped omp may take to finish writing after its stdin closes.
pub const DRAIN_ON_STOP: Duration = Duration::from_secs(10);
/// How long the output of an omp that exited by itself may keep arriving.
pub const DRAIN_ON_EXIT: Duration = Duration::from_secs(2);

/// Refuses a `ch_start` the runner could not honour safely, and returns its approval mode.
pub fn check_start(start: &StartSession) -> Result<&str, String> {
    let mut mode = None;
    let mut args = start.omp_args.iter();
    while let Some(arg) = args.next() {
        if arg.contains('\0') {
            return Err("an omp argument holds a NUL byte".to_owned());
        }
        // After `--` the runner's own `--config` would stop being an option.
        if arg == "--" || arg.starts_with("--config") {
            return Err(format!(
                "omp_args may not hold {arg:?}: the runner passes --config itself"
            ));
        }
        if let Some(value) = approval_arg(arg, &mut args)?
            && mode.replace(value).is_some()
        {
            return Err("omp_args may hold --approval-mode only once".to_owned());
        }
    }
    // Without the flag omp runs under its own configured default.
    let mode = mode.ok_or("omp_args must hold --approval-mode")?;
    check_env(&start.env)?;
    Ok(mode)
}

pub fn check_env(env: &BTreeMap<String, String>) -> Result<(), String> {
    for (name, value) in env {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(format!(
                "{name:?} is not a usable environment variable name"
            ));
        }
        if value.contains('\0') {
            return Err(format!("the value of {name} holds a NUL byte"));
        }
        if SUBSCRIPTION_TOKEN_ENV
            .iter()
            .any(|blocked| blocked.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "{name} carries a Claude subscription login, which C-Harness never passes to \
                 omp; use an Anthropic API key instead"
            ));
        }
    }
    Ok(())
}

/// The approval policy may only come as `--approval-mode` with a value omp knows. omp keeps
/// its own default, yolo, when the value is wrong, and the shortcut flags hide the policy.
/// Returns the mode when `arg` sets one.
fn approval_arg<'a>(
    arg: &'a str,
    rest: &mut impl Iterator<Item = &'a String>,
) -> Result<Option<&'a str>, String> {
    let flag = arg.split_once('=').map_or(arg, |(flag, _)| flag);
    if matches!(flag, "--yolo" | "--auto-approve") {
        return Err(format!(
            "omp_args may not hold {flag}: set the approval policy with --approval-mode"
        ));
    }
    if flag != "--approval-mode" {
        return Ok(None);
    }
    let value = match arg.split_once('=') {
        Some((_, value)) => value,
        None => rest.next().ok_or("--approval-mode needs a value")?.as_str(),
    };
    validate_approval_mode(value)?;
    Ok(Some(value))
}

/// What `omp --version` prints, or `None` when it could not run or failed.
pub fn probe_version(
    program: &OsStr,
    workdir: &Path,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    probe(program, &["--version"], workdir, env)
        .filter(|answer| answer.success)
        .map(|answer| answer.stdout)
}

/// omp's arguments: the client's, then each overlay, the host's last so its keys win.
pub fn omp_command_args(
    omp_args: &[String],
    client_overlay: Option<&Path>,
    host_overlay: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = omp_args.iter().map(OsString::from).collect();
    for overlay in client_overlay.into_iter().chain([host_overlay]) {
        args.push("--config".into());
        args.push(overlay.as_os_str().to_owned());
    }
    args
}

/// What omp reports about its providers before the session starts.
pub struct Providers {
    pub user_disabled: Vec<String>,
    /// `None` when omp's answer could not be read.
    pub oauth_accounts: Option<usize>,
}

/// Asks omp, with the session's own environment, which providers the user turned off and
/// whether a Claude subscription login is stored.
pub fn probe_providers(
    program: &OsStr,
    workdir: &Path,
    env: &BTreeMap<String, String>,
) -> Providers {
    // An empty store is an error to omp (exit 1, sentence on stderr), so the status is ignored
    // here; only a recognised answer counts and anything else stays unknown.
    let oauth_accounts = probe(
        program,
        &["token", ANTHROPIC_PROVIDER, "--list"],
        workdir,
        env,
    )
    .and_then(|answer| parse_oauth_account_count(&answer.both()));
    let user_disabled = probe(
        program,
        &["config", "get", "disabledProviders", "--json"],
        workdir,
        env,
    )
    .filter(|answer| answer.success)
    .and_then(|answer| parse_disabled_providers(&answer.stdout))
    .unwrap_or_default();
    Providers {
        user_disabled,
        oauth_accounts,
    }
}

/// Why anthropic is off for this session, when the host turned it off rather than the user.
pub fn provider_notice(user_disabled: &[String], oauth_accounts: Option<usize>) -> Option<String> {
    if user_disabled.iter().any(|id| id == ANTHROPIC_PROVIDER) {
        return None;
    }
    let reason = match oauth_accounts {
        Some(0) => return None,
        Some(_) => "omp has a Claude subscription login stored",
        None => "the runner could not check whether omp has a Claude subscription login stored",
    };
    Some(format!(
        "The anthropic provider is off for this session because {reason}. Anthropic API keys \
         work only when no subscription login is stored; remove it with omp to use one."
    ))
}

struct ProbeAnswer {
    success: bool,
    stdout: String,
    stderr: String,
}

impl ProbeAnswer {
    fn both(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Runs `omp <args>` to completion, or `None` if it could not start, hung, or flooded a pipe.
fn probe(
    program: &OsStr,
    args: &[&str],
    workdir: &Path,
    env: &BTreeMap<String, String>,
) -> Option<ProbeAnswer> {
    run_capped(program, args, workdir, env, PROBE_TIMEOUT).ok()
}

/// Asks the model for one word through omp's print mode, with tools off and omp's spending
/// cap set. The provider bills the key for this.
pub fn test_model(
    program: &OsStr,
    workdir: &Path,
    env: &BTreeMap<String, String>,
    model: Option<&str>,
    host_overlay: &Path,
) -> ModelTest {
    let mut args: Vec<OsString> = vec![
        "-p".into(),
        "--no-session".into(),
        "--no-tools".into(),
        format!("--max-cost={MODEL_TEST_MAX_COST}").into(),
    ];
    if let Some(model) = model {
        args.extend(["--model".into(), model.into()]);
    }
    args.extend([
        "--config".into(),
        host_overlay.as_os_str().to_owned(),
        MODEL_TEST_PROMPT.into(),
    ]);
    let (answered, detail) = match run_capped(program, &args, workdir, env, MODEL_TEST_TIMEOUT) {
        Err(e) => (false, e),
        Ok(answer) if answer.success => match last_line(&answer.stdout) {
            Some(line) => (true, line.to_owned()),
            None => (false, "omp exited without an answer".to_owned()),
        },
        Ok(answer) => {
            let said = last_line(&answer.stderr).or_else(|| last_line(&answer.stdout));
            (
                false,
                said.unwrap_or("omp failed and printed nothing").to_owned(),
            )
        }
    };
    // Scrubbed before it is cut, so a cut never leaves part of a key behind.
    let detail = Scrubber::for_env(env.values()).scrub(&detail);
    ModelTest {
        answered,
        detail: detail.chars().take(MODEL_TEST_DETAIL_CHARS).collect(),
    }
}

fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|line| !line.is_empty())
}

/// Runs `omp <args>` to completion with the session's environment minus subscription logins.
fn run_capped<S: AsRef<OsStr>>(
    program: &OsStr,
    args: &[S],
    workdir: &Path,
    env: &BTreeMap<String, String>,
    limit: Duration,
) -> Result<ProbeAnswer, String> {
    let mut command = proc_tree::command(program);
    proc_tree::own_group(&mut command);
    command.args(args).current_dir(workdir).envs(env);
    for name in SUBSCRIPTION_TOKEN_ENV {
        command.env_remove(name);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("omp could not start: {e}"))?;
    let unreadable = || "omp's output could not be read".to_owned();
    let stdout = read_capped(child.stdout.take().ok_or_else(unreadable)?);
    let stderr = read_capped(child.stderr.take().ok_or_else(unreadable)?);
    let Ok(Some(status)) = proc_tree::wait_for(&mut child, limit) else {
        let _ = proc_tree::kill_tree(&mut child);
        return Err(format!(
            "omp did not finish within {} seconds",
            limit.as_secs()
        ));
    };
    let read = |pipe| {
        proc_tree::join_within(pipe, JOIN_WAIT)
            .and_then(Result::ok)
            .ok_or_else(unreadable)
    };
    let stdout = read(stdout)?;
    let stderr = read(stderr)?;
    Ok(ProbeAnswer {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn read_capped(pipe: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut text = Vec::new();
        pipe.take(PROBE_OUTPUT_BYTES)
            .read_to_end(&mut text)
            .map(|_| text)
    })
}

/// How to start omp.
pub struct Launch<'a> {
    pub program: &'a OsStr,
    pub args: Vec<OsString>,
    pub workdir: &'a Path,
    pub env: &'a BTreeMap<String, String>,
    /// Files omp was given that go away with it.
    pub overlays: Vec<PathBuf>,
}

pub struct Omp {
    child: Child,
    stdin: Option<Sender<String>>,
    stdout: JoinHandle<()>,
    others: Vec<JoinHandle<()>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    scrubber: Scrubber,
    overlays: Vec<PathBuf>,
}

/// How omp ended.
pub struct Finished {
    pub exit_code: Option<i32>,
    pub stderr_tail: Vec<String>,
    /// Set when omp, or a process it started, could not be ended, so the working tree may
    /// still change. `exit_code` can be set as well when omp itself was reaped.
    pub problem: Option<String>,
}

impl Omp {
    pub fn spawn(launch: Launch<'_>, out: &Out) -> Result<Self, String> {
        let mut command = proc_tree::command(launch.program);
        proc_tree::own_group(&mut command);
        command
            .args(&launch.args)
            .current_dir(launch.workdir)
            .envs(launch.env);
        // After the session env, so neither it nor the runner's own env can supply one.
        for name in SUBSCRIPTION_TOKEN_ENV {
            command.env_remove(name);
        }
        let spawned = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                remove_all(&launch.overlays);
                let program = launch.program.to_string_lossy();
                let hint = if e.kind() == std::io::ErrorKind::NotFound {
                    ". Install omp and make sure `omp` is on the PATH that ch-runner sees"
                } else {
                    ""
                };
                return Err(format!("could not start {program}: {e}{hint}"));
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = proc_tree::kill_tree(&mut child);
            remove_all(&launch.overlays);
            return Err("omp started without its pipes".to_owned());
        };

        out.open_omp();
        let (sender, receiver) = mpsc::channel();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LINES)));
        let others = vec![
            feed(stdin, receiver),
            keep_stderr(stderr, Arc::clone(&stderr_tail)),
        ];
        Ok(Self {
            child,
            stdin: Some(sender),
            stdout: forward(stdout, out.clone()),
            others,
            stderr_tail,
            scrubber: Scrubber::for_env(launch.env.values()),
            overlays: launch.overlays,
        })
    }

    /// Queues a line for omp's stdin. False when omp no longer reads it.
    pub fn send(&self, line: String) -> bool {
        self.stdin
            .as_ref()
            .is_some_and(|stdin| stdin.send(line).is_ok())
    }

    /// The exit status, once omp has exited.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Closes omp's stdin, lets its output drain for up to `drain`, then ends its whole group.
    pub fn finish(mut self, drain: Duration) -> Finished {
        drop(self.stdin.take());
        proc_tree::finished_within(&self.stdout, drain);
        let killed = proc_tree::kill_tree(&mut self.child);
        let (status, problem) = kill_outcome(killed, || self.child.try_wait().ok().flatten());
        proc_tree::join_within(self.stdout, JOIN_WAIT);
        for handle in self.others {
            proc_tree::join_within(handle, JOIN_WAIT);
        }
        remove_all(&self.overlays);
        let stderr_tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|line| self.scrubber.scrub(line))
            .collect();
        Finished {
            exit_code: status.and_then(|status| status.code()),
            stderr_tail,
            problem,
        }
    }
}

/// omp's exit status and what is left running after [`proc_tree::kill_tree`].
///
/// When the tree could not be signalled, `kill_tree` has already killed and waited for omp
/// itself, so `reaped` only reads the result. A reaped omp still leaves a problem, because
/// the processes it started may live on.
fn kill_outcome(
    killed: io::Result<Option<ExitStatus>>,
    reaped: impl FnOnce() -> Option<ExitStatus>,
) -> (Option<ExitStatus>, Option<String>) {
    match killed {
        Ok(Some(status)) => (Some(status), None),
        Ok(None) => (
            None,
            Some("omp did not exit after it was killed".to_owned()),
        ),
        Err(e) => match reaped() {
            Some(status) => (
                Some(status),
                Some(format!(
                    "omp exited, but the processes it started may still be running: {e}"
                )),
            ),
            None => (None, Some(format!("could not end omp: {e}"))),
        },
    }
}

fn remove_all(paths: &[PathBuf]) {
    for path in paths {
        // The files sit in the session's temp dir, which goes away with the session anyway.
        let _ = fs::remove_file(path);
    }
}

/// Writes queued lines to omp's stdin on a thread of its own, so a full pipe never stalls
/// the runner. Closes stdin once the sender is dropped.
fn feed(mut stdin: ChildStdin, lines: Receiver<String>) -> JoinHandle<()> {
    thread::spawn(move || {
        for mut line in lines {
            line.push('\n');
            if stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush())
                .is_err()
            {
                return;
            }
        }
    })
}

/// Reads a pipe and hands each complete line, or `None` for one too long to keep, to `each`.
fn read_lines(mut pipe: impl Read, mut each: impl FnMut(Option<String>)) {
    let mut splitter = LineSplitter::new();
    let mut buf = vec![0_u8; READ_BUFFER];
    loop {
        let lines = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => splitter.push(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for line in lines {
            each(line.ok());
        }
    }
    // The last line may lack its newline.
    for line in splitter.push(b"\n") {
        each(line.ok());
    }
}

fn forward(stdout: impl Read + Send + 'static, out: Out) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut spoof_reported = false;
        let mut long_reported = false;
        read_lines(stdout, |line| match line {
            Some(line) if !is_runner_frame(&line) => out.omp_line(&line),
            Some(_) => {
                if !spoof_reported {
                    spoof_reported = true;
                    out.frame(&RunnerFrame::Notice {
                        kind: "omp_frame_dropped".into(),
                        message: "omp wrote a line that looked like a runner frame; it was \
                                  dropped, along with any later ones"
                            .into(),
                    });
                }
            }
            None => {
                if !long_reported {
                    long_reported = true;
                    out.frame(&RunnerFrame::Notice {
                        kind: "omp_line_dropped".into(),
                        message: "omp wrote a line too long to forward; it was dropped".into(),
                    });
                }
            }
        });
    })
}

/// Whether a line carries the runner's own frame prefix, which omp must not be able to forge.
fn is_runner_frame(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with(RUNNER_FRAME_PREFIX))
    })
}

fn keep_stderr(
    stderr: impl Read + Send + 'static,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        read_lines(stderr, |line| {
            let line = line.map_or_else(
                || "(a line too long to keep)".to_owned(),
                |line| cut_head(line.as_bytes(), STDERR_LINE_BYTES).0,
            );
            let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
            if tail.len() == STDERR_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(omp_args: &[&str], env: &[(&str, &str)]) -> StartSession {
        StartSession {
            session: "s1".into(),
            omp_args: omp_args.iter().map(|s| (*s).to_owned()).collect(),
            config_overlay: None,
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    const WRITE: [&str; 2] = ["--approval-mode", "write"];

    #[test]
    fn accepts_a_normal_start() {
        let ok = start(
            &["--mode", "rpc", "--approval-mode", "write"],
            &[("ANTHROPIC_API_KEY", "test-model-key-not-real")],
        );
        assert_eq!(check_start(&ok), Ok("write"));
    }

    #[test]
    fn a_start_names_its_approval_mode_once() {
        // Without the flag omp would run under its own default, which is yolo.
        for args in [&[][..], &["--mode", "rpc"]] {
            let error = check_start(&start(args, &[])).unwrap_err();
            assert!(
                error.contains("must hold --approval-mode"),
                "{args:?}: {error}"
            );
        }
        let twice = ["--approval-mode", "write", "--approval-mode=yolo"];
        let error = check_start(&start(&twice, &[])).unwrap_err();
        assert!(error.contains("only once"), "{error}");
    }

    #[test]
    fn refuses_args_that_would_unseat_the_runners_config() {
        for arg in ["--", "--config", "--config=x.yml"] {
            assert!(check_start(&start(&[arg], &[])).is_err(), "{arg}");
        }
        assert!(check_start(&start(&["a\0b"], &[])).is_err());
    }

    #[test]
    fn approval_policy_comes_only_as_a_known_approval_mode() {
        for (args, mode) in [
            (&["--approval-mode", "write"][..], "write"),
            (&["--approval-mode=always-ask"], "always-ask"),
            (
                &["--approval-mode", "always-ask-cached"],
                "always-ask-cached",
            ),
            (&["--approval-mode", "yolo"], "yolo"),
        ] {
            assert_eq!(check_start(&start(args, &[])), Ok(mode), "{args:?}");
        }
        // omp takes a bad value without a word and keeps its own default, which is yolo.
        for args in [
            &["--approval-mode", "Write"][..],
            &["--approval-mode=never"],
            &["--approval-mode"],
            &["--yolo"],
            &["--auto-approve"],
            &["--yolo=true"],
        ] {
            let error = check_start(&start(args, &[])).unwrap_err();
            assert!(error.contains("approval"), "{args:?}: {error}");
        }
    }

    #[test]
    fn refuses_subscription_tokens_in_any_case_without_echoing_them() {
        for name in [
            "ANTHROPIC_OAUTH_TOKEN",
            "claude_code_oauth_token",
            "Anthropic_OAuth_Token",
        ] {
            let error = check_start(&start(&WRITE, &[(name, "test-oauth-not-real")])).unwrap_err();
            assert!(error.contains("subscription login"), "{error}");
            assert!(!error.contains("test-oauth-not-real"), "{error}");
        }
    }

    #[test]
    fn refuses_unusable_environment_entries() {
        for (env, why) in [
            (("", "v"), "not a usable environment variable name"),
            (("A=B", "v"), "not a usable environment variable name"),
            (("A", "v\0w"), "holds a NUL byte"),
        ] {
            let error = check_start(&start(&WRITE, &[env])).unwrap_err();
            assert!(error.contains(why), "{env:?}: {error}");
        }
    }

    #[test]
    fn puts_the_host_overlay_last() {
        let args = omp_command_args(
            &["--mode".into(), "rpc".into()],
            Some(Path::new("client.yml")),
            Path::new("host.yml"),
        );
        let args: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--mode",
                "rpc",
                "--config",
                "client.yml",
                "--config",
                "host.yml"
            ]
        );
        let args = omp_command_args(&[], None, Path::new("host.yml"));
        assert_eq!(args, [OsString::from("--config"), "host.yml".into()]);
    }

    #[test]
    fn explains_why_anthropic_is_off() {
        let stored = provider_notice(&[], Some(1)).unwrap();
        assert!(stored.contains("subscription login stored"), "{stored}");
        let unknown = provider_notice(&[], None).unwrap();
        assert!(unknown.contains("could not check"), "{unknown}");
        assert_eq!(provider_notice(&[], Some(0)), None);
        assert_eq!(provider_notice(&["anthropic".into()], Some(2)), None);
    }

    #[test]
    fn spots_forged_runner_frames() {
        assert!(is_runner_frame(r#"{"type":"ch_landed","id":"x"}"#));
        assert!(!is_runner_frame(r#"{"type":"agent_start"}"#));
        assert!(!is_runner_frame("ch_landed"));
        assert!(!is_runner_frame(r#"{"kind":"ch_landed"}"#));
    }

    #[test]
    fn a_tree_that_could_not_be_signalled_stays_a_problem() {
        let status = proc_tree::command("git")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .unwrap();
        let no_kill = || io::Error::other("no kill");

        let (reaped, problem) = kill_outcome(Err(no_kill()), || Some(status));
        assert_eq!(reaped, Some(status));
        let problem = problem.expect("a reaped omp may still leave its children running");
        assert!(
            problem.contains("may still be running: no kill"),
            "{problem}"
        );

        let (reaped, problem) = kill_outcome(Err(no_kill()), || None);
        assert_eq!(reaped, None);
        assert_eq!(problem.as_deref(), Some("could not end omp: no kill"));

        assert_eq!(
            kill_outcome(Ok(Some(status)), || None),
            (Some(status), None)
        );
        let (_, problem) = kill_outcome(Ok(None), || Some(status));
        assert_eq!(
            problem.as_deref(),
            Some("omp did not exit after it was killed")
        );
    }
}
