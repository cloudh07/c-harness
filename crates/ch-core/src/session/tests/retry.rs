//! omp's own retries while it works, and what kind of failure a prompt ends in.

use serde_json::json;

use super::*;

const RETRY: &str = r#"{"type":"auto_retry_start","attempt":2,"maxAttempts":3,"delayMs":4000,"errorMessage":"529 overloaded"}"#;
const RETRY_DONE: &str = r#"{"type":"auto_retry_end","success":true,"attempt":2}"#;

// A way for a retry to end, named for the assert message.
type End = (&'static str, fn(&mut Session));

fn text(session: &mut Session, delta: &str) {
    let frame = json!({
        "type": "message_update",
        "assistantMessageEvent": {"type": "text_delta", "delta": delta}
    });
    feed(session, &frame.to_string());
}

fn message_start(session: &mut Session) {
    feed(session, r#"{"type":"message_start"}"#);
}

fn message_end(session: &mut Session, stop_reason: &str) {
    let frame = json!({
        "type": "message_end",
        "message": {"role": "assistant", "stopReason": stop_reason}
    });
    feed(session, &frame.to_string());
}

fn retrying() -> Session {
    let mut session = running();
    feed(&mut session, RETRY);
    assert!(session.retrying().is_some());
    session
}

fn prompt_error(error: &serde_json::Value) -> Session {
    let mut session = running();
    let frame = json!({
        "id": "req_2", "type": "prompt_result", "status": "error",
        "error": error, "sessionSettled": true
    });
    feed(&mut session, &frame.to_string());
    assert_eq!(session.state(), SessionState::Failed);
    session
}

#[test]
fn a_retry_shows_omps_count_and_the_run_carries_on() {
    let mut session = running();
    message_start(&mut session);
    text(&mut session, "Looking at the tests. ");
    message_end(&mut session, "toolUse");
    message_start(&mut session);
    text(&mut session, "The fix is to cha");
    message_end(&mut session, "error");

    feed(&mut session, RETRY);

    assert_eq!(session.state(), SessionState::Running);
    assert_eq!(
        session.retrying(),
        Some(&Retrying {
            attempt: Some(2),
            max_attempts: Some(3),
            reason: "529 overloaded".to_owned(),
        })
    );
    assert_eq!(
        session.reply(),
        "Looking at the tests. ",
        "the failed attempt's text is dropped, earlier text stays"
    );

    message_start(&mut session);
    text(&mut session, "The fix is to change the parser.");
    feed(&mut session, RETRY_DONE);
    assert_eq!(session.retrying(), None);
    feed(
        &mut session,
        r#"{"id":"req_2","type":"prompt_result","status":"completed","sessionSettled":true}"#,
    );

    assert_eq!(session.state(), SessionState::NeedsReview);
    assert_eq!(
        session.reply(),
        "Looking at the tests. The fix is to change the parser."
    );
    assert_eq!(session.failure(), None);
}

#[test]
fn a_retry_without_a_count_shows_no_count() {
    let mut session = running();
    feed(&mut session, r#"{"type":"auto_retry_start"}"#);
    assert_eq!(
        session.retrying(),
        Some(&Retrying {
            attempt: None,
            max_attempts: None,
            reason: String::new(),
        })
    );
}

#[test]
fn a_retry_before_the_next_message_keeps_the_last_good_one() {
    let mut session = running();
    message_start(&mut session);
    text(&mut session, "Done reading. ");
    message_end(&mut session, "toolUse");

    feed(&mut session, RETRY);

    assert_eq!(session.reply(), "Done reading. ");
}

#[test]
fn a_retry_in_the_first_message_of_a_new_prompt_drops_only_that_attempt() {
    let mut session = running();
    message_start(&mut session);
    text(&mut session, "an old reply that was long");
    message_end(&mut session, "stop");
    session.apply_action(&ClientAction::PromptSent);
    message_start(&mut session);
    text(&mut session, "née");
    message_end(&mut session, "toolUse");
    message_start(&mut session);
    text(&mut session, "ça");
    message_end(&mut session, "error");

    feed(&mut session, RETRY);

    assert_eq!(
        session.reply(),
        "née",
        "nothing of the old reply comes back and the cut lands on a char boundary"
    );
}

#[test]
fn the_retry_notice_ends_on_every_way_out() {
    let ends: [End; 7] = [
        ("auto_retry_end", |s| {
            feed(
                s,
                r#"{"type":"auto_retry_end","success":false,"attempt":3,"finalError":"529"}"#,
            );
        }),
        ("prompt_result", |s| {
            feed(
                s,
                r#"{"id":"req_2","type":"prompt_result","status":"aborted","sessionSettled":true}"#,
            );
        }),
        ("session_settled", |s| {
            feed(s, r#"{"type":"session_settled"}"#);
        }),
        ("stop", |s| s.apply_action(&ClientAction::StopSent)),
        ("ch_stopped", |s| {
            feed(s, r#"{"type":"ch_stopped","id":"s1","exit_code":0}"#);
        }),
        ("ch_omp_exited", |s| {
            feed(
                s,
                r#"{"type":"ch_omp_exited","exit_code":1,"stderr_tail":[]}"#,
            );
        }),
        ("new prompt", |s| s.apply_action(&ClientAction::PromptSent)),
    ];
    for (name, end) in ends {
        let mut session = retrying();
        end(&mut session);
        assert_eq!(session.retrying(), None, "still retrying after {name}");
    }
}

#[test]
fn a_retry_after_the_session_failed_is_ignored() {
    let mut session = running();
    feed(
        &mut session,
        r#"{"id":"req_1","type":"response","command":"prompt","success":false,"error":"busy"}"#,
    );

    feed(&mut session, RETRY);

    assert_eq!(session.retrying(), None);
    assert_eq!(session.state(), SessionState::Failed);
}

#[test]
fn a_prompt_error_says_whether_trying_again_can_help() {
    use FailureKind::{NeedsHuman, Permanent, Transient};
    let cases = [
        (
            json!({"message": "rate limited", "retryable": true}),
            Transient,
        ),
        (
            json!({"message": "bad request", "retryable": false}),
            Permanent,
        ),
        (json!({"message": "no flag"}), Permanent),
        (json!("a bare string"), Permanent),
        (
            json!({"message": "cost limit reached", "code": "cost_cap", "retryable": false}),
            NeedsHuman,
        ),
        (
            json!({"message": "cost limit reached", "code": "cost_cap", "retryable": true}),
            NeedsHuman,
        ),
        (
            json!({"message": "invalid key", "httpStatus": 401}),
            NeedsHuman,
        ),
        (
            json!({"message": "payment required", "httpStatus": 402, "retryable": true}),
            NeedsHuman,
        ),
        (
            json!({"message": "forbidden", "httpStatus": 403, "retryable": false}),
            NeedsHuman,
        ),
        (
            json!({"message": "server error", "httpStatus": 500, "retryable": true}),
            Transient,
        ),
    ];
    for (error, kind) in cases {
        let session = prompt_error(&error);
        let failure = session.failure().expect("a failure");
        assert_eq!(failure.kind, kind, "for {error}");
        assert_eq!(failure.stage, "prompt");
    }
}

#[test]
fn failures_omp_did_not_classify_are_permanent() {
    let mut rejected = running();
    feed(
        &mut rejected,
        r#"{"id":"req_1","type":"response","command":"prompt","success":false,"error":"busy"}"#,
    );
    let mut crashed = running();
    feed(
        &mut crashed,
        r#"{"type":"ch_omp_exited","exit_code":1,"stderr_tail":[]}"#,
    );
    let mut broken = running();
    feed(
        &mut broken,
        r#"{"type":"ch_error","stage":"start","message":"omp not found"}"#,
    );
    for session in [rejected, crashed, broken] {
        assert_eq!(
            session.failure().map(|f| f.kind),
            Some(FailureKind::Permanent)
        );
    }
}
