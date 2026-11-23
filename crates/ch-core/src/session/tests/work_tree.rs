//! Which working tree the latest diff shows, and whether the agent may have run since it was
//! asked for.

use serde_json::{Value, json};

use super::{feed, running};
use crate::session::{ClientAction, Session, SessionState};

const SETTLED: &str = r#"{"type":"session_settled"}"#;

fn diff_result(tree: Option<&str>, range: &Value) -> String {
    let mut frame = json!({ "type": "ch_diff_result", "id": "d1", "base": "abc", "stat": "1 file",
        "patch": "+x", "truncated": false });
    if let Some(frame) = frame.as_object_mut() {
        if let Some(tree) = tree {
            frame.insert("tree".into(), json!(tree));
        }
        if let Some(range) = range.as_object() {
            frame.extend(range.clone());
        }
    }
    frame.to_string()
}

fn ask(session: &mut Session, id: &str) {
    session.apply_action(&ClientAction::DiffSent { id: id.into() });
}

fn answer(session: &mut Session, id: &str, tree: Option<&str>) {
    feed(session, &diff_result(tree, &json!({ "id": id })));
}

/// A plain diff asked for and answered with nothing in between.
fn plain_diff(session: &mut Session, tree: Option<&str>) {
    ask(session, "d1");
    answer(session, "d1", tree);
}

fn reviewing() -> Session {
    let mut session = running();
    feed(&mut session, SETTLED);
    assert_eq!(session.state(), SessionState::NeedsReview);
    session
}

/// The tree of the current diff, or why there is none to read.
fn current_tree(session: &Session) -> Result<Option<&str>, &str> {
    session
        .current_diff()
        .map(|d| d.tree.as_deref())
        .ok_or("no current diff")
}

#[test]
fn a_diff_keeps_the_tree_the_runner_wrote() {
    let mut session = reviewing();
    plain_diff(&mut session, Some("t1"));
    assert_eq!(current_tree(&session), Ok(Some("t1")));
    assert_eq!(
        session.last_diff().and_then(|d| d.tree.as_deref()),
        Some("t1")
    );
}

#[test]
fn a_diff_from_an_older_runner_has_no_tree() {
    let mut session = reviewing();
    plain_diff(&mut session, None);
    assert_eq!(current_tree(&session), Ok(None));
}

#[test]
fn a_prompt_leaves_the_diff_shown_but_not_current() {
    let mut session = reviewing();
    plain_diff(&mut session, Some("t1"));
    session.apply_action(&ClientAction::PromptSent);
    assert_eq!(session.state(), SessionState::Running);
    assert_eq!(session.current_diff(), None);
    assert!(session.last_diff().is_some(), "the diff view keeps it");

    feed(&mut session, SETTLED);
    assert_eq!(
        session.current_diff(),
        None,
        "settling does not bring it back"
    );
}

#[test]
fn an_agent_start_makes_the_diff_stale() {
    let mut session = Session::new();
    session.apply_action(&ClientAction::StartSent);
    feed(&mut session, r#"{"type":"ready"}"#);
    plain_diff(&mut session, Some("t1"));
    assert_eq!(current_tree(&session), Ok(Some("t1")));

    feed(&mut session, r#"{"type":"agent_start"}"#);
    assert_eq!(session.current_diff(), None);
}

#[test]
fn a_diff_taken_after_the_run_is_current() {
    let mut session = reviewing();
    plain_diff(&mut session, Some("t1"));
    session.apply_action(&ClientAction::PromptSent);
    feed(&mut session, SETTLED);
    plain_diff(&mut session, Some("t2"));
    assert_eq!(current_tree(&session), Ok(Some("t2")));
}

#[test]
fn a_diff_that_arrives_while_the_agent_runs_is_not_current() {
    let mut session = running();
    plain_diff(&mut session, Some("t1"));
    assert_eq!(session.current_diff(), None);
    feed(&mut session, SETTLED);
    assert_eq!(session.current_diff(), None);
}

#[test]
fn a_diff_asked_before_a_prompt_is_not_current_when_it_lands_after_the_run() {
    let mut session = reviewing();
    ask(&mut session, "d1");
    session.apply_action(&ClientAction::PromptSent);
    feed(&mut session, SETTLED);
    answer(&mut session, "d1", Some("t1"));
    assert_eq!(session.current_diff(), None);
    assert_eq!(
        session.last_diff().and_then(|d| d.tree.as_deref()),
        Some("t1"),
        "the diff view still shows it"
    );
}

#[test]
fn only_a_diff_asked_after_the_run_is_current() {
    let mut session = reviewing();
    ask(&mut session, "d1");
    session.apply_action(&ClientAction::PromptSent);
    feed(&mut session, SETTLED);
    ask(&mut session, "d2");
    answer(&mut session, "d1", Some("t1"));
    assert_eq!(session.current_diff(), None);
    answer(&mut session, "d2", Some("t2"));
    assert_eq!(current_tree(&session), Ok(Some("t2")));
}

#[test]
fn a_diff_asked_while_the_agent_runs_is_not_current_after_it_settles() {
    let mut session = running();
    ask(&mut session, "d1");
    feed(&mut session, SETTLED);
    answer(&mut session, "d1", Some("t1"));
    assert_eq!(session.current_diff(), None);
}

#[test]
fn a_diff_reply_nobody_asked_for_is_not_current() {
    let mut session = reviewing();
    answer(&mut session, "d9", Some("t1"));
    assert_eq!(session.current_diff(), None);
}

#[test]
fn a_failed_diff_is_no_longer_waited_for() {
    let mut session = reviewing();
    ask(&mut session, "d1");
    feed(
        &mut session,
        r#"{"type":"ch_error","id":"d1","stage":"diff","message":"git diff failed"}"#,
    );
    answer(&mut session, "d1", Some("t1"));
    assert_eq!(session.current_diff(), None);
}

#[test]
fn a_checkpoint_range_leaves_the_current_diff_alone() {
    let mut session = reviewing();
    plain_diff(&mut session, Some("t1"));
    feed(
        &mut session,
        &diff_result(Some("t2"), &json!({ "from": 1 })),
    );
    assert_eq!(current_tree(&session), Ok(Some("t1")));
}

#[test]
fn a_restore_clears_the_current_diff() {
    let mut session = reviewing();
    plain_diff(&mut session, Some("t1"));
    feed(&mut session, r#"{"type":"ch_restored","id":"r1","turn":1}"#);
    assert_eq!(session.current_diff(), None);
}
