//! Frames between the desktop client and `ch-runner`.
//!
//! The runner speaks omp's own JSONL on its stdio and adds a few frames of its own, each with a
//! `type` that starts with [`RUNNER_FRAME_PREFIX`]. A client line of any other type goes to omp's
//! stdin unchanged, and an omp line goes back to the client unchanged.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped when a frame changes shape or a request gains a guarantee. The runner announces it
/// in [`RunnerFrame::Hello`]. 3 added the secret scan to land; 4 added `ch-runner preflight`.
pub const RUNNER_PROTOCOL_VERSION: u32 = 4;

/// The oldest runner protocol whose land scans for secrets. The app refuses to land through an
/// older runner, since one would commit and push without the scan and could not say so.
pub const SECRET_SCAN_PROTOCOL: u32 = 3;

const _: () = assert!(SECRET_SCAN_PROTOCOL <= RUNNER_PROTOCOL_VERSION);

/// Every frame the runner adds has a `type` with this prefix. omp never uses it.
pub const RUNNER_FRAME_PREFIX: &str = "ch_";

/// The runner keeps at most this much of a verification command's output (the end of it).
pub const MAX_OUTPUT_TAIL_BYTES: usize = 64 * 1024;

/// The runner sends at most this much of a diff. `ch_diff_result.truncated` says when it cut.
pub const MAX_PATCH_BYTES: usize = 512 * 1024;

/// The most checkpoints the runner lists in [`RunnerFrame::Checkpoints`].
pub const MAX_CHECKPOINTS: usize = 1000;

/// The most findings a refused land lists in [`SecretScan::findings`]; the rest are counted.
pub const MAX_SECRET_FINDINGS: usize = 50;

pub(crate) const MASKED: &str = "<hidden>";

/// What the client sends the runner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    /// Starts omp. Valid once, as the first runner frame of a connection.
    #[serde(rename = "ch_start")]
    Start(StartSession),
    /// Runs a command in the working tree and reports its real exit code.
    #[serde(rename = "ch_verify")]
    Verify {
        id: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    /// Reports the changes from `from` to `to`. A number names a checkpoint and 0 the
    /// session's base. Without `from` the diff starts at the base, and without `to` it ends
    /// at the working tree. A runner older than protocol 2 ignores both.
    #[serde(rename = "ch_diff")]
    Diff {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<u32>,
    },
    /// Saves the working tree as the next checkpoint. Valid only after `ch_checkpoints`.
    #[serde(rename = "ch_checkpoint")]
    Checkpoint { id: String },
    /// Saves the working tree as a checkpoint, then puts back checkpoint `turn` (0: the base).
    /// Valid only after `ch_checkpoints`.
    #[serde(rename = "ch_restore")]
    Restore { id: String, turn: u32 },
    /// Ends the agent: closes omp's stdin, drains its output, then ends what it left running.
    #[serde(rename = "ch_stop")]
    Stop { id: String },
    /// Commits the work on a branch, pushes it, and optionally opens a pull request.
    /// Refused while the agent is still running.
    #[serde(rename = "ch_land")]
    Land(LandRequest),
}

/// The `ch_start` payload.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSession {
    pub session: String,
    /// Arguments after `omp`. The runner adds nothing but `--config <overlay>`.
    pub omp_args: Vec<String>,
    /// YAML the runner writes outside the working tree and passes with `--config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_overlay: Option<String>,
    /// Environment for omp only, usually the model provider's key.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl fmt::Debug for StartSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: BTreeMap<&str, &str> = self.env.keys().map(|k| (k.as_str(), MASKED)).collect();
        f.debug_struct("StartSession")
            .field("session", &self.session)
            .field("omp_args", &self.omp_args)
            .field("config_overlay", &self.config_overlay)
            .field("env", &env)
            .finish()
    }
}

