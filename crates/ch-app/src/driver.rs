//! One session's traffic with its runner, kept out of the window so it can be tested.
//!
//! The driver owns the [`Link`] and the [`Session`] reducer. Every frame the app writes goes
//! through a method here, which sends it, adds it to the transcript and reports it to the
//! reducer, in that order and only once the link took it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use ch_core::{
    ApprovalDecision, ApprovalRequest, ClientAction, ContextMeter, ContextState, STATS_COMMAND,
    Session, SessionState, Transcript, Usage, redact_secrets,
};
use ch_protocol::runner::{ClientFrame, LandRequest, RunnerFrame, RunnerLine, StartSession};
use serde_json::{Value, json};

use crate::link::{Link, LinkEvent};

mod checkpoints;
mod history;

use history::Earlier;
pub use history::EarlierTurns;

const OMP_ARGS: [&str; 2] = ["--mode", "rpc"];
const PUMP_LIMIT: usize = 256;
const LOG_LIMIT: usize = 200;
const RECORD_LIMIT: usize = 50_000;
/// omp dialogs that wait for an answer the app has no screen for. omp reads a cancel as no.
const UNANSWERABLE_DIALOGS: &[&str] = &["confirm", "input", "editor", "ask"];
/// How much of an unknown request id a log line repeats.
const SHOWN_ID_CHARS: usize = 40;

pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Where omp is in its life, as far as this side knows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OmpState {
    Running,
    StopSent,
    Ended,
}

/// A request sent to the runner that its reply must name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Verify,
    /// The full diff from the base to the working tree.
    Diff,
    /// A diff between checkpoints, whose reply must echo the same range.
    TurnDiff {
        from: u32,
        to: Option<u32>,
    },
    Checkpoint,
    Restore,
    Stop,
    Land,
}

impl Request {
    fn label(self) -> &'static str {
        match self {
            Self::Verify => "verify",
            Self::Diff | Self::TurnDiff { .. } => "diff",
            Self::Checkpoint => "checkpoint",
            Self::Restore => "restore",
            Self::Stop => "stop",
            Self::Land => "land",
        }
    }

    /// The request a diff reply answers, by the range it echoes.
    fn for_diff(from: Option<u32>, to: Option<u32>) -> Self {
        match (from, to) {
            (None, None) => Self::Diff,
            (from, to) => Self::TurnDiff {
                from: from.unwrap_or(0),
                to,
            },
        }
    }
}

pub struct Driver {
    link: Link,
    session: Session,
    transcript: Transcript,
    records: Vec<String>,
    records_dropped: bool,
    log: VecDeque<String>,
    clock: fn() -> u64,
    next_id: u64,
    /// Requests the runner has not answered yet, by id.
    open: HashMap<String, Request>,
    queued_prompt: Option<String>,
    queued_land: Option<LandRequest>,
    land_in_flight: bool,
    omp: OmpState,
    closed: bool,
    usage: Usage,
    context: ContextMeter,
    earlier: Earlier,
    omp_session_file: Option<String>,
    saves: checkpoints::Saves,
}

impl Driver {
    pub fn new(link: Link, clock: fn() -> u64) -> Self {
        Self {
            link,
            session: Session::new(),
            transcript: Transcript::new(),
            records: Vec::new(),
            records_dropped: false,
            log: VecDeque::new(),
            clock,
            next_id: 0,
            open: HashMap::new(),
            queued_prompt: None,
            queued_land: None,
            land_in_flight: false,
            omp: OmpState::Running,
            closed: false,
            usage: Usage::default(),
            context: ContextMeter::default(),
            earlier: Earlier::Off,
            omp_session_file: None,
            saves: checkpoints::Saves::default(),
        }
    }

