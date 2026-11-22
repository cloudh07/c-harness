//! The session reducer: omp and runner frames, plus what the app sent, folded into one state.
//!
//! Lines go in through [`Session::apply_line`] in the order they were read, and the app reports
//! each frame it writes through [`Session::apply_action`]. Nothing here reads a clock or waits,
//! so the same lines always give the same state.

use ch_protocol::runner::{Checkpoint, Evidence, RunnerFrame, RunnerLine};
use serde_json::Value;

use crate::approval::{ApprovalRequest, withdrawn_approval};
use crate::checkpoints;
use crate::context::note_compaction;
use crate::failure::{FailureKind, Retrying, prompt_error_kind};

const PROMPT_STAGE: &str = "prompt";
const OMP_STAGE: &str = "omp";
const LAND_STAGE: &str = "land";
// A failed check, diff, checkpoint or restore is news about the work, not a broken session.
const NON_FATAL_STAGES: &[&str] = &["verify", "diff", "checkpoint", "restore"];
// The runner's stage for a line it could not pass to omp.
const PROTOCOL_STAGE: &str = "protocol";
const PROMPT_COMMANDS: &[&str] = &["prompt", "abort_and_prompt"];

/// Where a session stands, which decides what the app offers next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// No prompt is running: the session has not started, or omp is ready for a prompt.
    Idle,
    /// `ch_start` was sent and omp has not said it is ready.
    Starting,
    /// omp is working on a prompt.
    Running,
    /// omp is waiting on at least one approval.
    NeedsYou,
    /// omp settled or was stopped, and the work is ready to review, verify and land.
    NeedsReview,
    /// The branch was pushed. Later frames do not change this.
    Landed,
    /// Something broke; [`Session::failure`] says what.
    Failed,
}

/// A frame the app wrote, reported so the state can follow it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAction {
    /// `ch_start`. Clears everything from an earlier session.
    StartSent,
    /// An omp `prompt`. Also the retry after a failed prompt.
    PromptSent,
    /// The `extension_ui_response` for the approval with this id.
    ApprovalAnswered { id: String },
    /// `ch_stop`.
    StopSent,
    /// `ch_land`. The runner stops omp itself before landing. Also the retry after a failed land.
    LandSent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolStatus {
    Running,
    Succeeded,
    /// omp reported an error, or did not say the call succeeded.
    Failed,
}

/// One tool call omp ran or is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub status: ToolStatus,
}

/// What went wrong and where.
///
/// `stage` is the runner's `ch_error` stage, or one of two set here: `prompt` when omp rejected
/// or failed a prompt, and `omp` when omp exited without being asked to. Only a prompt omp
/// failed gets a `kind` other than [`FailureKind::Permanent`], read from omp's error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub stage: String,
    pub kind: FailureKind,
    pub message: String,
}

/// A `ch_notice` from the runner, such as a repository that carries its own omp config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub kind: String,
    pub message: String,
}

/// The latest `ch_diff_result`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    pub base: String,
    pub stat: String,
    pub patch: String,
    pub truncated: bool,
}

/// The latest diff over a range of checkpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnDiff {
    /// A checkpoint's turn, or 0 for the base.
    pub from: u32,
    /// `None` for the working tree.
    pub to: Option<u32>,
    pub diff: Diff,
}

/// Where the work landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    pub branch: String,
    pub commit: String,
    pub pull_request_url: Option<String>,
}

/// A run waiting for the model's first word, after a prompt or after one of omp's retries.
///
/// `epoch` changes whenever the wait starts over, so the app can restart its timer. omp waits
/// `delay_ms` before it sends a retry, which the app's deadline should not count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyWait {
    pub epoch: u32,
    pub delay_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Starting,
    Running,
    Review,
    Landed,
    Failed,
}