/// The `ch_land` payload.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandRequest {
    pub id: String,
    pub branch: String,
    pub message: String,
    /// The commit's author and committer. A fresh sandbox usually has no git identity.
    pub author: GitAuthor,
    /// Where the branch is pushed. Never read from the working tree's own git config.
    pub remote_url: String,
    /// Sent only now, after the agent has stopped, and used for this push and pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestSpec>,
    /// The [`SecretScan::tree`] of a land the person chose to make anyway. The runner skips
    /// the secret scan only when the tree it is about to commit is this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_tree: Option<String>,
}

impl fmt::Debug for LandRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LandRequest")
            .field("id", &self.id)
            .field("branch", &self.branch)
            .field("message", &self.message)
            .field("author", &self.author)
            .field("remote_url", &self.remote_url)
            .field("token", &self.token.as_ref().map(|_| MASKED))
            .field("pull_request", &self.pull_request)
            .field("acknowledged_tree", &self.acknowledged_tree)
            .finish()
    }
}

/// What kind of credential a line looks like it holds. The value itself is never sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// A token with a known provider prefix, such as a GitHub, cloud or model API key.
    ProviderToken,
    /// A PEM private key.
    PrivateKey,
}

impl SecretKind {
    /// How a person reads the kind, in a refusal and in the app.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ProviderToken => "provider token",
            Self::PrivateKey => "private key",
        }
    }
}

/// A line a land would add that looks like it holds a credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretFinding {
    pub path: String,
    /// 1-based, in the new version of the file.
    pub line: u32,
    pub kind: SecretKind,
}

/// Why the secret scan refused a land, carried on that land's `ch_error`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretScan {
    /// The tree the scan read. A `ch_land` naming it as `acknowledged_tree` lands it unscanned.
    pub tree: String,
    /// The first [`MAX_SECRET_FINDINGS`] lines that matched.
    pub findings: Vec<SecretFinding>,
    /// How many more lines matched than are listed.
    #[serde(default)]
    pub more: u32,
    /// The change was too large to read whole, so none of it was scanned.
    #[serde(default)]
    pub too_large: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAuthor {
    pub name: String,
    pub email: String,
}

/// A GitHub pull request to open after the push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSpec {
    /// `owner/name`.
    pub repo: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

/// What the runner sends the client, besides omp's own frames.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RunnerFrame {
    /// First line the runner writes.
    #[serde(rename = "ch_runner_hello")]
    Hello {
        version: String,
        protocol: u32,
        workdir: String,
    },
    /// Something the person should see before trusting the session, such as a repository
    /// that carries its own omp config.
    #[serde(rename = "ch_notice")]
    Notice { kind: String, message: String },
    #[serde(rename = "ch_evidence")]
    Evidence(Evidence),
    #[serde(rename = "ch_diff_result")]
    DiffResult {
        id: String,
        base: String,
        /// The range the `ch_diff` asked for, echoed back.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<u32>,
        stat: String,
        patch: String,
        truncated: bool,
        /// The working tree as a git tree, written the way [`Evidence`]'s trees are, when the
        /// range ends at the working tree. `None` for a range between two checkpoints, when the
        /// runner could not write the tree, and from a runner older than protocol 4.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tree: Option<String>,
    },
    /// Sent after a start when this working tree keeps checkpoints, with the ones it has.
    #[serde(rename = "ch_checkpoints")]
    Checkpoints { checkpoints: Vec<Checkpoint> },
    /// A checkpoint was saved, by the `ch_checkpoint` or the `ch_restore` named by `id`.
    #[serde(rename = "ch_checkpointed")]
    Checkpointed { id: String, checkpoint: Checkpoint },
    #[serde(rename = "ch_restored")]
    Restored { id: String, turn: u32 },
    #[serde(rename = "ch_stopped")]
    Stopped { id: String, exit_code: Option<i32> },
    #[serde(rename = "ch_landed")]
    Landed {
        id: String,
        branch: String,
        commit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pull_request_url: Option<String>,
    },
    /// omp ended without a `ch_stop`.
    #[serde(rename = "ch_omp_exited")]
    OmpExited {
        exit_code: Option<i32>,
        stderr_tail: Vec<String>,
    },
    #[serde(rename = "ch_error")]
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        stage: String,
        /// Complete on its own, for a client that does not read `secret_scan`.
        message: String,
        /// Set when the secret scan refused a land.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_scan: Option<SecretScan>,
    },
}