    /// Tokens and cost per model turn so far.
    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// How full omp's context window was at its last answer.
    pub fn context(&self) -> ContextState {
        self.context.state()
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Runner stderr and stream problems, oldest first, with secrets masked.
    pub fn log(&self) -> impl Iterator<Item = &str> {
        self.log.iter().map(String::as_str)
    }

    /// The runner's output has ended. Nothing sent from now on reaches it.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// A stop or land was asked for and omp has not ended yet.
    pub fn stopping(&self) -> bool {
        let asked = self.omp == OmpState::StopSent || self.queued_land.is_some();
        asked && self.omp != OmpState::Ended && !self.closed
    }

    /// A land is waiting for omp to stop, or was sent and has no answer yet.
    pub fn landing(&self) -> bool {
        self.queued_land.is_some() || self.land_in_flight
    }

    /// Whether a prompt sent now would start a run. A restore or the save of the last run
    /// must finish first, or the agent would work on a tree that is about to change.
    pub fn can_prompt(&self) -> bool {
        if self.closed || self.omp != OmpState::Running || self.loading_history() {
            return false;
        }
        if self.restoring() || self.saving_checkpoint() {
            return false;
        }
        match self.session.state() {
            SessionState::Idle | SessionState::NeedsReview => true,
            SessionState::Failed => self.session.failure().is_some_and(|f| f.stage == "prompt"),
            _ => false,
        }
    }

    /// Whether verify, diff and land can be asked for.
    pub fn can_review(&self) -> bool {
        !self.closed
            && matches!(
                self.session.state(),
                SessionState::NeedsReview | SessionState::Idle
            )
            && !self.landing()
            && !self.restoring()
    }

    /// Whether a land can go out now: once the work is ready for review, and again after a land
    /// that failed, since a rejected push is fixed by changing settings and trying again.
    pub fn can_land(&self) -> bool {
        if self.closed || self.landing() || self.restoring() {
            return false;
        }
        match self.session.state() {
            SessionState::NeedsReview | SessionState::Idle => true,
            SessionState::Failed => self.session.failure().is_some_and(|f| f.stage == "land"),
            _ => false,
        }
    }

    /// Starts omp. `prompt` is sent as soon as omp says it is ready.
    pub fn start(
        &mut self,
        session: &str,
        extra_args: Vec<String>,
        env: BTreeMap<String, String>,
        prompt: String,
    ) -> bool {
        let mut omp_args: Vec<String> = OMP_ARGS.iter().map(|s| (*s).to_string()).collect();
        omp_args.extend(extra_args);
        let frame = ClientFrame::Start(StartSession {
            session: session.to_string(),
            omp_args,
            config_overlay: None,
            env,
        });
        let sent = self.send_client(&frame, Some(ClientAction::StartSent));
        if sent && !prompt.trim().is_empty() {
            self.queued_prompt = Some(prompt);
        }
        sent
    }

    pub fn prompt(&mut self, text: &str) -> bool {
        if text.trim().is_empty() || !self.can_prompt() {
            return false;
        }
        let id = self.next_id();
        self.send_omp(
            &json!({ "id": id, "type": "prompt", "message": text }),
            Some(ClientAction::PromptSent),
        )
    }

    pub fn answer(&mut self, id: &str, decision: ApprovalDecision) -> Result<(), String> {
        let reply = self
            .session
            .pending_approvals()
            .iter()
            .find(|r| r.id() == id)
            .ok_or_else(|| "That approval is no longer open.".to_string())?
            .reply(decision)
            .map_err(|e| e.to_string())?;
        let answered = ClientAction::ApprovalAnswered { id: id.to_string() };
        if self.send_omp(&reply, Some(answered)) {
            Ok(())
        } else {
            Err("The runner is gone, so the answer was not sent.".to_string())
        }
    }

    /// Asks the runner to run `command`. Refused while the agent works, since a check must
    /// measure work that is standing still.
    pub fn verify(&mut self, command: &str) -> bool {
        if !self.can_review() {
            return false;
        }
        let id = self.next_id();
        let frame = ClientFrame::Verify {
            id: id.clone(),
            command: command.to_string(),
            timeout_secs: None,
        };
        self.send_request(id, Request::Verify, &frame, None)
    }

    pub fn diff(&mut self) -> bool {
        let id = self.next_id();
        let frame = ClientFrame::Diff {
            id: id.clone(),
            from: None,
            to: None,
        };
        self.send_request(id, Request::Diff, &frame, None)
    }

    pub fn stop(&mut self) -> bool {
        if self.omp != OmpState::Running {
            return true;
        }
        let id = self.next_id();
        let frame = ClientFrame::Stop { id: id.clone() };
        let sent = self.send_request(id, Request::Stop, &frame, Some(ClientAction::StopSent));
        if sent {
            self.omp = OmpState::StopSent;
        }
        sent
    }

    /// Lands the work. The runner refuses a land while omp runs, so omp is stopped first and the
    /// land goes out once the runner says it ended. The request's `id` is replaced when it is
    /// sent. Refused unless [`Self::can_land`], whoever asks.
    pub fn land(&mut self, request: LandRequest) -> bool {
        if !self.can_land() {
            return false;
        }
        if self.omp == OmpState::Ended {
            return self.send_land(request);
        }
        if !self.stop() {
            return false;
        }
        self.queued_land = Some(request);
        true
    }

    /// Folds in what arrived since the last call. Returns whether anything did.
    pub fn pump(&mut self) -> bool {
        let events = self.link.drain(PUMP_LIMIT);
        let changed = !events.is_empty();
        for event in events {
            self.on_event(event);
        }
        self.flush_queued();
        self.page_history();
        changed
    }

    /// The transcript as JSON lines, secrets removed.
    pub fn transcript(&self) -> String {
        let mut out = self.records.join("\n");
        out.push('\n');
        out
    }

    pub fn close(&mut self) {
        self.link.close_stdin();
    }

    fn on_event(&mut self, event: LinkEvent) {
        match event {
            LinkEvent::Line(line) => self.on_line(&line),
            LinkEvent::Problem(message) => self.note(&message),
            LinkEvent::Stderr(text) => self.note(&text),
            LinkEvent::Closed => {
                self.closed = true;
                self.queued_prompt = None;
                self.queued_land = None;
                self.land_in_flight = false;
                self.note("The runner closed its output.");
            }
        }
    }

    fn on_line(&mut self, line: &RunnerLine) {
        let answered = match line {
            RunnerLine::Runner(frame) => match self.take_request(frame) {
                Ok(answered) => answered,
                Err(problem) => {
                    self.note(&problem);
                    return;
                }
            },
            RunnerLine::Omp(_) => None,
        };
        let record = self.transcript.incoming((self.clock)(), line);
        self.keep(record);
        let before = self.session.state();
        self.session.apply_line(line);
        match line {
            RunnerLine::Runner(RunnerFrame::Stopped { .. } | RunnerFrame::OmpExited { .. }) => {
                self.omp = OmpState::Ended;
                self.queued_prompt = None;
                self.context.omp_ended();
            }
            RunnerLine::Omp(frame) => {
                self.usage.on_omp((self.clock)(), frame);
                if self.context.on_omp(frame) {
                    self.ask_context();
                }
                self.on_resume_frame(frame);
                self.cancel_unanswerable(frame);
            }
            RunnerLine::Runner(_) => {}
        }
        if let (Some(kind), RunnerLine::Runner(frame)) = (answered, line) {
            match kind {
                Request::Land => self.land_in_flight = false,
                Request::Checkpoint | Request::Restore => self.on_checkpoint_answer(kind, frame),
                Request::Verify | Request::Diff | Request::TurnDiff { .. } | Request::Stop => {}
            }
        }
        self.save_after_run(before);
    }

    /// Takes the request a runner reply answers off the open list. A reply that names no open
    /// request of its kind is refused, and the request it names stays open for the real answer.
    /// An error answers whatever request it names, since a malformed request comes back as a
    /// protocol error.
    fn take_request(&mut self, frame: &RunnerFrame) -> Result<Option<Request>, String> {
        let (id, expected) = match frame {
            RunnerFrame::Evidence(evidence) => (&evidence.id, Some(Request::Verify)),
            RunnerFrame::DiffResult { id, from, to, .. } => {
                (id, Some(Request::for_diff(*from, *to)))
            }
            // A restore saves the tree it replaces first, under its own id, and stays open
            // until its `ch_restored` or error.
            RunnerFrame::Checkpointed { id, .. }
                if self.open.get(id) == Some(&Request::Restore) =>
            {
                return Ok(None);
            }
            RunnerFrame::Checkpointed { id, .. } => (id, Some(Request::Checkpoint)),
            RunnerFrame::Restored { id, .. } => (id, Some(Request::Restore)),
            RunnerFrame::Stopped { id, .. } => (id, Some(Request::Stop)),
            RunnerFrame::Landed { id, .. } => (id, Some(Request::Land)),
            RunnerFrame::Error { id: Some(id), .. } => (id, None),
            RunnerFrame::Hello { .. }
            | RunnerFrame::Notice { .. }
            | RunnerFrame::Checkpoints { .. }
            | RunnerFrame::OmpExited { .. }
            | RunnerFrame::Error { id: None, .. } => return Ok(None),
        };
        match self.open.get(id) {
            Some(open) if expected.is_none_or(|kind| kind == *open) => Ok(self.open.remove(id)),
            _ => {
                let kind = expected.map_or("error", Request::label);
                let shown: String = id.chars().take(SHOWN_ID_CHARS).collect();
                Err(format!(
                    "Dropped a {kind} reply for request {shown:?}, which is not an open {kind} request."
                ))
            }
        }
    }

    fn cancel_unanswerable(&mut self, frame: &Value) {
        if frame.get("type").and_then(Value::as_str) != Some("extension_ui_request") {
            return;
        }
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return;
        };
        let Some(id) = frame.get("id").and_then(Value::as_str) else {
            return;
        };
        // A select that is not a tool approval has no screen either, and omp would wait on it.
        let unanswerable = UNANSWERABLE_DIALOGS.contains(&method)
            || (method == "select" && ApprovalRequest::parse(frame).is_none());
        if !unanswerable {
            return;
        }
        let reply = json!({ "type": "extension_ui_response", "id": id, "cancelled": true });
        if self.send_omp(&reply, None) {
            self.note(&format!(
                "omp asked for a {method} dialog, which C-Harness cannot show yet. It was answered no."
            ));
        }
    }

