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
        Self::with_clock(|| 1)
    }

    fn with_clock(clock: fn() -> u64) -> Self {
        let stdin = Stdin::default();
        let (tx, rx) = mpsc::channel();
        let stdout = Stdout {
            rx,
            pending: Vec::new(),
        };
        let link = Link::new(stdin.clone(), stdout, None::<std::io::Empty>);
        Self {
            driver: Driver::new(link, clock),
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

#[test]
fn each_turn_end_adds_a_usage_row() {
    let mut rig = Rig::new();
    rig.say(&json!({"type":"agent_start"}));
    rig.say(&json!({"type":"message_end","message":{"role":"assistant"}}));
    rig.say(&json!({
        "type":"turn_end","turnIndex":0,"toolResults":[],
        "message":{"role":"assistant","provider":"anthropic","model":"m","stopReason":"stop",
            "usage":{"input":10,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":12,
                "cost":{"total":0.001}}}
    }));
    rig.pump_until("a usage row", |driver, _| !driver.usage().rows().is_empty());
    let rows = rig.driver.usage().rows();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].turn, rows[0].at_ms), (1, 1));
    assert_eq!((rows[0].input, rows[0].output), (Some(10), Some(2)));
}

#[test]
fn each_turn_end_reads_the_context_and_keeps_it_out_of_the_transcript() {
    let mut rig = Rig::new();
    assert_eq!(rig.driver.context(), ContextState::NotRead);
    rig.say(&json!({"type":"turn_end","turnIndex":0,"toolResults":[]}));
    rig.pump_until("a stats request", |_, sent| {
        !of_type(sent, STATS_COMMAND).is_empty()
    });
    let sent = rig.sent();
    let id = of_type(&sent, STATS_COMMAND)[0]["id"].clone();
    assert!(id.is_string(), "{id}");

    rig.say(&json!({
        "id": id, "type": "response", "command": STATS_COMMAND, "success": true,
        "data": { "sessionFile": "/home/me/.omp/s.jsonl",
            "contextUsage": { "tokens": 18003, "contextWindow": 200_000 } },
    }));
    rig.pump_until("the reading", |d, _| {
        matches!(d.context(), ContextState::Known(_))
    });
    let transcript = rig.driver.transcript();
    assert!(!transcript.contains(STATS_COMMAND), "{transcript}");
    assert!(!transcript.contains("sessionFile"), "{transcript}");
}

