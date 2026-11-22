use ch_protocol::runner::route_runner_line;

use super::*;

const FIXTURE: &str = include_str!("../../../../testdata/omp/approval-session.jsonl");
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
        kind: FailureKind::Permanent,
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
    assert_eq!(
        session.failure(),
        Some(&Failure {
            kind: FailureKind::Transient,
            ..failure("prompt", "rate limited")
        })
    );

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

mod checkpoints;
mod retry;