/// A snapshot of the working tree, kept as `refs/c-harness/<session>/turn-<turn>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Counts from 1. Restores and diffs use 0 for the session's base.
    pub turn: u32,
    pub commit: String,
    pub kind: CheckpointKind,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// Saved when the agent finished a run.
    Run,
    /// Saved by a restore, so the restore can be undone.
    BeforeRestore,
}

/// The result of a `ch_verify`, measured by the runner rather than reported by the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub command: String,
    /// `None` when the process was killed by a signal or by the timeout.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// `HEAD` when the command ran, if the working tree has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The work as a git tree (the working tree staged on the session's base) when the command
    /// started. `None` when it could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_before: Option<String>,
    /// The same when the command ended. It differs from `tree_before` when files changed
    /// while the check ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_after: Option<String>,
    /// The end of stdout and stderr, interleaved as written, at most [`MAX_OUTPUT_TAIL_BYTES`].
    pub output_tail: String,
    pub truncated: bool,
    pub timed_out: bool,
}

impl Evidence {
    pub fn passed(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
}

impl ClientFrame {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("client frames always serialize")
    }
}

impl RunnerFrame {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("runner frames always serialize")
    }
}

/// Why a line could not be routed.
#[derive(Debug, PartialEq, Eq)]
pub enum RouteError {
    NotJson(String),
    MissingType,
    /// A `ch_` frame this side does not know, or a known one with bad fields.
    BadRunnerFrame(String),
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJson(e) => write!(f, "line is not a JSON object: {e}"),
            Self::MissingType => f.write_str("frame has no string \"type\""),
            Self::BadRunnerFrame(e) => write!(f, "bad runner frame: {e}"),
        }
    }
}

impl std::error::Error for RouteError {}

/// A line the runner read from the client.
// One value per line, consumed at once: boxing the large variant would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq, Eq)]
pub enum ClientLine {
    Runner(ClientFrame),
    /// For omp's stdin, byte for byte.
    Omp(String),
}

/// A line the client read from the runner.
#[derive(Debug, PartialEq)]
pub enum RunnerLine {
    Runner(RunnerFrame),
    Omp(Value),
}

fn frame_type(value: &Value) -> Result<&str, RouteError> {
    value
        .get("type")
        .and_then(Value::as_str)
        .ok_or(RouteError::MissingType)
}

fn parse_object(line: &str) -> Result<Value, RouteError> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| RouteError::NotJson(e.to_string()))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(RouteError::NotJson("not an object".into()))
    }
}

/// Sorts a client line into a runner command or a line for omp.
pub fn route_client_line(line: &str) -> Result<ClientLine, RouteError> {
    let value = parse_object(line)?;
    if frame_type(&value)?.starts_with(RUNNER_FRAME_PREFIX) {
        serde_json::from_value(value)
            .map(ClientLine::Runner)
            .map_err(|e| RouteError::BadRunnerFrame(e.to_string()))
    } else {
        Ok(ClientLine::Omp(line.to_owned()))
    }
}

