use std::sync::atomic::{AtomicU64, Ordering};

use ch_core::{HISTORY_COMMAND, UsageRow, UsageStatus};

use super::*;

const FILE: &str = "/home/me/.omp/sessions/s1.jsonl";

fn resumed(rig: &mut Rig) {
    assert!(rig.driver.resume(
        "s1",
        vec![format!("--resume={FILE}")],
        BTreeMap::new(),
        Vec::new(),
    ));
    rig.hello();
    rig.say(&json!({"type":"ready","protocolVersion":1}));
}

/// The page requests sent so far.
fn page_requests(rig: &Rig) -> Vec<Value> {
    of_type(&rig.sent(), HISTORY_COMMAND)
        .into_iter()
        .cloned()
        .collect()
}

/// Waits for page request number `n` (from 1) and returns it.
fn wait_for_page_request(rig: &mut Rig, n: usize) -> Value {
    rig.pump_until("a page request", |_, sent| {
        of_type(sent, HISTORY_COMMAND).len() >= n
    });
    page_requests(rig)[n - 1].clone()
}

fn answer_page(rig: &Rig, request: &Value, messages: &Value, cursor: Option<&str>) {
    let mut data = json!({ "messages": messages, "totalMessages": 99 });
    if let Some(cursor) = cursor {
        data["nextCursor"] = json!(cursor);
    }
    rig.say(&json!({
        "id": request["id"], "type": "response", "command": HISTORY_COMMAND,
        "success": true, "data": data,
    }));
}

fn finish(rig: &mut Rig) -> EarlierTurns {
    rig.pump_until("the end of the load", |d, _| !d.loading_history());
    let earlier = rig.driver.take_history().expect("the earlier conversation");
    assert_eq!(rig.driver.take_history(), None, "it is handed over once");
    earlier
}

fn pair(prompt: &str, reply: &str) -> (String, String) {
    (prompt.to_string(), reply.to_string())
}

fn first_page() -> Value {
    json!([
        { "role": "user", "content": "Fix it" },
        { "role": "assistant", "content": [{ "type": "text", "text": "Done." }] },
    ])
}

#[test]
fn a_resume_starts_omp_on_the_saved_conversation_and_reads_it_back() {
    let mut rig = Rig::new();
    resumed(&mut rig);
    assert!(rig.driver.loading_history());
    let first = wait_for_page_request(&mut rig, 1);
    let sent = rig.sent();
    assert_eq!(sent[0]["type"], "ch_start");
    assert_eq!(
        sent[0]["omp_args"],
        json!(["--mode", "rpc", format!("--resume={FILE}")])
    );
    assert!(of_type(&sent, "prompt").is_empty(), "{sent:?}");
    assert_eq!(first["limit"], 256);
    assert!(first.get("cursor").is_none(), "{first}");
    assert!(
        !rig.driver.can_prompt(),
        "no prompt while the history loads"
    );
    assert!(!rig.driver.prompt("too soon"));

    answer_page(&rig, &first, &first_page(), Some("c2"));
    let second = wait_for_page_request(&mut rig, 2);
    assert_eq!(second["cursor"], "c2");
    assert_ne!(second["id"], first["id"]);
    answer_page(
        &rig,
        &second,
        &json!([{ "role": "user", "content": "Thanks" }]),
        None,
    );

    let earlier = finish(&mut rig);
    assert_eq!(
        earlier,
        EarlierTurns {
            turns: vec![pair("Fix it", "Done."), pair("Thanks", "")],
            hidden: 0,
            problem: None,
            replaces: false,
        }
    );
    assert!(rig.driver.can_prompt());
    let transcript = rig.driver.transcript();
    assert!(!transcript.contains(HISTORY_COMMAND), "{transcript}");
    assert!(!transcript.contains("Fix it"), "{transcript}");
}

#[test]
fn a_new_session_reads_no_history() {
    let mut rig = Rig::new();
    assert!(
        rig.driver
            .start("s1", Vec::new(), BTreeMap::new(), "hi".into())
    );
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| !of_type(sent, "prompt").is_empty());
    assert!(!rig.driver.loading_history());
    assert_eq!(rig.driver.take_history(), None);
    assert_eq!(page_requests(&rig), Vec::<Value>::new());
}

#[test]
fn an_error_answer_ends_the_load_with_its_reason() {
    let mut rig = Rig::new();
    resumed(&mut rig);
    let request = wait_for_page_request(&mut rig, 1);
    rig.say(&json!({
        "id": request["id"], "type": "response", "command": HISTORY_COMMAND,
        "success": false, "error": "Session is busy", "code": "session_busy",
    }));
    let earlier = finish(&mut rig);
    assert_eq!(earlier.turns, []);
    assert_eq!(
        earlier.problem.as_deref(),
        Some("Could not load the earlier conversation: Session is busy (session_busy)")
    );
    assert!(rig.driver.can_prompt());
}

