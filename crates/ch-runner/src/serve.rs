//! The runner's main loop: one client on stdin and stdout, and at most one omp.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ch_protocol::runner::{
    ClientFrame, ClientLine, LandRequest, MAX_OUTPUT_TAIL_BYTES, MAX_PATCH_BYTES,
    RUNNER_PROTOCOL_VERSION, RouteError, RunnerFrame, StartSession, route_client_line,
};
use ch_protocol::{LineSplitter, MAX_FRAME_BYTES, host_config_overlay, host_disabled_providers};
use serde_json::Value;

use crate::git::{Base, Git};
use crate::land::Lander;
use crate::omp::{self, Launch, Omp};
use crate::out::Out;
use crate::pr::PrOpener;
use crate::proc_tree;
use crate::scratch::PrivateDir;
use crate::scrub::Scrubber;
use crate::verify::{self, Job};

// The `ch_error` stages the client tells apart; `verify` and `diff` failures leave the
// session usable, every other stage ends it.
const START: &str = "start";
const VERIFY: &str = "verify";
const DIFF: &str = "diff";
const STOP: &str = "stop";
const LAND: &str = "land";
const PROTOCOL: &str = "protocol";

/// Directories through which a repository can configure omp or another agent's tools.
const PROJECT_CONFIG_DIRS: [&str; 4] = [".omp", ".claude", ".codex", ".gemini"];

/// How often the loop looks at omp and the workers while the client is quiet.
const TICK: Duration = Duration::from_millis(100);
const INPUT_QUEUE: usize = 64;
const WORKER_JOIN: Duration = Duration::from_secs(5);
const READ_BUFFER: usize = 8192;

pub struct Options {
    pub workdir: PathBuf,
    pub omp_program: OsString,
    pub pr_opener: Box<dyn PrOpener>,
}

/// Serves one client until its input ends or it stops reading.
///
/// Errors come only from setting up, before the hello frame; everything after is reported
/// to the client as frames.
pub fn serve<R, W>(input: R, output: W, options: Options) -> io::Result<()>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let workdir = std::path::absolute(&options.workdir)?;
    if !workdir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", workdir.display()),
        ));
    }
    let dir = PrivateDir::create("ch-runner")?;
    dir.ensure_outside(&workdir)?;
    let no_hooks = dir.path().join("no-hooks");
    fs::create_dir(&no_hooks)?;

    let out = Out::new(output);
    out.frame(&RunnerFrame::Hello {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: RUNNER_PROTOCOL_VERSION,
        workdir: workdir.to_string_lossy().into_owned(),
    });
    if let Some(notice) = project_config_notice(&workdir) {
        out.frame(&notice);
    }

    let (sender, receiver) = mpsc::sync_channel(INPUT_QUEUE);
    read_client(input, sender);
    let mut session = Session {
        out,
        git: Git::new(&workdir, no_hooks),
        dir,
        omp_program: options.omp_program,
        opener: options.pr_opener,
        base: None,
        omp: OmpState::NotStarted,
        landed: HashMap::new(),
        workers: Vec::new(),
        shutdown: Arc::new(AtomicBool::new(false)),
    };
    session.run(&receiver);
    session.close();
    Ok(())
}

fn project_config_notice(workdir: &Path) -> Option<RunnerFrame> {
    let found: Vec<&str> = PROJECT_CONFIG_DIRS
        .into_iter()
        .filter(|name| fs::symlink_metadata(workdir.join(name)).is_ok())
        .collect();
    if found.is_empty() {
        return None;
    }
    Some(RunnerFrame::Notice {
        kind: "project_omp_config".into(),
        message: format!(
            "This repository carries {}. omp can load project config from these: it can change \
             which actions ask for your approval and load extensions that run code inside omp.",
            found.join(", ")
        ),
    })
}

enum Input {
    Line(String),
    TooLong,
    End,
}

/// Reads the client on a thread of its own, so the loop can watch omp between lines.
fn read_client<R: Read + Send + 'static>(mut input: R, lines: SyncSender<Input>) {
    let send_all = move |lines: &SyncSender<Input>, batch: Vec<Result<String, _>>| {
        batch.into_iter().all(|line| {
            let input = line.map_or(Input::TooLong, Input::Line);
            lines.send(input).is_ok()
        })
    };
    thread::spawn(move || {
        let mut splitter = LineSplitter::new();
        let mut buf = vec![0_u8; READ_BUFFER];
        loop {
            let batch = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => splitter.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if !send_all(&lines, batch) {
                return;
            }
        }
        // The last line may lack its newline.
        if send_all(&lines, splitter.push(b"\n")) {
            let _ = lines.send(Input::End);
        }
    });
}

