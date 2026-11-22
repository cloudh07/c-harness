//! A run that ends with no reply and no tool calls, and whether the model has said anything yet.

use serde_json::json;

use super::*;

const SETTLED: &str = r#"{"type":"prompt_result","status":"completed","sessionSettled":true}"#;
const TOOL: &str = r#"{"type":"tool_execution_start","toolCallId":"t1","toolName":"read"}"#;
const RETRY: &str = r#"{"type":"auto_retry_start","attempt":2,"maxAttempts":3,"delayMs":4000,"errorMessage":"529 overloaded"}"#;

fn delta(session: &mut Session, kind: &str, text: &str) {
    let frame = json!({
        "type": "message_update",
        "assistantMessageEvent": {"type": kind, "delta": text}
    });
    feed(session, &frame.to_string());
}

fn message_end(session: &mut Session, stop_reason: &str) {
    let frame = json!({
        "type": "message_end",
        "message": {"role": "assistant", "stopReason": stop_reason}
    });
    feed(session, &frame.to_string());
}

#[test]
fn a_run_with_no_reply_and_no_tool_calls_was_empty() {
    let mut session = running();
    feed(&mut session, SETTLED);
    assert_eq!(session.state(), SessionState::NeedsReview);
    assert!(session.run_was_empty());
}

#[test]
fn a_reply_or_a_tool_call_makes_a_run_count() {
    let mut replied = running();
    delta(&mut replied, "text_delta", "Done.");
    feed(&mut replied, SETTLED);
    assert!(!replied.run_was_empty(), "a reply");

    let mut worked = running();
    feed(&mut worked, TOOL);
    feed(&mut worked, SETTLED);
    assert!(!worked.run_was_empty(), "a tool call and no text");
}

#[test]
fn a_reply_of_only_whitespace_is_empty() {
    let mut session = running();
    delta(&mut session, "text_delta", "\n  ");
    feed(&mut session, SETTLED);
    assert!(session.run_was_empty());
}

#[test]
fn tool_calls_from_an_earlier_run_do_not_count() {
    let mut session = running();
    feed(&mut session, TOOL);
    feed(&mut session, SETTLED);
    session.apply_action(&ClientAction::PromptSent);
    feed(&mut session, SETTLED);
    assert!(session.run_was_empty());
}

#[test]
fn only_a_finished_run_after_a_prompt_can_be_empty() {
    assert!(!running().run_was_empty(), "still running");

    let mut failed = running();
    feed(
        &mut failed,
        r#"{"type":"prompt_result","status":"error","error":{"message":"boom"},"sessionSettled":true}"#,
    );
    assert_eq!(failed.state(), SessionState::Failed);
    assert!(!failed.run_was_empty(), "a failure says why already");

    let mut unprompted = Session::new();
    unprompted.apply_action(&ClientAction::StartSent);
    feed(&mut unprompted, r#"{"type":"ready"}"#);
    unprompted.apply_action(&ClientAction::StopSent);
    feed(
        &mut unprompted,
        r#"{"type":"ch_stopped","id":"s1","exit_code":0}"#,
    );
    assert_eq!(unprompted.state(), SessionState::NeedsReview);
    assert!(!unprompted.run_was_empty(), "stopped before any prompt");
}

#[test]
fn the_wait_starts_with_the_prompt_and_ends_with_any_word_from_the_model() {
    let mut session = running();
    let wait = session.reply_wait().expect("waiting after a prompt");
    assert_eq!(wait.delay_ms, 0);

    delta(&mut session, "thinking_delta", "Let me look");
    assert_eq!(session.reply_wait(), None, "thinking counts");
}

#[test]
fn a_tool_call_or_a_message_that_did_not_fail_ends_the_wait() {
    let mut tool = running();
    feed(&mut tool, TOOL);
    assert_eq!(tool.reply_wait(), None);

    let mut ended = running();
    message_end(&mut ended, "stop");
    assert_eq!(ended.reply_wait(), None);

    let mut failed = running();
    message_end(&mut failed, "error");
    assert!(
        failed.reply_wait().is_some(),
        "a failed attempt said nothing"
    );
}

#[test]
fn omp_ending_the_prompts_own_message_does_not_end_the_wait() {
    let mut session = running();
    let frame = json!({"type": "message_end", "message": {"role": "user"}});
    feed(&mut session, &frame.to_string());
    assert!(session.reply_wait().is_some());
}

#[test]
fn a_retry_starts_a_new_wait_after_omps_delay() {
    let mut session = running();
    let first = session.reply_wait().expect("waiting after a prompt");
    delta(&mut session, "text_delta", "The fix is to cha");
    message_end(&mut session, "error");

    feed(&mut session, RETRY);

    let wait = session.reply_wait().expect("waiting on the retry");
    assert_ne!(wait.epoch, first.epoch);
    assert_eq!(wait.delay_ms, 4000);
}

#[test]
fn a_new_prompt_starts_a_new_wait_with_no_delay() {
    let mut session = running();
    feed(&mut session, RETRY);
    let retry = session.reply_wait().expect("waiting on the retry");
    session.apply_action(&ClientAction::PromptSent);
    let wait = session.reply_wait().expect("waiting after a prompt");
    assert_ne!(wait.epoch, retry.epoch);
    assert_eq!(wait.delay_ms, 0);
}

#[test]
fn a_compaction_restarts_the_wait() {
    let mut session = running();
    let before = session.reply_wait().expect("waiting after a prompt");
    feed(
        &mut session,
        r#"{"type":"auto_compaction_start","reason":"threshold"}"#,
    );
    let during = session.reply_wait().expect("still waiting");
    feed(
        &mut session,
        r#"{"type":"auto_compaction_end","aborted":false}"#,
    );
    let after = session.reply_wait().expect("still waiting");
    assert_ne!(before.epoch, during.epoch);
    assert_ne!(during.epoch, after.epoch);
}

#[test]
fn nothing_is_awaited_outside_a_run_or_while_an_approval_is_open() {
    let mut idle = Session::new();
    idle.apply_action(&ClientAction::StartSent);
    assert_eq!(idle.reply_wait(), None, "starting");
    feed(&mut idle, r#"{"type":"ready"}"#);
    assert_eq!(idle.reply_wait(), None, "ready, no prompt");

    let mut asked = running();
    feed(&mut asked, APPROVAL);
    assert_eq!(asked.reply_wait(), None, "waiting on a person");
    asked.apply_action(&ClientAction::ApprovalAnswered { id: "ui_1".into() });
    assert!(asked.reply_wait().is_some(), "back to the model");

    let mut done = running();
    feed(&mut done, SETTLED);
    assert_eq!(done.reply_wait(), None, "finished");
}
