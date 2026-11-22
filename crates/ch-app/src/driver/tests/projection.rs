//! The UI only shows state; a button never grants anything by itself. Every action a button can
//! call is checked again here against the session, and a refused one writes nothing.

use super::*;

const UI_RESPONSE: &str = "extension_ui_response";

/// Waits until everything sent so far was written. The writer keeps order, so once a probe
/// diff is out, no earlier frame is still on its way.
fn flush(rig: &mut Rig) {
    let count = of_type(&rig.sent(), "ch_diff").len();
    assert!(rig.driver.diff());
    rig.pump_until("the probe", |_, sent| {
        of_type(sent, "ch_diff").len() == count + 1
    });
}

fn responses(rig: &Rig) -> Vec<Value> {
    of_type(&rig.sent(), UI_RESPONSE)
        .into_iter()
        .cloned()
        .collect()
}

/// Starts a session whose agent waits on approval `ui_1`.
fn waiting_on_approval(rig: &mut Rig) {
    rig.hello();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| sent.len() == 3);
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
}

#[test]
fn an_answer_to_an_approval_omp_never_asked_for_is_not_sent() {
    let mut rig = Rig::new();
    waiting_on_approval(&mut rig);

    let refused = rig.driver.answer("ui_9", ApprovalDecision::ApproveOnce);

    assert!(refused.is_err());
    flush(&mut rig);
    assert_eq!(responses(&rig), Vec::<Value>::new());
    assert_eq!(rig.driver.session().state(), SessionState::NeedsYou);
}

#[test]
fn a_choice_omp_did_not_offer_is_not_sent() {
    let mut rig = Rig::new();
    waiting_on_approval(&mut rig);
    rig.say(&json!({
        "type":"extension_ui_request","id":"ui_3","method":"select",
        "title":"Allow tool: write\nArguments:\n  path: src/lib.rs",
        "options":["Approve","Deny"]
    }));
    rig.pump_until("the second approval", |d, _| {
        d.session().pending_approvals().len() == 2
    });

    let refused = rig
        .driver
        .answer("ui_3", ApprovalDecision::ApproveForSession);

    assert!(refused.is_err(), "omp offered no approve-for-session here");
    flush(&mut rig);
    assert_eq!(responses(&rig), Vec::<Value>::new());
    assert_eq!(rig.driver.session().pending_approvals().len(), 2);
}

#[test]
fn an_approval_omp_withdrew_cannot_be_answered() {
    let mut rig = Rig::new();
    waiting_on_approval(&mut rig);
    rig.say(
        &json!({"type":"extension_ui_request","id":"ui_2","method":"cancel","targetId":"ui_1"}),
    );
    rig.pump_until("the withdrawal", |d, _| {
        d.session().state() == SessionState::Running
    });

    let refused = rig.driver.answer("ui_1", ApprovalDecision::ApproveOnce);

    assert!(refused.is_err());
    flush(&mut rig);
    assert_eq!(responses(&rig), Vec::<Value>::new());
}

#[test]
fn an_approval_is_answered_once() {
    let mut rig = Rig::new();
    waiting_on_approval(&mut rig);
    rig.driver
        .answer("ui_1", ApprovalDecision::Deny)
        .expect("the open approval takes an answer");

    let again = rig.driver.answer("ui_1", ApprovalDecision::ApproveOnce);

    assert!(again.is_err(), "a second click must not approve");
    flush(&mut rig);
    assert_eq!(
        responses(&rig),
        vec![json!({"type":UI_RESPONSE,"id":"ui_1","value":"Deny"})]
    );
}

#[test]
fn approvals_end_with_the_omp_that_asked_for_them() {
    let mut rig = Rig::new();
    waiting_on_approval(&mut rig);
    rig.say(&json!({"type":"ch_omp_exited","exit_code":1,"stderr_tail":[]}));
    rig.pump_until("omp to end", |d, _| {
        d.session().pending_approvals().is_empty()
    });

    let refused = rig.driver.answer("ui_1", ApprovalDecision::ApproveOnce);

    assert!(refused.is_err());
    flush(&mut rig);
    assert_eq!(responses(&rig), Vec::<Value>::new());
}