static NOW: AtomicU64 = AtomicU64::new(1);

fn now() -> u64 {
    NOW.load(Ordering::SeqCst)
}

#[test]
fn a_page_omp_never_answers_times_out() {
    let mut rig = Rig::with_clock(now);
    resumed(&mut rig);
    let request = wait_for_page_request(&mut rig, 1);
    rig.driver.pump();
    assert!(rig.driver.loading_history(), "still within the wait");

    NOW.fetch_add(30_000, Ordering::SeqCst);
    let earlier = finish(&mut rig);
    assert_eq!(
        earlier.problem.as_deref(),
        Some("omp did not send the earlier conversation in time.")
    );

    // An answer that comes after the load gave up changes nothing.
    answer_page(&rig, &request, &first_page(), None);
    // Frames are read in order, so once the stats answer behind it is in, so is the page.
    let stats = of_type(&rig.sent(), STATS_COMMAND)[0]["id"].clone();
    rig.say(&json!({
        "id": stats, "type": "response", "command": STATS_COMMAND, "success": true,
        "data": { "sessionFile": FILE },
    }));
    rig.pump_until("the late page", |d, _| d.omp_session_file().is_some());
    assert_eq!(rig.driver.take_history(), None);
    assert_eq!(page_requests(&rig).len(), 1);
}

#[test]
fn a_closed_runner_keeps_the_turns_read_so_far() {
    let mut rig = Rig::new();
    resumed(&mut rig);
    let request = wait_for_page_request(&mut rig, 1);
    answer_page(&rig, &request, &first_page(), Some("c2"));
    wait_for_page_request(&mut rig, 2);
    rig.runner = None;

    let earlier = finish(&mut rig);
    assert_eq!(earlier.turns, vec![pair("Fix it", "Done.")]);
    assert_eq!(
        earlier.problem.as_deref(),
        Some("The runner closed before the earlier conversation was loaded.")
    );
    assert!(!rig.driver.can_prompt());
}

#[test]
fn a_cursor_seen_before_stops_the_load() {
    let mut rig = Rig::new();
    resumed(&mut rig);
    let first = wait_for_page_request(&mut rig, 1);
    answer_page(&rig, &first, &first_page(), Some("c2"));
    let second = wait_for_page_request(&mut rig, 2);
    answer_page(
        &rig,
        &second,
        &json!([{ "role": "user", "content": "Again" }]),
        Some("c2"),
    );

    let earlier = finish(&mut rig);
    assert_eq!(
        earlier.turns,
        vec![pair("Fix it", "Done."), pair("Again", "")]
    );
    assert_eq!(
        earlier.problem.as_deref(),
        Some("Only part of the earlier conversation was loaded.")
    );
    assert_eq!(page_requests(&rig).len(), 2);
}

#[test]
fn a_stats_answer_names_the_omp_session_file() {
    let mut rig = Rig::new();
    assert_eq!(rig.driver.omp_session_file(), None);
    resumed(&mut rig);
    rig.pump_until("a stats request", |_, sent| {
        !of_type(sent, STATS_COMMAND).is_empty()
    });
    let id = of_type(&rig.sent(), STATS_COMMAND)[0]["id"].clone();
    rig.say(&json!({
        "id": id, "type": "response", "command": STATS_COMMAND, "success": true,
        "data": { "sessionFile": FILE, "contextUsage": { "tokens": 10, "contextWindow": 100 } },
    }));
    rig.pump_until("the session file", |d, _| d.omp_session_file().is_some());
    assert_eq!(rig.driver.omp_session_file(), Some(FILE));
}

#[test]
fn turns_number_on_from_the_ledger() {
    let mut rig = Rig::new();
    let earlier = UsageRow {
        turn: 1,
        at_ms: 1,
        provider: None,
        model: None,
        input: Some(5),
        output: Some(1),
        cache_read: None,
        cache_write: None,
        cost_usd: None,
        status: UsageStatus::Partial,
    };
    assert!(
        rig.driver
            .resume("s1", Vec::new(), BTreeMap::new(), vec![earlier])
    );
    rig.say(&json!({
        "type":"turn_end","turnIndex":0,"toolResults":[],
        "message":{"role":"assistant","usage":{"input":1}}
    }));
    rig.pump_until("the turn", |d, _| d.usage().rows().len() == 2);
    assert_eq!(rig.driver.usage().rows()[1].turn, 2);
    assert_eq!(rig.driver.usage().totals().input, 6);
}