/// Everything the app shows about one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    phase: Phase,
    pending: Vec<ApprovalRequest>,
    reply: String,
    tool_calls: Vec<ToolCall>,
    evidence: Vec<Evidence>,
    last_diff: Option<Diff>,
    turn_diff: Option<TurnDiff>,
    // `None` until the runner lists them, which it does only for a session worktree.
    checkpoints: Option<Vec<Checkpoint>>,
    notices: Vec<Notice>,
    warnings: Vec<Failure>,
    failure: Option<Failure>,
    landing: Option<Landing>,
    retrying: Option<Retrying>,
    // Where the reply stood when the last message that did not fail ended, so a retry can drop
    // the text of the attempt that failed.
    reply_mark: usize,
    // How many tool calls the session had when the latest prompt went out; `None` before any
    // prompt, when there is no run to judge.
    run_tools: Option<usize>,
    // The model said something in the current attempt: text, thinking or a tool call.
    heard: bool,
    wait_epoch: u32,
    wait_delay_ms: u64,
    // Set by `ch_stop` or `ch_land`, so omp exiting afterwards is not a crash.
    exit_expected: bool,
    // Set only by the runner saying omp ended. Nothing more comes from it, so a prompt sent after
    // this cannot start a run. A land the runner refuses leaves omp running and this unset.
    omp_gone: bool,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> SessionState {
        match self.phase {
            Phase::Failed => SessionState::Failed,
            Phase::Landed => SessionState::Landed,
            _ if !self.pending.is_empty() => SessionState::NeedsYou,
            Phase::Idle => SessionState::Idle,
            Phase::Starting => SessionState::Starting,
            Phase::Running => SessionState::Running,
            Phase::Review => SessionState::NeedsReview,
        }
    }

    /// Approvals omp is waiting on, oldest first.
    pub fn pending_approvals(&self) -> &[ApprovalRequest] {
        &self.pending
    }

    /// The assistant's text for the latest prompt, as streamed so far.
    pub fn reply(&self) -> &str {
        &self.reply
    }

    /// Every tool call in the session, in the order omp started them.
    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }

    /// Verify results, in the order the runner sent them.
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    pub fn last_diff(&self) -> Option<&Diff> {
        self.last_diff.as_ref()
    }

    pub fn turn_diff(&self) -> Option<&TurnDiff> {
        self.turn_diff.as_ref()
    }

    /// Whether the runner keeps checkpoints for this session.
    pub fn checkpoints_on(&self) -> bool {
        self.checkpoints.is_some()
    }

    /// Saved checkpoints in turn order; empty while they are off.
    pub fn checkpoints(&self) -> &[Checkpoint] {
        self.checkpoints.as_deref().unwrap_or_default()
    }

    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    /// Runner errors that did not fail the session: a verify or diff that could not run.
    pub fn warnings(&self) -> &[Failure] {
        &self.warnings
    }

    /// Why the session failed. Only set while the state is [`SessionState::Failed`].
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    pub fn landing(&self) -> Option<&Landing> {
        self.landing.as_ref()
    }

    /// omp's retry in progress, only while it is working.
    pub fn retrying(&self) -> Option<&Retrying> {
        self.retrying.as_ref()
    }

    pub fn pull_request_url(&self) -> Option<&str> {
        self.landing.as_ref()?.pull_request_url.as_deref()
    }

    /// The wait for the model's first word, while omp works and the model has said nothing.
    pub fn reply_wait(&self) -> Option<ReplyWait> {
        (self.state() == SessionState::Running && !self.heard).then_some(ReplyWait {
            epoch: self.wait_epoch,
            delay_ms: self.wait_delay_ms,
        })
    }

    /// The latest run finished with no reply text and no tool calls.
    pub fn run_was_empty(&self) -> bool {
        self.run_tools == Some(self.tool_calls.len())
            && self.state() == SessionState::NeedsReview
            && self.reply.trim().is_empty()
    }

    /// Folds in a frame the app wrote.
    ///
    /// A failure is cleared only by an action that moves past it: a new land after a `land`
    /// failure, and a new prompt after a `prompt` failure or after a `land` failure that left
    /// omp running. Any other failure stays until the next [`ClientAction::StartSent`].
    pub fn apply_action(&mut self, action: &ClientAction) {
        match action {
            ClientAction::StartSent => {
                *self = Self {
                    phase: Phase::Starting,
                    ..Self::default()
                };
            }
            ClientAction::PromptSent => {
                if self.omp_gone {
                    return;
                }
                self.clear_failure(PROMPT_STAGE);
                self.clear_failure(LAND_STAGE);
                self.retrying = None;
                if self.is_live() {
                    self.phase = Phase::Running;
                    self.reply.clear();
                    self.reply_mark = 0;
                    self.run_tools = Some(self.tool_calls.len());
                    self.restart_wait(0);
                }
            }
            ClientAction::ApprovalAnswered { id } => self.pending.retain(|r| r.id() != id),
            ClientAction::StopSent => {
                self.exit_expected = true;
                self.retrying = None;
            }
            ClientAction::LandSent => {
                self.exit_expected = true;
                self.clear_failure(LAND_STAGE);
            }
        }
    }

    /// Folds in a line read from the runner.
    pub fn apply_line(&mut self, line: &RunnerLine) {
        match line {
            RunnerLine::Omp(frame) => self.apply_omp(frame),
            RunnerLine::Runner(frame) => self.apply_runner(frame),
        }
    }

    fn apply_omp(&mut self, frame: &Value) {
        match frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "ready" if self.phase == Phase::Starting => self.phase = Phase::Idle,
            "response" => self.on_response(frame),
            "agent_start" => self.run(),
            "message_start" => self.reply_mark = self.reply.len(),
            "message_update" => self.on_message(frame),
            "message_end" => self.on_message_end(frame),
            "auto_retry_start" if self.phase == Phase::Running => self.on_retry(frame),
            "auto_retry_end" => self.retrying = None,
            "tool_execution_start" => self.on_tool_start(frame),
            "tool_execution_end" => self.on_tool_end(frame),
            "prompt_result" => self.on_prompt_result(frame),
            "session_settled" => {
                self.retrying = None;
                if self.phase == Phase::Running {
                    self.phase = Phase::Review;
                }
            }
            "extension_ui_request" => self.on_ui_request(frame),
            "auto_compaction_start" | "auto_compaction_end" => {
                note_compaction(&mut self.notices, frame);
                // Compacting is a model call of its own, and the reply comes after it.
                self.wait_epoch = self.wait_epoch.wrapping_add(1);
                self.wait_delay_ms = 0;
            }
            _ => {}
        }
    }

    fn apply_runner(&mut self, frame: &RunnerFrame) {
        match frame {
            RunnerFrame::Hello { .. } => {}
            RunnerFrame::Notice { kind, message } => self.notices.push(Notice {
                kind: kind.clone(),
                message: message.clone(),
            }),
            RunnerFrame::Evidence(evidence) => self.evidence.push(evidence.clone()),
            RunnerFrame::DiffResult {
                base,
                from,
                to,
                stat,
                patch,
                truncated,
                ..
            } => {
                let diff = Diff {
                    base: base.clone(),
                    stat: stat.clone(),
                    patch: patch.clone(),
                    truncated: *truncated,
                };
                if from.is_none() && to.is_none() {
                    self.last_diff = Some(diff);
                } else {
                    self.turn_diff = Some(TurnDiff {
                        from: from.unwrap_or(0),
                        to: *to,
                        diff,
                    });
                }
            }
            RunnerFrame::Checkpoints { checkpoints } => {
                let mut list = Vec::new();
                for checkpoint in checkpoints {
                    checkpoints::add(&mut list, checkpoint);
                }
                self.checkpoints = Some(list);
            }
            RunnerFrame::Checkpointed { checkpoint, .. } => {
                if let Some(list) = &mut self.checkpoints {
                    checkpoints::add(list, checkpoint);
                }
            }
            RunnerFrame::Restored { turn, .. } => {
                // Both diffs describe a tree that is gone.
                self.last_diff = None;
                self.turn_diff = None;
                let list = self.checkpoints.as_deref().unwrap_or_default();
                checkpoints::note_restore(&mut self.notices, list, *turn);
            }
            RunnerFrame::Stopped { .. } => self.omp_ended(),
            RunnerFrame::OmpExited {
                exit_code,
                stderr_tail,
            } => {
                let expected = self.exit_expected;
                self.omp_ended();
                if !expected {
                    let message = exit_message(*exit_code, stderr_tail);
                    self.fail(OMP_STAGE, FailureKind::Permanent, message);
                }
            }
            RunnerFrame::Landed {
                branch,
                commit,
                pull_request_url,
                ..
            } => {
                self.pending.clear();
                self.failure = None;
                self.phase = Phase::Landed;
                self.landing = Some(Landing {
                    branch: branch.clone(),
                    commit: commit.clone(),
                    pull_request_url: pull_request_url.clone(),
                });
            }
            RunnerFrame::Error { stage, message, .. } => {
                let problem = Failure {
                    stage: stage.clone(),
                    kind: FailureKind::Permanent,
                    message: message.clone(),
                };
                // A line sent just before omp died is dropped, which says nothing new and must
                // not replace why omp ended.
                let after_omp = self.omp_gone && stage == PROTOCOL_STAGE;
                if after_omp || NON_FATAL_STAGES.contains(&stage.as_str()) {
                    // A save that fails the same way after every run is shown once.
                    if !self.warnings.contains(&problem) {
                        self.warnings.push(problem);
                    }
                } else {
                    self.fail_with(problem);
                }
            }
        }
    }

    fn on_response(&mut self, frame: &Value) {
        let command = frame.get("command").and_then(Value::as_str);
        if !command.is_some_and(|c| PROMPT_COMMANDS.contains(&c)) {
            return;
        }
        if frame.get("success").and_then(Value::as_bool) == Some(true) {
            self.run();
        } else {
            let message = omp_error(frame.get("error"), "omp rejected the prompt");
            self.fail(PROMPT_STAGE, FailureKind::Permanent, message);
        }
    }

    fn on_message(&mut self, frame: &Value) {
        self.heard = true;
        let event = &frame["assistantMessageEvent"];
        if event["type"] == "text_delta"
            && let Some(delta) = event["delta"].as_str()
        {
            self.reply.push_str(delta);
        }
    }

    // omp sends the failed attempt's `message_end`, with `stopReason: "error"`, before it says it
    // will retry, so that text is not kept.
    fn on_message_end(&mut self, frame: &Value) {
        let message = &frame["message"];
        if message["stopReason"] != "error" {
            self.reply_mark = self.reply.len();
            // omp also ends the prompt's own message, which is not the model speaking.
            if message["role"] == "assistant" {
                self.heard = true;
            }
        }
    }

    fn on_retry(&mut self, frame: &Value) {
        if self.reply.is_char_boundary(self.reply_mark) {
            self.reply.truncate(self.reply_mark);
        }
        self.retrying = Some(Retrying::from_frame(frame));
        self.restart_wait(frame["delayMs"].as_u64().unwrap_or(0));
    }

    fn restart_wait(&mut self, delay_ms: u64) {
        self.heard = false;
        self.wait_epoch = self.wait_epoch.wrapping_add(1);
        self.wait_delay_ms = delay_ms;
    }

    fn on_tool_start(&mut self, frame: &Value) {
        self.heard = true;
        let Some(id) = frame["toolCallId"].as_str() else {
            return;
        };
        if self.tool_calls.iter().all(|call| call.id != id) {
            self.tool_calls.push(ToolCall {
                id: id.to_owned(),
                name: frame["toolName"].as_str().unwrap_or_default().to_owned(),
                status: ToolStatus::Running,
            });
        }
    }

    fn on_tool_end(&mut self, frame: &Value) {
        let Some(id) = frame["toolCallId"].as_str() else {
            return;
        };
        let status = if frame["isError"] == false {
            ToolStatus::Succeeded
        } else {
            ToolStatus::Failed
        };
        match self.tool_calls.iter_mut().find(|call| call.id == id) {
            Some(call) => call.status = status,
            None => self.tool_calls.push(ToolCall {
                id: id.to_owned(),
                name: frame["toolName"].as_str().unwrap_or_default().to_owned(),
                status,
            }),
        }
    }

    fn on_prompt_result(&mut self, frame: &Value) {
        self.retrying = None;
        if !self.is_live() {
            return;
        }
        match frame["status"].as_str() {
            Some("completed" | "aborted") => {
                // When false, omp still has background work and sends `session_settled` later.
                if frame["sessionSettled"] == true {
                    self.phase = Phase::Review;
                }
            }
            Some("error") => {
                let error = frame.get("error");
                let message = omp_error(error, "the prompt failed");
                self.fail(PROMPT_STAGE, prompt_error_kind(error), message);
            }
            other => {
                let status = other.unwrap_or("none");
                let message = format!("omp ended the prompt with an unknown status: {status}");
                self.fail(PROMPT_STAGE, FailureKind::Permanent, message);
            }
        }
    }

    fn on_ui_request(&mut self, frame: &Value) {
        if let Some(target) = withdrawn_approval(frame) {
            self.pending.retain(|r| r.id() != target);
        } else if let Some(request) = ApprovalRequest::parse(frame) {
            self.pending.retain(|r| r.id() != request.id());
            self.pending.push(request);
        }
    }

    fn is_live(&self) -> bool {
        !matches!(self.phase, Phase::Failed | Phase::Landed)
    }

    fn run(&mut self) {
        if self.is_live() {
            self.phase = Phase::Running;
        }
    }

    fn omp_ended(&mut self) {
        self.omp_gone = true;
        self.retrying = None;
        // omp is gone, so nothing is waiting for these answers any more.
        self.pending.clear();
        if self.is_live() {
            self.phase = Phase::Review;
        }
    }

    fn fail(&mut self, stage: &str, kind: FailureKind, message: String) {
        self.fail_with(Failure {
            stage: stage.to_owned(),
            kind,
            message,
        });
    }

    fn fail_with(&mut self, failure: Failure) {
        self.retrying = None;
        if self.phase != Phase::Landed {
            self.phase = Phase::Failed;
            self.failure = Some(failure);
        }
    }

    fn clear_failure(&mut self, stage: &str) {
        if self.phase == Phase::Failed && self.failure.as_ref().is_some_and(|f| f.stage == stage) {
            self.failure = None;
            self.phase = Phase::Review;
        }
    }
}

/// omp's error text, which is a string on a `response` and an object with `message` on a
/// `prompt_result`.
fn omp_error(error: Option<&Value>, fallback: &str) -> String {
    let text = match error {
        Some(Value::String(text)) => Some(text.as_str()),
        Some(Value::Object(fields)) => fields.get("message").and_then(Value::as_str),
        _ => None,
    };
    text.filter(|t| !t.trim().is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn exit_message(exit_code: Option<i32>, stderr_tail: &[String]) -> String {
    let mut message = match exit_code {
        Some(code) => format!("omp exited with code {code}"),
        None => "omp ended without an exit code".to_owned(),
    };
    if !stderr_tail.is_empty() {
        message.push_str(":\n");
        message.push_str(&stderr_tail.join("\n"));
    }
    message
}

#[cfg(test)]
mod tests;