#[test]
fn no_stats_request_once_omp_is_asked_to_stop() {
    let mut rig = Rig::new();
    assert!(rig.driver.stop());
    rig.say(&json!({
        "type":"turn_end","turnIndex":0,"toolResults":[],
        "message":{"role":"assistant","usage":{"input":1}}
    }));
    rig.pump_until("the turn", |d, _| !d.usage().rows().is_empty());
    assert_eq!(of_type(&rig.sent(), STATS_COMMAND), Vec::<&Value>::new());
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
    rig.say(&json!({"id":"req_2","type":"response","command":"prompt","success":true}));
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
    rig.pump_until("the answer", |_, sent| sent.len() == 4);
    assert_eq!(
        rig.sent()[3],
        json!({"type":"extension_ui_response","id":"ui_1","value":"Approve"})
    );
    assert_eq!(rig.driver.session().state(), SessionState::Running);

    rig.say(&json!({"type":"extension_ui_request","id":"ui_2","method":"input","title":"Name?"}));
    rig.pump_until("the cancel", |_, sent| sent.len() == 5);
    assert_eq!(
        rig.sent()[4],
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
    rig.pump_until("the queued prompt", |_, sent| sent.len() == 3);
    let sent = rig.sent();
    assert_eq!(sent[0]["type"], "ch_start");
    assert_eq!(
        sent[0]["omp_args"],
        json!(["--mode", "rpc", "--model", "m1"])
    );
    assert_eq!(sent[0]["env"]["OPENAI_API_KEY"], KEY);
    assert_eq!(sent[1], json!({"id":"req_1","type":STATS_COMMAND}));
    assert_eq!(
        sent[2],
        json!({"id":"req_2","type":"prompt","message":"Fix the bug"})
    );

    approve_bash_then_cancel_input(&mut rig);

    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    assert!(rig.driver.verify("cargo test"));
    assert!(rig.driver.diff());
    rig.say(
        &json!({"type":"ch_evidence","id":"req_3","command":"cargo test","exit_code":0,
        "duration_ms":10,"output_tail":"ok","truncated":false,"timed_out":false}),
    );
    rig.say(
        &json!({"type":"ch_diff_result","id":"req_4","base":"abc","stat":" a | 1 +",
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

    rig.say(&json!({"type":"ch_stopped","id":"req_5","exit_code":0}));
    rig.pump_until("the land", |_, sent| !of_type(sent, "ch_land").is_empty());
    let sent = rig.sent();
    let land = of_type(&sent, "ch_land")[0];
    assert_eq!(land["token"], TOKEN);
    assert_eq!(land["branch"], "ch/s1");

    rig.say(
        &json!({"type":"ch_landed","id":"req_6","branch":"ch/s1","commit":"def",
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
    rig.pump_until("prompt", |_, sent| sent.len() == 3);
    assert_eq!(
        rig.sent()[1]["type"],
        STATS_COMMAND,
        "the context is read first"
    );
    assert_eq!(rig.sent()[2]["message"], "hello");
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
    assert!(
        of_type(&rig.sent(), "prompt").is_empty(),
        "{:?}",
        rig.sent()
    );
}

fn settle(rig: &mut Rig) {
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| sent.len() == 3);
    rig.say(&json!({"type":"agent_start"}));
    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
}

#[test]
fn verify_waits_until_the_agent_is_done() {
    let mut rig = Rig::new();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| sent.len() == 3);
    rig.say(&json!({"type":"agent_start"}));
    rig.pump_until("the agent to run", |d, _| {
        d.session().state() == SessionState::Running
    });
    assert!(!rig.driver.verify("cargo test"));

    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    assert!(
        of_type(&rig.sent(), "ch_verify").is_empty(),
        "a verify went out while the agent worked"
    );
    assert!(rig.driver.verify("cargo test"));
}

#[test]
fn a_select_that_is_not_an_approval_is_answered_no() {
    let mut rig = Rig::new();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| sent.len() == 3);
    rig.say(
        &json!({"type":"extension_ui_request","id":"ui_7","method":"select",
        "title":"Pick a model","options":["a","b"]}),
    );
    rig.pump_until("the cancel", |_, sent| sent.len() == 4);
    assert_eq!(
        rig.sent()[3],
        json!({"type":"extension_ui_response","id":"ui_7","cancelled":true})
    );
    let pending = rig.driver.session().pending_approvals();
    assert!(pending.is_empty(), "{pending:?}");

    rig.say(
        &json!({"type":"extension_ui_request","id":"ui_8","method":"ask",
        "questions":[{"id":"q1","question":"Which file?","options":[{"label":"a"}]}]}),
    );
    rig.pump_until("the ask cancel", |_, sent| sent.len() == 5);
    assert_eq!(
        rig.sent()[4],
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
    let stop_id = of_type(&rig.sent(), "ch_stop")[0]["id"].clone();
    rig.say(&json!({"type":"ch_stopped","id":stop_id,"exit_code":0}));
    rig.pump_until("the land", |_, sent| !of_type(sent, "ch_land").is_empty());
    assert!(!rig.driver.can_land(), "a land is already out");
    assert!(!rig.driver.can_review());

    let land_id = of_type(&rig.sent(), "ch_land")[0]["id"].clone();
    rig.say(&json!({"type":"ch_error","id":land_id,"stage":"land","message":"push rejected"}));
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

fn drops(driver: &Driver) -> usize {
    driver.log().filter(|l| l.starts_with("Dropped a ")).count()
}

fn diff_result(id: &Value, stat: &str) -> Value {
    json!({"type":"ch_diff_result","id":id,"base":"abc","stat":stat,"patch":"+x",
        "truncated":false})
}

#[test]
fn replies_that_answer_no_open_request_are_dropped() {
    let mut rig = Rig::new();
    settle(&mut rig);
    assert!(rig.driver.diff());
    rig.pump_until("the diff request", |_, sent| {
        !of_type(sent, "ch_diff").is_empty()
    });
    let diff_id = of_type(&rig.sent(), "ch_diff")[0]["id"].clone();

    rig.say(&json!({"type":"ch_landed","id":diff_id,"branch":"ch/s1","commit":"def"}));
    rig.say(
        &json!({"type":"ch_evidence","id":"req_99","command":"cargo test","exit_code":0,
        "duration_ms":1,"output_tail":"","truncated":false,"timed_out":false}),
    );
    rig.say(&json!({"type":"ch_stopped","id":"req_99","exit_code":0}));
    rig.say(&diff_result(&diff_id, "first"));
    rig.pump_until("the real diff", |d, _| d.session().last_diff().is_some());
    assert_eq!(
        drops(&rig.driver),
        3,
        "{:?}",
        rig.driver.log().collect::<Vec<_>>()
    );
    assert_ne!(rig.driver.session().state(), SessionState::Landed);
    assert!(
        rig.driver.session().evidence().is_empty(),
        "evidence for a verify never sent was kept"
    );
    assert!(rig.driver.can_prompt(), "a forged ch_stopped ended omp");

    rig.say(&diff_result(&diff_id, "replayed"));
    rig.pump_until("the replay to be dropped", |d, _| drops(d) == 4);
    assert_eq!(
        rig.driver.session().last_diff().map(|d| d.stat.as_str()),
        Some("first"),
        "a replayed diff replaced the answered one"
    );
    assert!(
        !rig.driver.transcript().contains("replayed"),
        "a dropped reply reached the transcript"
    );
}

#[test]
fn an_error_answers_an_open_request_of_any_kind() {
    let mut rig = Rig::new();
    settle(&mut rig);
    assert!(rig.driver.verify("cargo test"));
    rig.pump_until("the verify request", |_, sent| {
        !of_type(sent, "ch_verify").is_empty()
    });
    let verify_id = of_type(&rig.sent(), "ch_verify")[0]["id"].clone();
    rig.say(&json!({"type":"ch_error","id":verify_id,"stage":"protocol","message":"bad"}));
    rig.say(&json!({"type":"ch_error","id":verify_id,"stage":"protocol","message":"again"}));
    rig.pump_until("the second error to be dropped", |d, _| drops(d) == 1);
    assert!(
        rig.driver.transcript().contains(r#""message":"bad""#),
        "the first error answers the open verify"
    );
    assert!(
        !rig.driver.transcript().contains(r#""message":"again""#),
        "a second answer to the same request was kept"
    );
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

mod checkpoints;
mod resume;