enum OmpState {
    NotStarted,
    Running(Omp),
    Ended {
        exit_code: Option<i32>,
    },
    /// omp could not be ended, so whether it still runs is unknown.
    Lost,
}

struct Session {
    out: Out,
    git: Git,
    dir: PrivateDir,
    omp_program: OsString,
    opener: Box<dyn PrOpener>,
    /// Set by the `ch_start` that started omp.
    base: Option<Base>,
    omp: OmpState,
    landed: HashMap<String, String>,
    workers: Vec<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

fn error(id: Option<String>, stage: &str, message: impl Into<String>) -> RunnerFrame {
    RunnerFrame::Error {
        id,
        stage: stage.to_owned(),
        message: message.into(),
    }
}

impl Session {
    fn run(&mut self, input: &Receiver<Input>) {
        loop {
            match input.recv_timeout(TICK) {
                Ok(Input::Line(line)) => self.handle_line(&line),
                Ok(Input::TooLong) => self.out.frame(&error(
                    None,
                    PROTOCOL,
                    format!("a line longer than {MAX_FRAME_BYTES} bytes was dropped"),
                )),
                Ok(Input::End) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.watch_omp();
            self.workers.retain(|worker| !worker.is_finished());
            if self.out.is_broken() {
                return;
            }
        }
    }

    /// Ends omp and the workers once the client is gone.
    fn close(mut self) {
        if let OmpState::Running(omp) = std::mem::replace(&mut self.omp, OmpState::Lost) {
            omp.finish(omp::DRAIN_ON_STOP);
        }
        self.out.close_omp();
        self.shutdown.store(true, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            proc_tree::join_within(worker, WORKER_JOIN);
        }
    }

    fn handle_line(&mut self, line: &str) {
        match route_client_line(line) {
            Ok(ClientLine::Runner(frame)) => self.handle_frame(frame),
            Ok(ClientLine::Omp(line)) => self.to_omp(line),
            Err(e) => self.out.frame(&bad_line(line, &e)),
        }
    }

    fn handle_frame(&mut self, frame: ClientFrame) {
        match frame {
            ClientFrame::Start(start) => self.start(&start),
            ClientFrame::Verify {
                id,
                command,
                timeout_secs,
            } => self.verify(id, command, timeout_secs),
            ClientFrame::Diff { id } => self.diff(id),
            ClientFrame::Stop { id } => self.stop(id),
            ClientFrame::Land(request) => self.land(&request),
        }
    }

    fn to_omp(&self, line: String) {
        let sent = match &self.omp {
            OmpState::Running(omp) => omp.send(line),
            _ => false,
        };
        if !sent {
            self.out.frame(&error(
                None,
                PROTOCOL,
                "omp is not running, so a line for it was dropped",
            ));
        }
    }

    fn start(&mut self, start: &StartSession) {
        if !matches!(self.omp, OmpState::NotStarted) {
            self.out.frame(&error(
                None,
                START,
                "omp was already started on this connection",
            ));
            return;
        }
        if let Err(e) = self.git.pin() {
            self.out.frame(&error(None, START, e));
            return;
        }
        match self.start_omp(start) {
            Ok((omp, base)) => {
                self.omp = OmpState::Running(omp);
                self.base = Some(base);
            }
            Err(e) => {
                let scrubber = Scrubber::for_env(start.env.values());
                self.out.frame(&error(None, START, scrubber.scrub(&e)));
            }
        }
    }

    fn start_omp(&self, start: &StartSession) -> Result<(Omp, Base), String> {
        omp::check_start(start)?;
        let base = self.git.base()?;
        let write = |stem: &str, text: &str| {
            self.dir
                .write_file(stem, text.as_bytes())
                .map_err(|e| format!("could not write omp's config overlay: {e}"))
        };
        let client = start
            .config_overlay
            .as_deref()
            .map(|text| write("client-config.yml", text))
            .transpose()?;
        let providers = omp::probe_providers(&self.omp_program, self.git.workdir(), &start.env);
        let disabled = host_disabled_providers(&providers.user_disabled, providers.oauth_accounts);
        let host = match write("host-config.yml", &host_config_overlay(&disabled)) {
            Ok(host) => host,
            Err(e) => {
                if let Some(client) = &client {
                    let _ = fs::remove_file(client);
                }
                return Err(e);
            }
        };
        if let Some(message) =
            omp::provider_notice(&providers.user_disabled, providers.oauth_accounts)
        {
            self.out.frame(&RunnerFrame::Notice {
                kind: "provider_disabled".into(),
                message,
            });
        }
        let launch = Launch {
            program: &self.omp_program,
            args: omp::omp_command_args(&start.omp_args, client.as_deref(), &host),
            workdir: self.git.workdir(),
            env: &start.env,
            overlays: client.into_iter().chain([host]).collect(),
        };
        let omp = Omp::spawn(launch, &self.out)?;
        Ok((omp, base))
    }

    fn verify(&mut self, id: String, command: String, timeout_secs: Option<u64>) {
        if self.base.is_none() {
            self.out
                .frame(&error(Some(id), VERIFY, "send ch_start before ch_verify"));
            return;
        }
        let git = self.git.clone();
        let out = self.out.clone();
        let stop = Arc::clone(&self.shutdown);
        self.workers.push(thread::spawn(move || {
            let job = Job {
                id: id.clone(),
                command,
                timeout_secs,
                workdir: git.workdir().to_owned(),
                head: git.head(),
            };
            let frame = match verify::run(job, &stop) {
                Ok(evidence) => RunnerFrame::Evidence(evidence),
                Err(e) => error(Some(id), VERIFY, e),
            };
            out.frame(&frame);
        }));
    }

    fn diff(&mut self, id: String) {
        let Some(base) = self.base.clone() else {
            self.out
                .frame(&error(Some(id), DIFF, "send ch_start before ch_diff"));
            return;
        };
        let git = self.git.clone();
        let out = self.out.clone();
        let index = self.dir.unique_path("diff-index");
        self.workers.push(thread::spawn(move || {
            let frame = match git.diff(&base, &index, MAX_OUTPUT_TAIL_BYTES, MAX_PATCH_BYTES) {
                Ok(diff) => RunnerFrame::DiffResult {
                    id,
                    base: base.id,
                    stat: diff.stat,
                    patch: diff.patch,
                    truncated: diff.truncated,
                },
                Err(e) => error(Some(id), DIFF, e),
            };
            out.frame(&frame);
        }));
    }

    fn stop(&mut self, id: String) {
        match std::mem::replace(&mut self.omp, OmpState::Lost) {
            OmpState::NotStarted => {
                self.omp = OmpState::NotStarted;
                self.out
                    .frame(&error(Some(id), STOP, "send ch_start before ch_stop"));
            }
            OmpState::Running(omp) => {
                let finished = omp.finish(omp::DRAIN_ON_STOP);
                if let Some(problem) = finished.problem {
                    self.out.close_omp();
                    self.out.frame(&error(Some(id), STOP, problem));
                } else {
                    self.omp = OmpState::Ended {
                        exit_code: finished.exit_code,
                    };
                    self.out.close_omp_with(&RunnerFrame::Stopped {
                        id,
                        exit_code: finished.exit_code,
                    });
                }
            }
            OmpState::Ended { exit_code } => {
                self.omp = OmpState::Ended { exit_code };
                self.out.frame(&RunnerFrame::Stopped { id, exit_code });
            }
            OmpState::Lost => self.out.frame(&error(
                Some(id),
                STOP,
                "omp could not be ended earlier, so whether it still runs is unknown",
            )),
        }
    }

    /// Reports an omp that exited without a `ch_stop`.
    fn watch_omp(&mut self) {
        let exited = match &mut self.omp {
            OmpState::Running(omp) => omp.exited().is_some(),
            _ => false,
        };
        if !exited {
            return;
        }
        let OmpState::Running(omp) = std::mem::replace(&mut self.omp, OmpState::Lost) else {
            return;
        };
        let finished = omp.finish(omp::DRAIN_ON_EXIT);
        self.out.close_omp_with(&RunnerFrame::OmpExited {
            exit_code: finished.exit_code,
            stderr_tail: finished.stderr_tail,
        });
        match finished.problem {
            None => {
                self.omp = OmpState::Ended {
                    exit_code: finished.exit_code,
                };
            }
            Some(problem) => self.out.frame(&error(None, STOP, problem)),
        }
    }

    fn land(&mut self, request: &LandRequest) {
        let id = Some(request.id.clone());
        let refusal = match &self.omp {
            OmpState::NotStarted => Some("send ch_start before ch_land"),
            OmpState::Running(_) => {
                Some("refused while omp is running: send ch_stop and wait for ch_stopped first")
            }
            OmpState::Lost => {
                Some("omp could not be ended, so the working tree may still be changing")
            }
            OmpState::Ended { .. } => None,
        };
        self.workers.retain(|worker| !worker.is_finished());
        let refusal = refusal.or_else(|| {
            (!self.workers.is_empty())
                .then_some("refused while a check or diff is still running: wait for its result")
        });
        let base = match (refusal, &self.base) {
            (None, Some(base)) => base.clone(),
            (refusal, _) => {
                let reason = refusal.unwrap_or("send ch_start before ch_land");
                self.out.frame(&error(id, LAND, reason));
                return;
            }
        };
        let scrubber = Scrubber::for_token(request.token.as_deref());
        let outcome = Lander {
            git: &self.git,
            base: &base,
            session: &self.dir,
            landed: &mut self.landed,
            opener: self.opener.as_ref(),
        }
        .land(request);
        let frame = match outcome {
            Ok(done) => RunnerFrame::Landed {
                id: request.id.clone(),
                branch: done.branch,
                commit: done.commit,
                pull_request_url: done.pull_request_url.map(|url| scrubber.scrub(&url)),
            },
            Err(e) => error(id, LAND, scrubber.scrub(&e)),
        };
        self.out.frame(&frame);
    }
}

/// The error for a client line that could not be routed. A malformed runner frame still
/// names its `id` when it has one, and nothing it carried as a secret is echoed back.
fn bad_line(line: &str, route: &RouteError) -> RunnerFrame {
    let RouteError::BadRunnerFrame(_) = route else {
        return error(None, PROTOCOL, route.to_string());
    };
    let value: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
    let mut secrets: Vec<String> = Vec::new();
    for field in ["token", "env"] {
        match value.get(field) {
            Some(Value::String(text)) => secrets.push(text.clone()),
            Some(Value::Object(map)) => {
                secrets.extend(map.values().filter_map(Value::as_str).map(str::to_owned));
            }
            _ => {}
        }
    }
    error(
        id,
        PROTOCOL,
        Scrubber::new(secrets).scrub(&route.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_each_project_config_dir_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(project_config_notice(dir.path()).is_none());
        fs::create_dir(dir.path().join(".omp")).unwrap();
        fs::write(dir.path().join(".gemini"), "").unwrap();
        let Some(RunnerFrame::Notice { kind, message }) = project_config_notice(dir.path()) else {
            panic!("expected a notice");
        };
        assert_eq!(kind, "project_omp_config");
        assert!(message.contains(".omp, .gemini"), "{message}");
        assert!(!message.contains(".claude"), "{message}");
        assert!(message.contains("approval"), "{message}");
        assert!(message.contains("extensions"), "{message}");
    }

    #[test]
    fn a_bad_runner_frame_keeps_its_id_and_hides_its_secrets() {
        let line = r#"{"type":"ch_land","id":"l1","token":"test-token-not-real","author":"test-token-not-real"}"#;
        let route = route_client_line(line).unwrap_err();
        let RunnerFrame::Error { id, stage, message } = bad_line(line, &route) else {
            panic!("expected an error");
        };
        assert_eq!(id.as_deref(), Some("l1"));
        assert_eq!(stage, PROTOCOL);
        assert!(!message.contains("test-token-not-real"), "{message}");

        let line =
            r#"{"type":"ch_start","session":"s","omp_args":[],"env":"test-model-key-not-real"}"#;
        let route = route_client_line(line).unwrap_err();
        let RunnerFrame::Error { id, message, .. } = bad_line(line, &route) else {
            panic!("expected an error");
        };
        assert_eq!(id, None);
        assert!(!message.contains("test-model-key-not-real"), "{message}");
    }

    #[test]
    fn a_line_that_is_not_json_is_a_protocol_error() {
        let route = route_client_line("not json").unwrap_err();
        let RunnerFrame::Error { id, stage, .. } = bad_line("not json", &route) else {
            panic!("expected an error");
        };
        assert_eq!((id, stage.as_str()), (None, PROTOCOL));
    }
}
