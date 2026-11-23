//! Checkpoints after each run, restores and turn diffs, as the driver sends and matches them.

use std::sync::atomic::{AtomicU64, Ordering};

use ch_core::RESTORE_NOTICE;

use super::restore::{FakeOmp, saved_run_on};
use super::*;

pub(super) fn checkpoint(turn: u32, kind: &str) -> Value {
    json!({ "turn": turn, "commit": format!("{turn:040}"), "kind": kind, "at": 1 })
}

/// Pumps until `count` frames of `kind` were written, and returns them. The link writes on
/// its own thread, so a frame the driver sent may not be in `sent` yet.
pub(super) fn wait_sent(rig: &mut Rig, kind: &str, count: usize) -> Vec<Value> {
    rig.pump_until(kind, |_, sent| of_type(sent, kind).len() == count);
    of_type(&rig.sent(), kind).into_iter().cloned().collect()
}

/// Waits until everything sent so far was written. The writer keeps order, so once a probe
/// diff is out, no earlier frame is still on its way. Leaves the probe open.
pub(super) fn flush(rig: &mut Rig) {
    let count = of_type(&rig.sent(), "ch_diff").len();
    assert!(rig.driver.diff());
    wait_sent(rig, "ch_diff", count + 1);
}

/// Starts a session and lets one run end. `listed` says whether the runner keeps checkpoints.
pub(super) fn first_run(rig: &mut Rig, listed: bool) {
    rig.hello();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    if listed {
        rig.say(&json!({"type":"ch_checkpoints","checkpoints":[]}));
    }
    wait_sent(rig, "prompt", 1);
    end_run(rig);
    if listed {
        wait_sent(rig, "ch_checkpoint", 1);
    }
}

/// Lets a run end with a reply, so it counts as a turn worth a checkpoint. Answers the diff
/// the run's end asks for, so it holds back no land or restore.
pub(super) fn end_run(rig: &mut Rig) {
    let before = open_diffs(rig);
    rig.say(&json!({"type":"agent_start"}));
    rig.say(&json!({"type":"message_update",
        "assistantMessageEvent":{"type":"text_delta","delta":"Done."}}));
    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    answer_run_diff(rig, &before);
}

