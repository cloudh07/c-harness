//! The session reducer: omp and runner frames, plus what the app sent, folded into one state.
//!
//! Lines go in through [`Session::apply_line`] in the order they were read, and the app reports
//! each frame it writes through [`Session::apply_action`]. Nothing here reads a clock or waits,
//! so the same lines always give the same state.

use ch_protocol::runner::{Evidence, RunnerFrame, RunnerLine};
use serde_json::Value;

use crate::approval::{ApprovalRequest, withdrawn_approval};
use crate::context::note_compaction;

const PROMPT_STAGE: &str = "prompt";
const OMP_STAGE: &str = "omp";
const LAND_STAGE: &str = "land";
// A check that fails or a diff that cannot be built is news about the work, not a broken session.
const NON_FATAL_STAGES: &[&str] = &["verify", "diff"];
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
/// or failed a prompt, and `omp` when omp exited without being asked to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub stage: String,
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

/// Where the work landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    pub branch: String,
    pub commit: String,
    pub pull_request_url: Option<String>,
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
    notices: Vec<Notice>,
    warnings: Vec<Failure>,
    failure: Option<Failure>,
    landing: Option<Landing>,
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

    pub fn pull_request_url(&self) -> Option<&str> {
        self.landing.as_ref()?.pull_request_url.as_deref()
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
                if self.is_live() {
                    self.phase = Phase::Running;
                    self.reply.clear();
                }
            }
            ClientAction::ApprovalAnswered { id } => self.pending.retain(|r| r.id() != id),
            ClientAction::StopSent => self.exit_expected = true,
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
            "message_update" => self.on_message(frame),
            "tool_execution_start" => self.on_tool_start(frame),
            "tool_execution_end" => self.on_tool_end(frame),
            "prompt_result" => self.on_prompt_result(frame),
            "session_settled" if self.phase == Phase::Running => self.phase = Phase::Review,
            "extension_ui_request" => self.on_ui_request(frame),
            "auto_compaction_start" | "auto_compaction_end" => {
                note_compaction(&mut self.notices, frame);
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
                stat,
                patch,
                truncated,
                ..
            } => {
                self.last_diff = Some(Diff {
                    base: base.clone(),
                    stat: stat.clone(),
                    patch: patch.clone(),
                    truncated: *truncated,
                });
            }
            RunnerFrame::Stopped { .. } => self.omp_ended(),
            RunnerFrame::OmpExited {
                exit_code,
                stderr_tail,
            } => {
                let expected = self.exit_expected;
                self.omp_ended();
                if !expected {
                    self.fail(OMP_STAGE, exit_message(*exit_code, stderr_tail));
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
                    message: message.clone(),
                };
                // A line sent just before omp died is dropped, which says nothing new and must
                // not replace why omp ended.
                let after_omp = self.omp_gone && stage == PROTOCOL_STAGE;
                if after_omp || NON_FATAL_STAGES.contains(&stage.as_str()) {
                    self.warnings.push(problem);
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
            self.fail(PROMPT_STAGE, message);
        }
    }

    fn on_message(&mut self, frame: &Value) {
        let event = &frame["assistantMessageEvent"];
        if event["type"] == "text_delta"
            && let Some(delta) = event["delta"].as_str()
        {
            self.reply.push_str(delta);
        }
    }

    fn on_tool_start(&mut self, frame: &Value) {
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
                let message = omp_error(frame.get("error"), "the prompt failed");
                self.fail(PROMPT_STAGE, message);
            }
            other => {
                let status = other.unwrap_or("none");
                let message = format!("omp ended the prompt with an unknown status: {status}");
                self.fail(PROMPT_STAGE, message);
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
        // omp is gone, so nothing is waiting for these answers any more.
        self.pending.clear();
        if self.is_live() {
            self.phase = Phase::Review;
        }
    }

    fn fail(&mut self, stage: &str, message: String) {
        self.fail_with(Failure {
            stage: stage.to_owned(),
            message,
        });
    }

    fn fail_with(&mut self, failure: Failure) {
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
mod tests {
    use ch_protocol::runner::route_runner_line;

    use super::*;

    const FIXTURE: &str = include_str!("../../../testdata/omp/approval-session.jsonl");
    const APPROVAL: &str = r#"{"type":"extension_ui_request","id":"ui_1","method":"select","title":"Allow tool: bash\nCommand: ls","options":["Approve","Deny"]}"#;

    fn feed(session: &mut Session, line: &str) {
        let line = route_runner_line(line).expect("a routable line");
        session.apply_line(&line);
    }

    fn running() -> Session {
        let mut session = Session::new();
        session.apply_action(&ClientAction::StartSent);
        feed(&mut session, r#"{"type":"ready"}"#);
        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Running);
        session
    }

    fn failure(stage: &str, message: &str) -> Failure {
        Failure {
            stage: stage.to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn follows_the_fixture_session_line_by_line() {
        use SessionState::{Idle, NeedsReview, NeedsYou, Running};
        let expected = [
            Idle,
            Running,
            Running,
            Running,
            NeedsYou,
            Running,
            Running,
            Running,
            Running,
            Running,
            Running,
            NeedsReview,
        ];
        assert_eq!(FIXTURE.lines().count(), expected.len());

        let mut session = Session::new();
        assert_eq!(session.state(), Idle);
        session.apply_action(&ClientAction::StartSent);
        assert_eq!(session.state(), SessionState::Starting);
        for (n, (line, want)) in FIXTURE.lines().zip(expected).enumerate() {
            match n {
                1 => session.apply_action(&ClientAction::PromptSent),
                5 => {
                    assert_eq!(session.pending_approvals()[0].id(), "ui_1");
                    let id = "ui_1".to_owned();
                    session.apply_action(&ClientAction::ApprovalAnswered { id });
                }
                _ => {}
            }
            feed(&mut session, line);
            assert_eq!(session.state(), want, "after fixture line {}", n + 1);
        }

        assert_eq!(
            session.reply(),
            "I'll run the tests first.All three tests pass."
        );
        assert_eq!(
            session.tool_calls(),
            [ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                status: ToolStatus::Succeeded,
            }]
        );
        assert_eq!(session.pending_approvals(), []);
        assert_eq!(session.failure(), None);
    }

    #[test]
    fn an_unyielded_agent_end_keeps_the_run_going() {
        let mut session = running();
        for line in [
            r#"{"type":"agent_start"}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"a"}}"#,
            r#"{"type":"agent_end","messages":[],"yielded":false}"#,
            r#"{"type":"agent_start"}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"b"}}"#,
            r#"{"type":"agent_end","messages":[],"yielded":true}"#,
        ] {
            feed(&mut session, line);
            assert_eq!(session.state(), SessionState::Running, "{line}");
        }
        feed(
            &mut session,
            r#"{"id":"req_1","type":"prompt_result","status":"completed","sessionSettled":true}"#,
        );
        assert_eq!(session.state(), SessionState::NeedsReview);
        assert_eq!(session.reply(), "ab");

        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.reply(), "", "a new prompt starts a new reply");
    }

    #[test]
    fn a_cancel_withdraws_only_its_own_approval() {
        let mut session = running();
        feed(&mut session, APPROVAL);
        feed(&mut session, APPROVAL);
        assert_eq!(session.pending_approvals().len(), 1, "same id replaces");
        assert_eq!(session.state(), SessionState::NeedsYou);

        feed(
            &mut session,
            r#"{"type":"extension_ui_request","id":"ui_8","method":"cancel","targetId":"ui_7"}"#,
        );
        assert_eq!(session.state(), SessionState::NeedsYou);

        feed(
            &mut session,
            r#"{"type":"extension_ui_request","id":"ui_9","method":"cancel","targetId":"ui_1"}"#,
        );
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.pending_approvals(), []);
    }

    #[test]
    fn non_approval_dialogs_do_not_wait_on_the_person() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"type":"extension_ui_request","id":"ui_2","method":"select","title":"Pick a model","options":["a","b"]}"#,
        );
        assert_eq!(session.state(), SessionState::Running);
    }

    #[test]
    fn a_rejected_or_failed_prompt_fails_until_the_next_prompt() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"id":"req_1","type":"response","command":"prompt","success":false,"error":"busy"}"#,
        );
        assert_eq!(session.state(), SessionState::Failed);
        assert_eq!(session.failure(), Some(&failure("prompt", "busy")));

        feed(&mut session, r#"{"type":"session_settled"}"#);
        assert_eq!(session.state(), SessionState::Failed);

        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.failure(), None);

        feed(
            &mut session,
            r#"{"id":"req_2","type":"prompt_result","status":"error","error":{"message":"rate limited","retryable":true},"sessionSettled":true}"#,
        );
        assert_eq!(session.failure(), Some(&failure("prompt", "rate limited")));

        session.apply_action(&ClientAction::PromptSent);
        feed(
            &mut session,
            r#"{"id":"req_3","type":"prompt_result","status":"exploded","sessionSettled":true}"#,
        );
        assert_eq!(session.state(), SessionState::Failed);
    }

    #[test]
    fn a_failed_command_other_than_prompt_is_ignored() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"id":"req_4","type":"response","command":"set_model","success":false,"error":"no"}"#,
        );
        assert_eq!(session.state(), SessionState::Running);
    }

    #[test]
    fn omp_exiting_unasked_fails_and_drops_approvals() {
        let mut session = running();
        feed(&mut session, APPROVAL);
        feed(
            &mut session,
            r#"{"type":"ch_omp_exited","exit_code":1,"stderr_tail":["panic: out of memory"]}"#,
        );
        assert_eq!(session.state(), SessionState::Failed);
        assert_eq!(
            session.failure(),
            Some(&failure(
                "omp",
                "omp exited with code 1:\npanic: out of memory"
            ))
        );
        assert_eq!(session.pending_approvals(), []);

        feed(&mut session, r#"{"type":"agent_start"}"#);
        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Failed, "omp is gone");

        session.apply_action(&ClientAction::StartSent);
        assert_eq!(session, {
            let mut fresh = Session::new();
            fresh.apply_action(&ClientAction::StartSent);
            fresh
        });
    }

    #[test]
    fn a_line_dropped_after_omp_died_keeps_the_real_failure() {
        let dropped = r#"{"type":"ch_error","stage":"protocol","message":"omp is not running, so a line for it was dropped"}"#;
        let mut crashed = running();
        feed(
            &mut crashed,
            r#"{"type":"ch_omp_exited","exit_code":1,"stderr_tail":["panic"]}"#,
        );
        feed(&mut crashed, dropped);
        assert_eq!(crashed.failure().map(|f| f.stage.as_str()), Some("omp"));
        assert_eq!(crashed.warnings().len(), 1);

        let mut stopped = running();
        stopped.apply_action(&ClientAction::StopSent);
        feed(
            &mut stopped,
            r#"{"type":"ch_stopped","id":"s1","exit_code":0}"#,
        );
        feed(&mut stopped, dropped);
        assert_eq!(stopped.state(), SessionState::NeedsReview);

        // While omp runs, a protocol error still means the session is broken.
        let mut live = running();
        feed(&mut live, dropped);
        assert_eq!(live.state(), SessionState::Failed);
    }

    #[test]
    fn stopping_leads_to_review() {
        let mut stopped = running();
        feed(&mut stopped, APPROVAL);
        stopped.apply_action(&ClientAction::StopSent);
        assert_eq!(stopped.state(), SessionState::NeedsYou);
        feed(
            &mut stopped,
            r#"{"type":"ch_stopped","id":"s1","exit_code":0}"#,
        );
        assert_eq!(stopped.state(), SessionState::NeedsReview);
        assert_eq!(stopped.pending_approvals(), []);

        let mut exited = running();
        exited.apply_action(&ClientAction::StopSent);
        feed(
            &mut exited,
            r#"{"type":"ch_omp_exited","exit_code":null,"stderr_tail":[]}"#,
        );
        assert_eq!(exited.state(), SessionState::NeedsReview);
        exited.apply_action(&ClientAction::PromptSent);
        assert_eq!(exited.state(), SessionState::NeedsReview, "omp is gone");
    }

    #[test]
    fn verify_and_diff_errors_are_warnings_and_others_fail() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"type":"ch_error","id":"v1","stage":"verify","message":"command not found"}"#,
        );
        feed(
            &mut session,
            r#"{"type":"ch_error","id":"d1","stage":"diff","message":"no HEAD"}"#,
        );
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.warnings().len(), 2);

        feed(
            &mut session,
            r#"{"type":"ch_error","stage":"start","message":"omp not found"}"#,
        );
        assert_eq!(session.state(), SessionState::Failed);
        assert_eq!(session.failure(), Some(&failure("start", "omp not found")));
        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Failed);
    }

    #[test]
    fn compaction_keeps_one_notice_and_the_state() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"type":"ch_notice","kind":"k","message":"m"}"#,
        );
        feed(
            &mut session,
            r#"{"type":"auto_compaction_start","reason":"threshold"}"#,
        );
        feed(
            &mut session,
            r#"{"type":"auto_compaction_end","aborted":false}"#,
        );
        assert_eq!(session.state(), SessionState::Running);
        let kinds: Vec<_> = session.notices().iter().map(|n| n.kind.as_str()).collect();
        assert_eq!(kinds, ["k", crate::COMPACTION_NOTICE]);
        assert_eq!(session.notices()[1].message, "Context compacted.");
    }

    #[test]
    fn records_evidence_diffs_and_notices() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"type":"ch_notice","kind":"project_config","message":"found .omp"}"#,
        );
        feed(
            &mut session,
            r#"{"type":"ch_evidence","id":"v1","command":"cargo test","exit_code":0,"duration_ms":1200,"output_tail":"ok","truncated":false,"timed_out":false}"#,
        );
        feed(
            &mut session,
            r#"{"type":"ch_diff_result","id":"d1","base":"abc","stat":"1 file","patch":"+x","truncated":false}"#,
        );
        feed(
            &mut session,
            r#"{"type":"ch_diff_result","id":"d2","base":"abc","stat":"2 files","patch":"+y","truncated":true}"#,
        );
        assert_eq!(session.notices()[0].kind, "project_config");
        assert!(session.evidence()[0].passed());
        assert_eq!(
            session.last_diff(),
            Some(&Diff {
                base: "abc".into(),
                stat: "2 files".into(),
                patch: "+y".into(),
                truncated: true,
            })
        );
        assert_eq!(session.state(), SessionState::Running);
    }

    #[test]
    fn a_tool_call_without_a_clear_success_counts_as_failed() {
        let mut session = running();
        for line in [
            r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"bash"}"#,
            r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","isError":true}"#,
            r#"{"type":"tool_execution_end","toolCallId":"c2","toolName":"edit"}"#,
            r#"{"type":"tool_execution_start","toolCallId":"c3","toolName":"read"}"#,
        ] {
            feed(&mut session, line);
        }
        let statuses: Vec<_> = session.tool_calls().iter().map(|c| c.status).collect();
        assert_eq!(
            statuses,
            [ToolStatus::Failed, ToolStatus::Failed, ToolStatus::Running]
        );
    }

    #[test]
    fn a_failed_land_can_be_retried_and_landing_is_final() {
        let mut session = running();
        feed(
            &mut session,
            r#"{"id":"req_1","type":"prompt_result","status":"completed","sessionSettled":true}"#,
        );
        session.apply_action(&ClientAction::LandSent);
        feed(
            &mut session,
            r#"{"type":"ch_stopped","id":"l1","exit_code":0}"#,
        );
        feed(
            &mut session,
            r#"{"type":"ch_error","id":"l1","stage":"land","message":"push rejected"}"#,
        );
        assert_eq!(session.state(), SessionState::Failed);

        session.apply_action(&ClientAction::LandSent);
        assert_eq!(session.state(), SessionState::NeedsReview);
        feed(
            &mut session,
            r#"{"type":"ch_landed","id":"l2","branch":"ch/fix","commit":"abc123","pull_request_url":"https://github.com/octo/repo/pull/7"}"#,
        );
        assert_eq!(session.state(), SessionState::Landed);
        assert_eq!(
            session.pull_request_url(),
            Some("https://github.com/octo/repo/pull/7")
        );

        feed(&mut session, APPROVAL);
        feed(
            &mut session,
            r#"{"type":"ch_error","stage":"start","message":"late"}"#,
        );
        feed(&mut session, r#"{"type":"agent_start"}"#);
        assert_eq!(session.state(), SessionState::Landed);
        assert_eq!(session.failure(), None);
    }

    #[test]
    fn a_land_refused_while_omp_runs_leaves_omp_usable() {
        let mut session = running();
        session.apply_action(&ClientAction::LandSent);
        feed(
            &mut session,
            r#"{"type":"ch_error","id":"l1","stage":"land","message":"the agent is still running"}"#,
        );
        assert_eq!(session.state(), SessionState::Failed);

        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.failure(), None);

        session.apply_action(&ClientAction::StopSent);
        feed(
            &mut session,
            r#"{"type":"ch_stopped","id":"s1","exit_code":0}"#,
        );
        feed(
            &mut session,
            r#"{"type":"ch_error","id":"l2","stage":"land","message":"push rejected"}"#,
        );
        session.apply_action(&ClientAction::PromptSent);
        assert_eq!(session.state(), SessionState::Failed, "omp is gone");
    }
}
