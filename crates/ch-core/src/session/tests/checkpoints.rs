//! Checkpoints, turn diffs and restores as the session sees them.

use ch_protocol::runner::{Checkpoint, CheckpointKind, MAX_CHECKPOINTS};
use serde_json::{Value, json};

use super::{feed, running};
use crate::checkpoints::{RESTORE_NOTICE, previous_turn, run_ended};
use crate::session::{Diff, Session, SessionState, TurnDiff};

fn checkpoint(turn: u32, kind: CheckpointKind) -> Checkpoint {
    Checkpoint {
        turn,
        commit: format!("{turn:040}"),
        kind,
        at: 1,
    }
}

fn say(session: &mut Session, frame: &Value) {
    feed(session, &frame.to_string());
}

fn listed(session: &mut Session, checkpoints: &[Checkpoint]) {
    say(
        session,
        &json!({ "type": "ch_checkpoints", "checkpoints": checkpoints }),
    );
}

fn saved(session: &mut Session, id: &str, checkpoint: &Checkpoint) {
    say(
        session,
        &json!({ "type": "ch_checkpointed", "id": id, "checkpoint": checkpoint }),
    );
}

fn reviewing() -> Session {
    let mut session = running();
    feed(&mut session, r#"{"type":"session_settled"}"#);
    assert_eq!(session.state(), SessionState::NeedsReview);
    session
}

fn turns(session: &Session) -> Vec<u32> {
    session.checkpoints().iter().map(|c| c.turn).collect()
}

fn diff(stat: &str) -> Diff {
    Diff {
        base: "abc".into(),
        stat: stat.into(),
        patch: "+x".into(),
        truncated: false,
    }
}

fn diff_result(range: &Value, stat: &str) -> Value {
    let mut frame = json!({ "type": "ch_diff_result", "id": "d1", "base": "abc", "stat": stat,
        "patch": "+x", "truncated": false });
    if let (Some(frame), Some(range)) = (frame.as_object_mut(), range.as_object()) {
        frame.extend(range.clone());
    }
    frame
}

#[test]
fn checkpoints_are_off_until_the_runner_lists_them() {
    let mut session = reviewing();
    assert!(!session.checkpoints_on());
    assert_eq!(turns(&session), Vec::<u32>::new());

    saved(&mut session, "c1", &checkpoint(1, CheckpointKind::Run));
    assert!(!session.checkpoints_on(), "a stray save turned them on");

    listed(&mut session, &[checkpoint(1, CheckpointKind::Run)]);
    assert!(session.checkpoints_on());
    assert_eq!(turns(&session), [1]);

    session.apply_action(&crate::ClientAction::StartSent);
    assert!(!session.checkpoints_on(), "a new start keeps the old list");
}

#[test]
fn a_saved_checkpoint_joins_the_list_once() {
    let mut session = reviewing();
    listed(&mut session, &[checkpoint(1, CheckpointKind::Run)]);
    saved(&mut session, "c2", &checkpoint(2, CheckpointKind::Run));
    saved(&mut session, "c2", &checkpoint(2, CheckpointKind::Run));
    saved(
        &mut session,
        "r1",
        &checkpoint(3, CheckpointKind::BeforeRestore),
    );
    assert_eq!(turns(&session), [1, 2, 3]);
    assert_eq!(session.checkpoints()[2].kind, CheckpointKind::BeforeRestore);
}

#[test]
fn the_list_keeps_the_newest_checkpoints() {
    let mut session = reviewing();
    let all: Vec<Checkpoint> = (1..=u32::try_from(MAX_CHECKPOINTS).expect("fits"))
        .map(|turn| checkpoint(turn, CheckpointKind::Run))
        .collect();
    listed(&mut session, &all);
    let next = u32::try_from(MAX_CHECKPOINTS + 1).expect("fits");
    saved(&mut session, "c9", &checkpoint(next, CheckpointKind::Run));
    assert_eq!(session.checkpoints().len(), MAX_CHECKPOINTS);
    assert_eq!(session.checkpoints()[0].turn, 2);
    assert_eq!(session.checkpoints().last().map(|c| c.turn), Some(next));
}

#[test]
fn ranged_diffs_go_to_the_turn_diff() {
    let mut session = reviewing();
    say(&mut session, &diff_result(&json!({}), "full"));
    say(
        &mut session,
        &diff_result(&json!({ "from": 1, "to": 2 }), "turn"),
    );
    assert_eq!(session.last_diff(), Some(&diff("full")));
    assert_eq!(
        session.turn_diff(),
        Some(&TurnDiff {
            from: 1,
            to: Some(2),
            diff: diff("turn"),
        })
    );

    say(&mut session, &diff_result(&json!({ "from": 2 }), "since"));
    let since = session.turn_diff().expect("a turn diff");
    assert_eq!((since.from, since.to), (2, None));
    say(&mut session, &diff_result(&json!({ "to": 2 }), "upto"));
    let upto = session.turn_diff().expect("a turn diff");
    assert_eq!((upto.from, upto.to), (0, Some(2)), "no from is the base");
    assert_eq!(session.last_diff(), Some(&diff("full")));
}

#[test]
fn a_restore_clears_diffs_and_says_where_the_tree_is() {
    let mut session = reviewing();
    listed(&mut session, &[checkpoint(1, CheckpointKind::Run)]);
    say(&mut session, &diff_result(&json!({}), "full"));
    say(
        &mut session,
        &diff_result(&json!({ "from": 0, "to": 1 }), "turn"),
    );

    saved(
        &mut session,
        "r1",
        &checkpoint(2, CheckpointKind::BeforeRestore),
    );
    say(
        &mut session,
        &json!({ "type": "ch_restored", "id": "r1", "turn": 1 }),
    );
    assert_eq!(session.last_diff(), None);
    assert_eq!(session.turn_diff(), None);
    assert_eq!(session.state(), SessionState::NeedsReview);
    let notes = restore_notes(&session);
    assert_eq!(notes.len(), 1);
    for part in ["checkpoint 1", "Checkpoint 2 holds", "checks again"] {
        assert!(notes[0].contains(part), "no {part:?}: {}", notes[0]);
    }

    saved(
        &mut session,
        "r2",
        &checkpoint(3, CheckpointKind::BeforeRestore),
    );
    say(
        &mut session,
        &json!({ "type": "ch_restored", "id": "r2", "turn": 0 }),
    );
    let notes = restore_notes(&session);
    assert_eq!(notes.len(), 1, "an older restore notice stayed: {notes:?}");
    assert!(notes[0].contains("where the session started"), "{notes:?}");
    assert!(notes[0].contains("Checkpoint 3 holds"), "{notes:?}");
}

fn restore_notes(session: &Session) -> Vec<&str> {
    session
        .notices()
        .iter()
        .filter(|n| n.kind == RESTORE_NOTICE)
        .map(|n| n.message.as_str())
        .collect()
}

#[test]
fn checkpoint_and_restore_errors_are_warnings() {
    let mut session = reviewing();
    for stage in ["checkpoint", "restore"] {
        say(
            &mut session,
            &json!({ "type": "ch_error", "id": "x", "stage": stage, "message": "no" }),
        );
    }
    assert_eq!(session.state(), SessionState::NeedsReview);
    let stages: Vec<&str> = session
        .warnings()
        .iter()
        .map(|w| w.stage.as_str())
        .collect();
    assert_eq!(stages, ["checkpoint", "restore"]);
}

#[test]
fn a_warning_already_shown_is_not_added_again() {
    let mut session = reviewing();
    let full = json!({
        "type": "ch_error", "id": "x", "stage": "checkpoint",
        "message": "this session has 1000 checkpoints, the most it keeps",
    });
    let restore = json!({ "type": "ch_error", "id": "y", "stage": "restore", "message": "no" });
    for frame in [&full, &full, &restore, &full] {
        say(&mut session, frame);
    }
    let stages: Vec<&str> = session
        .warnings()
        .iter()
        .map(|w| w.stage.as_str())
        .collect();
    assert_eq!(stages, ["checkpoint", "restore"]);
}

#[test]
fn a_turn_diff_starts_at_the_checkpoint_before_it() {
    let list = [
        checkpoint(1, CheckpointKind::Run),
        checkpoint(2, CheckpointKind::Run),
        checkpoint(4, CheckpointKind::BeforeRestore),
        checkpoint(5, CheckpointKind::Run),
    ];
    assert_eq!(previous_turn(&list, 1), 0);
    assert_eq!(previous_turn(&list, 4), 2);
    assert_eq!(previous_turn(&list, 5), 4);
    assert_eq!(
        previous_turn(&list, 9),
        0,
        "an unknown turn starts at the base"
    );
}

#[test]
fn only_a_run_that_ends_in_review_takes_a_checkpoint() {
    use SessionState::{Failed, Idle, Landed, NeedsReview, NeedsYou, Running, Starting};
    assert!(run_ended(Running, NeedsReview));
    assert!(run_ended(NeedsYou, NeedsReview));
    for (before, after) in [
        (Idle, NeedsReview),
        (Starting, NeedsReview),
        (NeedsReview, NeedsReview),
        (Running, Running),
        (Running, Failed),
        (NeedsReview, Landed),
    ] {
        assert!(!run_ended(before, after), "{before:?} -> {after:?}");
    }
}