/// Ids of the whole-session diffs that have no answer yet.
pub(super) fn open_diffs(rig: &Rig) -> Vec<String> {
    rig.driver
        .open
        .iter()
        .filter(|(_, kind)| matches!(kind, Request::Diff))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Answers the diff a run's end asked for: the one open now that was not in `before`.
pub(super) fn answer_run_diff(rig: &mut Rig, before: &[String]) {
    let id = open_diffs(rig)
        .into_iter()
        .find(|id| !before.contains(id))
        .expect("the run's end asked for a diff");
    rig.say(
        &json!({"type":"ch_diff_result","id":id,"base":"b","stat":"","patch":"",
        "truncated":false,"tree":"run"}),
    );
    rig.pump_until("the run's diff", |d, _| !d.open.contains_key(&id));
}

/// One run, saved as checkpoint 1 and marked in omp's conversation.
fn saved_run(rig: &mut Rig) -> FakeOmp {
    saved_run_on(rig, FakeOmp::new())
}

pub(super) fn saves(rig: &Rig) -> Vec<Value> {
    of_type(&rig.sent(), "ch_checkpoint")
        .into_iter()
        .cloned()
        .collect()
}

pub(super) fn answer_save(rig: &mut Rig, request: &Value, turn: u32) {
    rig.say(&json!({"type":"ch_checkpointed","id":request["id"],
        "checkpoint":checkpoint(turn, "run")}));
    rig.pump_until("the saved checkpoint", |d, _| {
        d.session().checkpoints().iter().any(|c| c.turn == turn)
    });
}

#[test]
fn each_run_that_ends_saves_a_checkpoint() {
    let mut rig = Rig::new();
    let mut omp = FakeOmp::new();
    first_run(&mut rig, true);
    omp.ran(1);
    let first = saves(&rig);
    assert_eq!(first.len(), 1, "{:?}", rig.sent());
    assert!(rig.driver.saving_checkpoint());
    assert!(
        !rig.driver.can_prompt(),
        "a prompt could go ahead of the save"
    );

    answer_save(&mut rig, &first[0], 1);
    assert!(
        rig.driver.saving_checkpoint(),
        "a prompt could go ahead of the save's mark in the conversation"
    );
    omp.settle(&mut rig);
    assert!(!rig.driver.saving_checkpoint());
    assert!(rig.driver.can_prompt());

    assert!(rig.driver.prompt("again"));
    end_run(&mut rig);
    wait_sent(&mut rig, "ch_checkpoint", 2);
    assert_eq!(drops(&rig.driver), 0);
}

#[test]
fn a_runner_that_lists_no_checkpoints_is_never_asked_for_them() {
    let mut rig = Rig::new();
    first_run(&mut rig, false);
    assert!(rig.driver.can_prompt());
    assert!(!rig.driver.can_restore());
    assert!(!rig.driver.restore(0));
    assert!(!rig.driver.diff_range(0, None));
    flush(&mut rig);
    let sent = rig.sent();
    assert_eq!(
        of_type(&sent, "ch_checkpoint").len() + of_type(&sent, "ch_restore").len(),
        0,
        "{sent:?}"
    );
    assert_eq!(
        of_type(&sent, "ch_diff").len(),
        2,
        "only the run's diff and the probe went out"
    );
}

#[test]
fn a_failed_save_is_tried_again_after_the_next_run() {
    let mut rig = Rig::new();
    first_run(&mut rig, true);
    let id = saves(&rig)[0]["id"].clone();
    rig.say(&json!({"type":"ch_error","id":id,"stage":"checkpoint","message":"disk full"}));
    rig.pump_until("the warning", |d, _| !d.session().warnings().is_empty());
    assert!(!rig.driver.saving_checkpoint());
    assert!(rig.driver.can_prompt());

    assert!(rig.driver.prompt("again"));
    end_run(&mut rig);
    assert_eq!(wait_sent(&mut rig, "ch_checkpoint", 2).len(), 2);
}

#[test]
fn a_run_with_no_reply_and_no_tool_calls_saves_no_checkpoint() {
    let mut rig = Rig::new();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.say(&json!({"type":"ch_checkpoints","checkpoints":[]}));
    wait_sent(&mut rig, "prompt", 1);
    rig.say(&json!({"type":"agent_start"}));
    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    assert!(rig.driver.session().run_was_empty());

    flush(&mut rig);
    assert_eq!(saves(&rig).len(), 0, "{:?}", rig.sent());
    assert!(rig.driver.can_prompt(), "no save holds the next prompt");

    assert!(rig.driver.prompt("again"));
    end_run(&mut rig);
    wait_sent(&mut rig, "ch_checkpoint", 1);
}

static NOW: AtomicU64 = AtomicU64::new(1);

fn now() -> u64 {
    NOW.load(Ordering::SeqCst)
}

#[test]
fn a_slow_save_holds_prompts_for_a_while_only() {
    let mut rig = Rig::with_clock(now);
    first_run(&mut rig, true);
    NOW.fetch_add(14_999, Ordering::SeqCst);
    assert!(!rig.driver.can_prompt());
    NOW.fetch_add(1, Ordering::SeqCst);
    assert!(
        rig.driver.can_prompt(),
        "a save that never ends held prompts"
    );
    assert!(!rig.driver.saving_checkpoint());
}

#[test]
fn a_restore_holds_the_session_until_it_ends_then_refreshes_the_diff() {
    let mut rig = Rig::new();
    let mut omp = saved_run(&mut rig);
    assert!(rig.driver.can_restore());
    assert!(rig.driver.restore(1));
    let request = omp.wait_sent(&mut rig, "ch_restore", 1).remove(0);
    assert_eq!(request["turn"], 1);
    let id = request["id"].clone();
    assert!(!rig.driver.can_prompt());
    assert!(!rig.driver.can_review());
    assert!(!rig.driver.can_land());
    assert!(!rig.driver.restore(0), "two restores at once");

    // The restore saves the tree it replaces first, under its own id.
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(2, "before_restore")}));
    rig.pump_until("the undo checkpoint", |d, _| {
        d.session().checkpoints().len() == 2
    });
    assert!(
        !rig.driver.can_review(),
        "the restore ended at its first frame"
    );

    rig.say(&json!({"type":"ch_restored","id":id,"turn":1}));
    let refresh = wait_sent(&mut rig, "ch_diff", 1).remove(0);
    assert!(
        refresh.get("from").is_none() && refresh.get("to").is_none(),
        "{refresh}"
    );
    assert!(
        rig.driver.restoring(),
        "omp has not yet saved the conversation's move"
    );
    omp.settle(&mut rig);
    assert!(rig.driver.can_prompt());
    assert!(rig.driver.can_review());
    assert_eq!(drops(&rig.driver), 0);
    let notices = rig.driver.session().notices();
    assert!(
        notices.iter().any(|n| n.kind == RESTORE_NOTICE),
        "{notices:?}"
    );
}

