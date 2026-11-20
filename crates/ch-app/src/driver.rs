//! One session's traffic with its runner, kept out of the window so it can be tested.
//!
//! The driver owns the [`Link`] and the [`Session`] reducer. Every frame the app writes goes
//! through a method here, which sends it, adds it to the transcript and reports it to the
//! reducer, in that order and only once the link took it.

use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use ch_core::{
    ApprovalDecision, ApprovalRequest, ClientAction, Session, SessionState, Transcript,
    redact_secrets,
};
use ch_protocol::runner::{ClientFrame, LandRequest, RunnerFrame, RunnerLine, StartSession};
use serde_json::{Value, json};

use crate::link::{Link, LinkEvent};

const OMP_ARGS: [&str; 2] = ["--mode", "rpc"];
const PUMP_LIMIT: usize = 256;
const LOG_LIMIT: usize = 200;
const RECORD_LIMIT: usize = 50_000;
/// omp dialogs that wait for an answer the app has no screen for. omp reads a cancel as no.
const UNANSWERABLE_DIALOGS: &[&str] = &["confirm", "input", "editor", "ask"];

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

pub struct Driver {
    link: Link,
    session: Session,
    transcript: Transcript,
    records: Vec<String>,
    records_dropped: bool,
    log: VecDeque<String>,
    clock: fn() -> u64,
    next_id: u64,
    queued_prompt: Option<String>,
    queued_land: Option<LandRequest>,
    land_in_flight: bool,
    omp: OmpState,
    closed: bool,
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
            queued_prompt: None,
            queued_land: None,
            land_in_flight: false,
            omp: OmpState::Running,
            closed: false,
        }
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

    /// Whether a prompt sent now would start a run.
    pub fn can_prompt(&self) -> bool {
        if self.closed || self.omp != OmpState::Running {
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
    }

    /// Whether a land can go out now: once the work is ready for review, and again after a land
    /// that failed, since a rejected push is fixed by changing settings and trying again.
    pub fn can_land(&self) -> bool {
        if self.closed || self.landing() {
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

    pub fn verify(&mut self, command: &str) -> bool {
        let id = self.next_id();
        let frame = ClientFrame::Verify {
            id,
            command: command.to_string(),
            timeout_secs: None,
        };
        self.send_client(&frame, None)
    }

    pub fn diff(&mut self) -> bool {
        let id = self.next_id();
        self.send_client(&ClientFrame::Diff { id }, None)
    }

    pub fn stop(&mut self) -> bool {
        if self.omp != OmpState::Running {
            return true;
        }
        let id = self.next_id();
        let sent = self.send_client(&ClientFrame::Stop { id }, Some(ClientAction::StopSent));
        if sent {
            self.omp = OmpState::StopSent;
        }
        sent
    }

    /// Lands the work. The runner refuses a land while omp runs, so omp is stopped first and the
    /// land goes out once the runner says it ended.
    pub fn land(&mut self, mut request: LandRequest) -> bool {
        request.id = self.next_id();
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
        let record = self.transcript.incoming((self.clock)(), line);
        self.keep(record);
        self.session.apply_line(line);
        match line {
            RunnerLine::Runner(RunnerFrame::Stopped { .. } | RunnerFrame::OmpExited { .. }) => {
                self.omp = OmpState::Ended;
                self.queued_prompt = None;
            }
            RunnerLine::Runner(RunnerFrame::Landed { .. }) => self.land_in_flight = false,
            RunnerLine::Runner(RunnerFrame::Error { stage, .. }) if stage == "land" => {
                self.land_in_flight = false;
            }
            RunnerLine::Omp(frame) => self.cancel_unanswerable(frame),
            RunnerLine::Runner(_) => {}
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

    fn send_land(&mut self, request: LandRequest) -> bool {
        let sent = self.send_client(&ClientFrame::Land(request), Some(ClientAction::LandSent));
        self.land_in_flight |= sent;
        sent
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
mod tests {
    use super::*;
    use ch_protocol::runner::GitAuthor;
    use std::io::{Read, Write};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    const KEY: &str = "sk-proj-TESTKEYabcdefghijklmnopqrstuvwxyz0123456789";
    const TOKEN: &str = "github_pat_11TESTTOKEN0abcdefghijklmnopqrstuvwxyz0123456789";

    #[derive(Clone, Default)]
    struct Stdin(Arc<Mutex<Vec<u8>>>);

    impl Write for Stdin {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Stdin {
        fn lines(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    /// stdout fed one write at a time, so the test decides what the runner has said so far.
    struct Stdout {
        rx: Receiver<Vec<u8>>,
        pending: Vec<u8>,
    }

    impl Read for Stdout {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.is_empty() {
                match self.rx.recv() {
                    Ok(bytes) => self.pending = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.pending.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            Ok(n)
        }
    }

    struct Rig {
        driver: Driver,
        stdin: Stdin,
        runner: Option<Sender<Vec<u8>>>,
    }

    impl Rig {
        fn new() -> Self {
            let stdin = Stdin::default();
            let (tx, rx) = mpsc::channel();
            let stdout = Stdout {
                rx,
                pending: Vec::new(),
            };
            let link = Link::new(stdin.clone(), stdout, None::<std::io::Empty>);
            Self {
                driver: Driver::new(link, || 1),
                stdin,
                runner: Some(tx),
            }
        }

        fn say(&self, line: &Value) {
            let mut bytes = line.to_string().into_bytes();
            bytes.push(b'\n');
            self.runner.as_ref().unwrap().send(bytes).unwrap();
        }

        fn pump_until(&mut self, what: &str, done: impl Fn(&Driver, &[Value]) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                self.driver.pump();
                if done(&self.driver, &self.stdin.lines()) {
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            }
            panic!(
                "timed out waiting for {what}; sent {:?}",
                self.stdin.lines()
            );
        }

        fn sent(&self) -> Vec<Value> {
            self.stdin.lines()
        }
    }

    fn of_type<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
        lines.iter().filter(|l| l["type"] == kind).collect()
    }

    fn land_request() -> LandRequest {
        LandRequest {
            id: String::new(),
            branch: "ch/s1".into(),
            message: "Fix the bug".into(),
            author: GitAuthor {
                name: "Dev".into(),
                email: "dev@example.com".into(),
            },
            remote_url: "https://github.com/example/demo.git".into(),
            token: Some(TOKEN.into()),
            pull_request: None,
        }
    }

    /// omp asks to run bash, which is approved once, then asks for text, which has no screen.
    fn approve_bash_then_cancel_input(rig: &mut Rig) {
        rig.say(&json!({"id":"req_1","type":"response","command":"prompt","success":true}));
        rig.say(&json!({"type":"agent_start"}));
        rig.say(&json!({
            "type":"extension_ui_request","id":"ui_1","method":"select",
            "title":"Allow tool: bash\nReason: run the test suite\nArguments:\n  command: cargo test",
            "options":["Approve","Approve bash for this session","Deny"]
        }));
        rig.pump_until("the approval", |d, _| {
            d.session().state() == SessionState::NeedsYou
        });
        rig.driver
            .answer("ui_1", ApprovalDecision::ApproveOnce)
            .unwrap();
        rig.pump_until("the answer", |_, sent| sent.len() == 3);
        assert_eq!(
            rig.sent()[2],
            json!({"type":"extension_ui_response","id":"ui_1","value":"Approve"})
        );
        assert_eq!(rig.driver.session().state(), SessionState::Running);

        rig.say(
            &json!({"type":"extension_ui_request","id":"ui_2","method":"input","title":"Name?"}),
        );
        rig.pump_until("the cancel", |_, sent| sent.len() == 4);
        assert_eq!(
            rig.sent()[3],
            json!({"type":"extension_ui_response","id":"ui_2","cancelled":true})
        );
    }

    #[test]
    fn runs_one_session_from_start_to_landed() {
        let mut rig = Rig::new();
        let env = BTreeMap::from([("OPENAI_API_KEY".to_string(), KEY.to_string())]);
        assert!(rig.driver.start(
            "s1",
            vec!["--model".into(), "m1".into()],
            env,
            "Fix the bug".into()
        ));

        rig.say(&json!({"type":"ch_runner_hello","version":"0.1.0","protocol":1,"workdir":"/w"}));
        rig.say(&json!({"type":"ready","protocolVersion":1}));
        rig.pump_until("the queued prompt", |_, sent| sent.len() == 2);
        let sent = rig.sent();
        assert_eq!(sent[0]["type"], "ch_start");
        assert_eq!(
            sent[0]["omp_args"],
            json!(["--mode", "rpc", "--model", "m1"])
        );
        assert_eq!(sent[0]["env"]["OPENAI_API_KEY"], KEY);
        assert_eq!(
            sent[1],
            json!({"id":"req_1","type":"prompt","message":"Fix the bug"})
        );

        approve_bash_then_cancel_input(&mut rig);

        rig.say(&json!({"type":"session_settled"}));
        rig.pump_until("review", |d, _| {
            d.session().state() == SessionState::NeedsReview
        });
        assert!(rig.driver.verify("cargo test"));
        assert!(rig.driver.diff());
        rig.say(
            &json!({"type":"ch_evidence","id":"req_2","command":"cargo test","exit_code":0,
            "duration_ms":10,"output_tail":"ok","truncated":false,"timed_out":false}),
        );
        rig.say(
            &json!({"type":"ch_diff_result","id":"req_3","base":"abc","stat":" a | 1 +",
            "patch":"+x","truncated":false}),
        );
        rig.pump_until("evidence and diff", |d, _| {
            d.session().evidence().len() == 1 && d.session().last_diff().is_some()
        });

        assert!(rig.driver.land(land_request()));
        assert!(rig.driver.stopping());
        rig.pump_until("the stop", |_, sent| !of_type(sent, "ch_stop").is_empty());
        let sent = rig.sent();
        assert_eq!(of_type(&sent, "ch_stop").len(), 1);
        assert!(
            of_type(&sent, "ch_land").is_empty(),
            "land must wait for omp to stop"
        );

        rig.say(&json!({"type":"ch_stopped","id":"req_4","exit_code":0}));
        rig.pump_until("the land", |_, sent| !of_type(sent, "ch_land").is_empty());
        let sent = rig.sent();
        let land = of_type(&sent, "ch_land")[0];
        assert_eq!(land["token"], TOKEN);
        assert_eq!(land["branch"], "ch/s1");

        rig.say(
            &json!({"type":"ch_landed","id":"req_5","branch":"ch/s1","commit":"def",
            "pull_request_url":"https://github.com/example/demo/pull/7"}),
        );
        rig.pump_until("landed", |d, _| d.session().state() == SessionState::Landed);
        assert_eq!(
            rig.driver.session().pull_request_url(),
            Some("https://github.com/example/demo/pull/7")
        );

        let transcript = rig.driver.transcript();
        assert!(
            !transcript.contains(KEY),
            "model key reached the transcript"
        );
        assert!(!transcript.contains(TOKEN), "token reached the transcript");
        assert!(transcript.contains("ch_land"));
        assert!(transcript.contains(r#""type":"prompt""#));

        rig.runner = None;
        rig.pump_until("closed", |d, _| d.closed());
        assert!(!rig.driver.can_prompt());
    }

    #[test]
    fn prompt_waits_until_omp_is_ready() {
        let mut rig = Rig::new();
        assert!(
            rig.driver
                .start("s1", Vec::new(), BTreeMap::new(), "hello".into())
        );
        assert!(!rig.driver.prompt("too early"));
        rig.say(&json!({"type":"ch_runner_hello","version":"0.1.0","protocol":1,"workdir":"/w"}));
        rig.pump_until("hello", |d, sent| {
            sent.len() == 1 && d.transcript().contains("ch_runner_hello")
        });
        assert_eq!(
            rig.sent().len(),
            1,
            "nothing but ch_start before omp is ready"
        );
        rig.say(&json!({"type":"ready"}));
        rig.pump_until("prompt", |_, sent| sent.len() == 2);
        assert_eq!(rig.sent()[1]["message"], "hello");
    }

    #[test]
    fn a_failed_start_drops_the_queued_prompt() {
        let mut rig = Rig::new();
        rig.driver
            .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
        rig.say(&json!({"type":"ch_error","stage":"start","message":"omp not found"}));
        rig.pump_until("failure", |d, _| {
            d.session().state() == SessionState::Failed
        });
        rig.say(&json!({"type":"ready"}));
        rig.runner = None;
        rig.pump_until("closed", |d, _| d.closed());
        assert!(!rig.driver.transcript().contains(r#""type":"prompt""#));
        assert_eq!(rig.sent().len(), 1, "{:?}", rig.sent());
    }

    fn settle(rig: &mut Rig) {
        rig.driver
            .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
        rig.say(&json!({"type":"ready"}));
        rig.pump_until("the prompt", |_, sent| sent.len() == 2);
        rig.say(&json!({"type":"agent_start"}));
        rig.say(&json!({"type":"session_settled"}));
        rig.pump_until("review", |d, _| {
            d.session().state() == SessionState::NeedsReview
        });
    }

    #[test]
    fn a_select_that_is_not_an_approval_is_answered_no() {
        let mut rig = Rig::new();
        rig.driver
            .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
        rig.say(&json!({"type":"ready"}));
        rig.pump_until("the prompt", |_, sent| sent.len() == 2);
        rig.say(
            &json!({"type":"extension_ui_request","id":"ui_7","method":"select",
            "title":"Pick a model","options":["a","b"]}),
        );
        rig.pump_until("the cancel", |_, sent| sent.len() == 3);
        assert_eq!(
            rig.sent()[2],
            json!({"type":"extension_ui_response","id":"ui_7","cancelled":true})
        );
        let pending = rig.driver.session().pending_approvals();
        assert!(pending.is_empty(), "{pending:?}");

        rig.say(
            &json!({"type":"extension_ui_request","id":"ui_8","method":"ask",
            "questions":[{"id":"q1","question":"Which file?","options":[{"label":"a"}]}]}),
        );
        rig.pump_until("the ask cancel", |_, sent| sent.len() == 4);
        assert_eq!(
            rig.sent()[3],
            json!({"type":"extension_ui_response","id":"ui_8","cancelled":true})
        );
    }

    #[test]
    fn a_land_in_flight_blocks_a_second_one_until_it_fails() {
        let mut rig = Rig::new();
        settle(&mut rig);
        assert!(rig.driver.can_land());
        assert!(rig.driver.land(land_request()));
        assert!(!rig.driver.can_land());
        rig.pump_until("the stop", |_, sent| !of_type(sent, "ch_stop").is_empty());
        rig.say(&json!({"type":"ch_stopped","id":"req_3","exit_code":0}));
        rig.pump_until("the land", |_, sent| !of_type(sent, "ch_land").is_empty());
        assert!(!rig.driver.can_land(), "a land is already out");
        assert!(!rig.driver.can_review());

        rig.say(&json!({"type":"ch_error","id":"req_4","stage":"land","message":"push rejected"}));
        rig.pump_until("the failure", |d, _| {
            d.session().state() == SessionState::Failed
        });
        assert!(rig.driver.can_land());
        assert!(!rig.driver.can_prompt(), "omp has ended");

        assert!(rig.driver.land(land_request()));
        rig.pump_until("the second land", |_, sent| {
            of_type(sent, "ch_land").len() == 2
        });
        assert_eq!(
            of_type(&rig.sent(), "ch_stop").len(),
            1,
            "omp was already stopped"
        );
        assert_ne!(rig.driver.session().state(), SessionState::Failed);
    }

    #[test]
    fn stderr_is_masked_before_display() {
        let mut rig = Rig::new();
        let stdin = Stdin::default();
        let (_tx, rx) = mpsc::channel::<Vec<u8>>();
        let stdout = Stdout {
            rx,
            pending: Vec::new(),
        };
        let stderr = std::io::Cursor::new(format!("using key {KEY}\n").into_bytes());
        rig.driver = Driver::new(Link::new(stdin, stdout, Some(stderr)), || 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while rig.driver.log().next().is_none() && Instant::now() < deadline {
            rig.driver.pump();
            thread::sleep(Duration::from_millis(2));
        }
        let log: Vec<&str> = rig.driver.log().collect();
        assert_eq!(log.len(), 1);
        assert!(!log[0].contains(KEY), "{log:?}");
    }
}