#[test]
fn nothing_can_be_landed_or_checked_while_omp_waits_on_the_person() {
    let mut rig = Rig::new();
    waiting_on_approval(&mut rig);
    assert!(!rig.driver.can_land());
    assert!(!rig.driver.can_review());
    assert!(!rig.driver.can_prompt());

    assert!(!rig.driver.land(land_request()));
    assert!(!rig.driver.verify("cargo test"));
    assert!(!rig.driver.prompt("land it anyway"));

    flush(&mut rig);
    let sent = rig.sent();
    for kind in ["ch_stop", "ch_land", "ch_verify"] {
        assert_eq!(
            of_type(&sent, kind),
            Vec::<&Value>::new(),
            "{kind} went out"
        );
    }
    assert_eq!(of_type(&sent, "prompt").len(), 1, "only the first prompt");
    assert_eq!(rig.driver.session().state(), SessionState::NeedsYou);
}

#[test]
fn a_land_is_refused_while_the_agent_works() {
    let mut rig = Rig::new();
    rig.hello();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| sent.len() == 3);
    rig.say(&json!({"type":"agent_start"}));
    rig.pump_until("the run", |d, _| {
        d.session().state() == SessionState::Running
    });

    assert!(!rig.driver.land(land_request()));

    flush(&mut rig);
    assert!(!rig.driver.landing());
    assert_eq!(of_type(&rig.sent(), "ch_stop"), Vec::<&Value>::new());
    assert_eq!(rig.driver.session().state(), SessionState::Running);
}

/// Asserts that a land went nowhere: no stop for the agent and no land for the runner.
fn assert_nothing_landed(rig: &mut Rig) {
    flush(rig);
    let sent = rig.sent();
    for kind in ["ch_stop", "ch_land"] {
        assert_eq!(
            of_type(&sent, kind),
            Vec::<&Value>::new(),
            "{kind} went out"
        );
    }
    assert!(!rig.driver.landing());
}

#[test]
fn a_runner_older_than_the_secret_scan_cannot_land() {
    let mut rig = Rig::new();
    rig.hello_at(SECRET_SCAN_PROTOCOL - 1);
    run_to_review(&mut rig);

    assert_eq!(rig.driver.runner_block(), Some(RunnerBlock::Outdated));
    assert!(!rig.driver.can_land());
    assert!(!rig.driver.land(land_request()));
    assert_nothing_landed(&mut rig);
}

#[test]
fn a_later_hello_cannot_make_the_runner_newer() {
    let mut rig = Rig::new();
    rig.hello_at(SECRET_SCAN_PROTOCOL - 1);
    rig.hello();
    run_to_review(&mut rig);

    assert_eq!(rig.driver.runner_block(), Some(RunnerBlock::Outdated));
    assert!(!rig.driver.land(land_request()));
    assert_nothing_landed(&mut rig);
}

#[test]
fn nothing_lands_through_a_runner_that_never_said_hello() {
    let mut rig = Rig::new();
    run_to_review(&mut rig);

    assert_eq!(
        rig.driver.runner_block(),
        Some(RunnerBlock::Silent),
        "an unknown runner is named, not called old"
    );
    assert!(!rig.driver.can_land());
    assert!(!rig.driver.land(land_request()));
    assert_nothing_landed(&mut rig);
}

#[test]
fn a_missing_hello_is_not_named_while_the_session_starts() {
    let mut rig = Rig::new();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());

    assert_eq!(rig.driver.session().state(), SessionState::Starting);
    assert_eq!(
        rig.driver.runner_block(),
        None,
        "the hello may be on its way"
    );
}

#[test]
fn a_runner_at_or_past_the_secret_scan_protocol_lands() {
    for protocol in [SECRET_SCAN_PROTOCOL, SECRET_SCAN_PROTOCOL + 1] {
        let mut rig = Rig::new();
        rig.hello_at(protocol);
        run_to_review(&mut rig);

        assert_eq!(rig.driver.runner_block(), None, "protocol {protocol}");
        assert!(rig.driver.land(land_request()), "protocol {protocol}");
        rig.pump_until("the stop", |_, sent| !of_type(sent, "ch_stop").is_empty());
        let stop_id = of_type(&rig.sent(), "ch_stop")[0]["id"].clone();
        rig.say(&json!({"type":"ch_stopped","id":stop_id,"exit_code":0}));
        rig.pump_until("the land", |_, sent| !of_type(sent, "ch_land").is_empty());
    }
}