#[test]
fn a_refused_restore_frees_the_session_and_refreshes_the_diff() {
    let mut rig = Rig::new();
    let mut omp = saved_run(&mut rig);
    assert!(rig.driver.restore(1));
    let id = omp.wait_sent(&mut rig, "ch_restore", 1)[0]["id"].clone();
    rig.say(&json!({"type":"ch_error","id":id,"stage":"restore","message":"refused"}));
    wait_sent(&mut rig, "ch_diff", 1);
    omp.settle(&mut rig);
    assert_eq!(
        omp.path(),
        ["prompt 1", "reply 1"],
        "the conversation moved back"
    );
    assert!(rig.driver.can_review());
    assert!(rig.driver.can_prompt());
    assert_eq!(rig.driver.session().state(), SessionState::NeedsReview);
}

#[test]
fn a_restore_waits_for_open_requests_and_for_the_agent() {
    let mut rig = Rig::new();
    saved_run(&mut rig);
    assert!(rig.driver.diff());
    assert!(
        !rig.driver.can_restore(),
        "the runner refuses a restore while a diff runs"
    );
    let id = wait_sent(&mut rig, "ch_diff", 2)[1]["id"].clone();
    rig.say(&diff_result(&id, "1 file"));
    rig.pump_until("the diff", |d, _| {
        d.session()
            .last_diff()
            .is_some_and(|diff| diff.stat == "1 file")
    });
    assert!(rig.driver.can_restore());

    assert!(rig.driver.prompt("again"));
    rig.say(&json!({"type":"agent_start"}));
    rig.pump_until("the agent to run", |d, _| {
        d.session().state() == SessionState::Running
    });
    assert!(!rig.driver.can_restore());
    assert!(!rig.driver.restore(1));
}

#[test]
fn a_turn_diff_reply_must_echo_its_range() {
    let mut rig = Rig::new();
    saved_run(&mut rig);
    let full = rig.driver.session().last_diff().cloned();
    assert!(rig.driver.diff_range(0, Some(1)));
    let request = wait_sent(&mut rig, "ch_diff", 2).remove(1);
    assert_eq!((&request["from"], &request["to"]), (&json!(0), &json!(1)));
    let id = request["id"].clone();

    let mut wrong = diff_result(&id, "turn");
    wrong["from"] = json!(1);
    rig.say(&wrong);
    rig.say(&diff_result(&id, "full"));
    rig.pump_until("two drops", |d, _| drops(d) == 2);
    assert_eq!(rig.driver.session().turn_diff(), None);
    assert_eq!(rig.driver.session().last_diff(), full.as_ref());

    let mut right = diff_result(&id, "turn");
    right["from"] = json!(0);
    right["to"] = json!(1);
    rig.say(&right);
    rig.pump_until("the turn diff", |d, _| d.session().turn_diff().is_some());
    let turn = rig.driver.session().turn_diff().expect("a turn diff");
    assert_eq!((turn.from, turn.to), (0, Some(1)));
    assert_eq!(
        rig.driver.session().last_diff(),
        full.as_ref(),
        "a turn diff is not the full diff"
    );
}