/// Sorts a runner line into a runner frame or an omp frame.
pub fn route_runner_line(line: &str) -> Result<RunnerLine, RouteError> {
    let value = parse_object(line)?;
    if frame_type(&value)?.starts_with(RUNNER_FRAME_PREFIX) {
        serde_json::from_value(value)
            .map(RunnerLine::Runner)
            .map_err(|e| RouteError::BadRunnerFrame(e.to_string()))
    } else {
        Ok(RunnerLine::Omp(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> StartSession {
        StartSession {
            session: "s1".into(),
            omp_args: vec!["--mode".into(), "rpc".into()],
            config_overlay: Some("startup:\n  checkUpdate: false\n".into()),
            env: BTreeMap::from([("ANTHROPIC_API_KEY".into(), "sk-ant-secret-value".into())]),
        }
    }

    fn land() -> LandRequest {
        LandRequest {
            id: "l1".into(),
            branch: "ch/s1".into(),
            message: "Add a test".into(),
            author: GitAuthor {
                name: "Octo Cat".into(),
                email: "octo@example.com".into(),
            },
            remote_url: "https://github.com/octo/repo.git".into(),
            token: Some("github_pat_secret_value".into()),
            pull_request: Some(PullRequestSpec {
                repo: "octo/repo".into(),
                base: "main".into(),
                title: "Add a test".into(),
                body: String::new(),
            }),
            acknowledged_tree: None,
        }
    }

    fn refused_scan() -> SecretScan {
        SecretScan {
            tree: "t1".into(),
            findings: vec![SecretFinding {
                path: "src/config.rs".into(),
                line: 12,
                kind: SecretKind::ProviderToken,
            }],
            more: 3,
            too_large: false,
        }
    }

    fn checkpoint(turn: u32, kind: CheckpointKind) -> Checkpoint {
        Checkpoint {
            turn,
            commit: "c0ffee".into(),
            kind,
            at: 1_700_000_000,
        }
    }

    #[test]
    fn client_frames_round_trip() {
        let frames = [
            ClientFrame::Start(start()),
            ClientFrame::Verify {
                id: "v1".into(),
                command: "cargo test".into(),
                timeout_secs: Some(600),
            },
            ClientFrame::Diff {
                id: "d1".into(),
                from: None,
                to: None,
            },
            ClientFrame::Diff {
                id: "d2".into(),
                from: Some(0),
                to: Some(3),
            },
            ClientFrame::Checkpoint { id: "c1".into() },
            ClientFrame::Restore {
                id: "r1".into(),
                turn: 2,
            },
            ClientFrame::Stop { id: "x1".into() },
            ClientFrame::Land(land()),
            ClientFrame::Land(LandRequest {
                acknowledged_tree: Some("t1".into()),
                ..land()
            }),
        ];
        for frame in frames {
            let line = frame.to_line();
            assert_eq!(route_client_line(&line), Ok(ClientLine::Runner(frame)));
        }
    }

    #[test]
    fn runner_frames_round_trip() {
        let frames = [
            RunnerFrame::Hello {
                version: "0.1.0".into(),
                protocol: RUNNER_PROTOCOL_VERSION,
                workdir: "/work/repo".into(),
            },
            RunnerFrame::Evidence(Evidence {
                id: "v1".into(),
                command: "cargo test".into(),
                exit_code: Some(101),
                duration_ms: 1200,
                head: Some("abc123".into()),
                tree_before: Some("t1".into()),
                tree_after: Some("t1".into()),
                output_tail: "test result: FAILED".into(),
                truncated: false,
                timed_out: false,
            }),
            RunnerFrame::DiffResult {
                id: "d2".into(),
                base: "abc123".into(),
                from: Some(2),
                to: None,
                stat: String::new(),
                patch: String::new(),
                truncated: false,
                tree: Some("t2".into()),
            },
            RunnerFrame::Checkpoints {
                checkpoints: vec![checkpoint(1, CheckpointKind::Run)],
            },
            RunnerFrame::Checkpointed {
                id: "r1".into(),
                checkpoint: checkpoint(2, CheckpointKind::BeforeRestore),
            },
            RunnerFrame::Restored {
                id: "r1".into(),
                turn: 1,
            },
            RunnerFrame::Landed {
                id: "l1".into(),
                branch: "ch/s1".into(),
                commit: "def456".into(),
                pull_request_url: None,
            },
            RunnerFrame::Error {
                id: None,
                stage: "start".into(),
                message: "omp not found".into(),
                secret_scan: None,
            },
            RunnerFrame::Error {
                id: Some("l1".into()),
                stage: "land".into(),
                message: "nothing was landed".into(),
                secret_scan: Some(refused_scan()),
            },
        ];
        for frame in frames {
            let line = frame.to_line();
            assert_eq!(route_runner_line(&line), Ok(RunnerLine::Runner(frame)));
        }
    }

    #[test]
    fn omp_lines_pass_through_unchanged() {
        let line = r#"{"id":"req_1","type":"prompt","message":"hi"}"#;
        assert_eq!(
            route_client_line(line),
            Ok(ClientLine::Omp(line.to_owned()))
        );

        let out = r#"{"type":"agent_start"}"#;
        match route_runner_line(out) {
            Ok(RunnerLine::Omp(value)) => assert_eq!(value["type"], "agent_start"),
            other => panic!("expected an omp frame, got {other:?}"),
        }
    }

    #[test]
    fn unknown_runner_frames_are_refused_not_forwarded() {
        let line = r#"{"type":"ch_format_disk"}"#;
        assert!(matches!(
            route_client_line(line),
            Err(RouteError::BadRunnerFrame(_))
        ));
    }

    #[test]
    fn lines_without_a_type_are_refused() {
        assert_eq!(
            route_client_line(r#"{"id":"1"}"#),
            Err(RouteError::MissingType)
        );
        assert!(matches!(
            route_client_line("[1,2]"),
            Err(RouteError::NotJson(_))
        ));
        assert!(matches!(
            route_client_line("not json"),
            Err(RouteError::NotJson(_))
        ));
    }

    #[test]
    fn debug_output_hides_secrets() {
        let start = format!("{:?}", ClientFrame::Start(start()));
        assert!(start.contains("ANTHROPIC_API_KEY"));
        assert!(!start.contains("sk-ant-secret-value"));

        let land = format!("{:?}", ClientFrame::Land(land()));
        assert!(!land.contains("github_pat_secret_value"));
        assert!(land.contains(MASKED));
    }

    #[test]
    fn evidence_from_an_older_runner_has_no_trees() {
        let line = r#"{"type":"ch_evidence","id":"v1","command":"true","exit_code":0,
            "duration_ms":1,"output_tail":"","truncated":false,"timed_out":false}"#;
        match route_runner_line(line) {
            Ok(RunnerLine::Runner(RunnerFrame::Evidence(evidence))) => {
                assert_eq!((evidence.tree_before, evidence.tree_after), (None, None));
            }
            other => panic!("expected evidence, got {other:?}"),
        }
    }

    #[test]
    fn a_diff_without_a_range_is_the_whole_session() {
        let old = r#"{"type":"ch_diff","id":"d1"}"#;
        let frame = ClientFrame::Diff {
            id: "d1".into(),
            from: None,
            to: None,
        };
        assert_eq!(
            route_client_line(old),
            Ok(ClientLine::Runner(frame.clone()))
        );
        assert_eq!(frame.to_line(), old);

        let result = r#"{"type":"ch_diff_result","id":"d1","base":"b","stat":"",
            "patch":"","truncated":false}"#;
        match route_runner_line(result) {
            Ok(RunnerLine::Runner(RunnerFrame::DiffResult { from, to, .. })) => {
                assert_eq!((from, to), (None, None));
            }
            other => panic!("expected a diff result, got {other:?}"),
        }
    }

    #[test]
    fn a_diff_result_from_an_older_runner_has_no_tree() {
        let line = r#"{"type":"ch_diff_result","id":"d1","base":"b","stat":"","patch":"",
            "truncated":false}"#;
        match route_runner_line(line) {
            Ok(RunnerLine::Runner(RunnerFrame::DiffResult { tree, .. })) => {
                assert_eq!(tree, None);
            }
            other => panic!("expected a diff result, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_kinds_are_snake_case() {
        let line = RunnerFrame::Checkpointed {
            id: "c1".into(),
            checkpoint: checkpoint(3, CheckpointKind::BeforeRestore),
        }
        .to_line();
        assert!(line.contains(r#""kind":"before_restore""#), "{line}");
        let bad = r#"{"type":"ch_restore","id":"r1","turn":-1}"#;
        assert!(matches!(
            route_client_line(bad),
            Err(RouteError::BadRunnerFrame(_))
        ));
    }

    #[test]
    fn a_secret_scan_is_optional_on_both_sides_and_names_its_kind() {
        let old_error = r#"{"type":"ch_error","id":"l1","stage":"land","message":"no"}"#;
        match route_runner_line(old_error) {
            Ok(RunnerLine::Runner(RunnerFrame::Error { secret_scan, .. })) => {
                assert_eq!(secret_scan, None);
            }
            other => panic!("expected an error, got {other:?}"),
        }
        let plain = RunnerFrame::Error {
            id: None,
            stage: "land".into(),
            message: "no".into(),
            secret_scan: None,
        }
        .to_line();
        assert!(!plain.contains("secret_scan"), "{plain}");
        let refused = RunnerFrame::Error {
            id: Some("l1".into()),
            stage: "land".into(),
            message: "no".into(),
            secret_scan: Some(refused_scan()),
        }
        .to_line();
        assert!(refused.contains(r#""kind":"provider_token""#), "{refused}");
        let sparse = r#"{"type":"ch_error","stage":"land","message":"no",
            "secret_scan":{"tree":"t1","findings":[]}}"#;
        match route_runner_line(sparse) {
            Ok(RunnerLine::Runner(RunnerFrame::Error {
                secret_scan: Some(scan),
                ..
            })) => assert_eq!((scan.more, scan.too_large), (0, false)),
            other => panic!("expected a refused scan, got {other:?}"),
        }

        let plain_land = ClientFrame::Land(land()).to_line();
        assert!(!plain_land.contains("acknowledged_tree"), "{plain_land}");
        let old_land = r#"{"type":"ch_land","id":"l1","branch":"b","message":"m",
            "author":{"name":"n","email":"e"},"remote_url":"/r"}"#;
        match route_client_line(old_land) {
            Ok(ClientLine::Runner(ClientFrame::Land(request))) => {
                assert_eq!(request.acknowledged_tree, None);
            }
            other => panic!("expected a land, got {other:?}"),
        }
    }

    #[test]
    fn debug_output_shows_the_acknowledged_tree() {
        let land = format!(
            "{:?}",
            LandRequest {
                acknowledged_tree: Some("t1".into()),
                ..land()
            }
        );
        assert!(land.contains(r#"acknowledged_tree: Some("t1")"#), "{land}");
    }

    #[test]
    fn evidence_passes_only_on_zero_without_timeout() {
        let mut evidence = Evidence {
            id: "v".into(),
            command: "true".into(),
            exit_code: Some(0),
            duration_ms: 1,
            head: None,
            tree_before: None,
            tree_after: None,
            output_tail: String::new(),
            truncated: false,
            timed_out: false,
        };
        assert!(evidence.passed());
        let line = RunnerFrame::Evidence(evidence.clone()).to_line();
        assert!(!line.contains("tree_"), "{line}");
        evidence.timed_out = true;
        assert!(!evidence.passed());
        evidence.timed_out = false;
        evidence.exit_code = None;
        assert!(!evidence.passed());
    }

    #[test]
    fn a_secret_kind_reads_as_words() {
        assert_eq!(SecretKind::ProviderToken.label(), "provider token");
        assert_eq!(SecretKind::PrivateKey.label(), "private key");
    }
}