    fn flush_queued(&mut self) {
        if self.closed {
            return;
        }
        match self.session.state() {
            SessionState::Idle => {
                if let Some(prompt) = self.queued_prompt.take() {
                    self.prompt(&prompt);
                }
            }
            SessionState::Failed => self.queued_prompt = None,
            _ => {}
        }
        if self.omp == OmpState::Ended
            && let Some(request) = self.queued_land.take()
        {
            self.send_land(request);
        }
    }

    fn send_land(&mut self, mut request: LandRequest) -> bool {
        let id = self.next_id();
        request.id.clone_from(&id);
        let frame = ClientFrame::Land(request);
        let sent = self.send_request(id, Request::Land, &frame, Some(ClientAction::LandSent));
        self.land_in_flight |= sent;
        sent
    }

    /// Sends a request whose reply must carry `id`, and opens it once the link took it.
    fn send_request(
        &mut self,
        id: String,
        kind: Request,
        frame: &ClientFrame,
        action: Option<ClientAction>,
    ) -> bool {
        let sent = self.send_client(frame, action);
        if sent {
            self.open.insert(id, kind);
        }
        sent
    }

    /// Asks omp how full its context is. The answer is read in `ContextMeter::on_omp`.
    fn ask_context(&mut self) {
        if self.closed || self.omp != OmpState::Running {
            return;
        }
        let frame = json!({ "id": self.next_id(), "type": STATS_COMMAND });
        self.send_omp(&frame, None);
    }

    fn send_client(&mut self, frame: &ClientFrame, action: Option<ClientAction>) -> bool {
        let Ok(value) = serde_json::to_value(frame) else {
            return false;
        };
        self.send_omp(&value, action)
    }

    fn send_omp(&mut self, frame: &Value, action: Option<ClientAction>) -> bool {
        if !self.link.send(frame.to_string()) {
            return false;
        }
        let record = self.transcript.outgoing((self.clock)(), frame);
        self.keep(record);
        if let Some(action) = action {
            self.session.apply_action(&action);
        }
        true
    }

    fn keep(&mut self, record: Option<String>) {
        let Some(record) = record else { return };
        if self.records.len() < RECORD_LIMIT {
            self.records.push(record);
        } else if !self.records_dropped {
            self.records_dropped = true;
            self.note("The transcript is full. Later frames are not recorded.");
        }
    }

    fn note(&mut self, text: &str) {
        if self.log.len() == LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(redact_secrets(text));
    }

    fn next_id(&mut self) -> String {
        self.next_id += 1;
        format!("req_{}", self.next_id)
    }
}

#[cfg(test)]
mod tests;
