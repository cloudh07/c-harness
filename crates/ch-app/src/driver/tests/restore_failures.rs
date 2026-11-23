//! Restores and marks that fail partway: omp stopping, omp lacking a command, a slow runner,
//! and a runner that refuses after it saved the tree it was replacing.

use std::sync::atomic::{AtomicU64, Ordering};

use ch_core::{ENTRIES_COMMAND, HISTORY_COMMAND};

use super::checkpoints::{answer_save, checkpoint, first_run, flush, saves, wait_sent};
use super::restore::{
    FakeOmp, LABEL, NAVIGATE, next_run, noted, restore_both, restored, saved_run_on, sent_after,
    start, texts,
};
use super::*;

fn count(rig: &Rig, text: &str) -> usize {
    rig.driver.log().filter(|line| line.contains(text)).count()
}

/// Stops omp and waits until the runner says it ended.
fn stop_omp(rig: &mut Rig) {
    let stops = of_type(&rig.sent(), "ch_stop").len();
    assert!(rig.driver.stop());
    let stop = wait_sent(rig, "ch_stop", stops + 1).remove(stops);
    rig.say(&json!({"type":"ch_stopped","id":stop["id"],"exit_code":0}));
    rig.pump_until("omp to end", |d, _| !d.stopping());
}

/// Answers the diff refresh that follows the runner's answer to a restore.
fn answer_refresh(rig: &mut Rig, diffs: usize) {
    let refresh = wait_sent(rig, "ch_diff", diffs + 1).remove(diffs);
    rig.say(&diff_result(&refresh["id"], "refreshed"));
}

#[test]
fn a_stopped_omp_can_still_be_reviewed_but_not_restored() {
    let mut rig = Rig::new();
    let _omp = saved_run_on(&mut rig, FakeOmp::new());
    assert!(rig.driver.can_restore());

    stop_omp(&mut rig);
    assert!(rig.driver.can_review(), "diff, verify and land need no omp");
    assert!(
        !rig.driver.can_restore(),
        "a stopped omp can't move the conversation"
    );
    assert!(!rig.driver.restore(1));
}

#[test]
fn an_omp_without_set_label_is_told_once_and_not_read_again() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::old());
    let reads = of_type(&rig.sent(), ENTRIES_COMMAND).len();
    let lacks = "omp does not know the set_label command, so checkpoints can't be marked";
    assert_eq!(count(&rig, lacks), 1);

    next_run(&mut rig, &mut omp, 2);
    flush(&mut rig);
    let sent = rig.sent();
    assert_eq!(of_type(&sent, ENTRIES_COMMAND).len(), reads, "{sent:?}");
    assert_eq!(of_type(&sent, LABEL).len(), 1, "{sent:?}");
    assert_eq!(count(&rig, lacks), 1, "the second run adds no note");

    let probe = of_type(&sent, "ch_diff").last().expect("the probe")["id"].clone();
    rig.say(&diff_result(&probe, "probe"));
    rig.pump_until("the probe", |d, _| d.can_restore());
    assert!(rig.driver.restore(2));
    assert!(noted(
        &rig,
        "omp does not know the set_label command, which a restore needs. Code and conversation \
         were left unchanged."
    ));
    assert!(!rig.driver.restoring());
}

static MARK_NOW: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_mark_omp_never_answers_is_reported() {
    let mut rig = Rig::with_clock(|| MARK_NOW.load(Ordering::SeqCst));
    let mut omp = FakeOmp::new();
    omp.silent.push(ENTRIES_COMMAND);
    first_run(&mut rig, true);
    omp.ran(1);
    omp.wait_sent(&mut rig, ENTRIES_COMMAND, 1);
    let request = saves(&rig)[0].clone();
    answer_save(&mut rig, &request, 1);
    assert!(rig.driver.saving_checkpoint(), "the mark is still out");

    MARK_NOW.fetch_add(15_000, Ordering::SeqCst);
    let text = "Checkpoint 1 could not be marked in the conversation, so it can't be restored: \
                omp did not answer in time";
    rig.pump_until("the mark to give up", |d, _| {
        d.log().any(|line| line.contains(text))
    });
    assert!(!rig.driver.saving_checkpoint());
}

#[test]
fn a_run_that_left_nothing_in_the_conversation_is_not_marked() {
    let mut rig = Rig::new();
    let mut omp = FakeOmp::new();
    first_run(&mut rig, true);
    omp.wait_sent(&mut rig, ENTRIES_COMMAND, 1);
    let request = saves(&rig)[0].clone();
    answer_save(&mut rig, &request, 1);
    omp.settle(&mut rig);

    assert!(noted(
        &rig,
        "Checkpoint 1 could not be marked in the conversation, so it can't be restored: the \
         conversation holds nothing from this run to mark"
    ));
    let sent = rig.sent();
    let marks = of_type(&sent, LABEL);
    assert!(marks.is_empty(), "{marks:?}");
}

static CODE_NOW: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_slow_code_restore_is_noted_once_and_still_finishes() {
    let mut rig = Rig::with_clock(|| CODE_NOW.load(Ordering::SeqCst));
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    let id = start(&mut rig, &mut omp, 1);
    let slow = "The runner is still restoring the code of checkpoint 1.";
    CODE_NOW.fetch_add(119_999, Ordering::SeqCst);
    rig.driver.pump();
    assert_eq!(count(&rig, slow), 0);

    CODE_NOW.fetch_add(1, Ordering::SeqCst);
    rig.pump_until("the slow note", |d, _| {
        d.log().any(|line| line.contains(slow))
    });
    CODE_NOW.fetch_add(120_000, Ordering::SeqCst);
    rig.driver.pump();
    assert_eq!(count(&rig, slow), 1);
    assert!(
        rig.driver.restoring(),
        "only the runner's answer ends the code step"
    );
    assert!(!rig.driver.can_prompt());

    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    restored(&mut rig, &mut omp, &id, 1);
    assert!(noted(&rig, "The conversation is back at checkpoint 1 too."));
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
    assert!(rig.driver.can_prompt());
}

#[test]
fn a_refusal_after_the_runner_saved_the_tree_keeps_that_save_restorable() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    let labels = of_type(&rig.sent(), LABEL).len();

    let id = start(&mut rig, &mut omp, 1);
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    let diffs = of_type(&rig.sent(), "ch_diff").len();
    rig.say(&json!({"type":"ch_error","id":id,"stage":"restore","message":"disk full"}));
    answer_refresh(&mut rig, diffs);
    omp.settle(&mut rig);
    assert!(noted(
        &rig,
        "The runner stopped partway through restoring checkpoint 1, so the conversation was moved \
         back. The code may be partly changed; checkpoint 3 holds it as it was before the \
         restore."
    ));
    let sent = rig.sent();
    let marks = of_type(&sent, LABEL);
    assert_eq!(marks.len(), labels + 1, "{sent:?}");
    assert_eq!(marks[labels]["label"], "c-harness:turn:3");
    let both = texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"]);
    assert_eq!(omp.path(), both);

    rig.pump_until("the refreshed diff", |d, _| d.can_restore());
    restore_both(&mut rig, &mut omp, 3, 4);
    assert_eq!(omp.path(), both);
}

#[test]
fn a_move_omp_will_not_save_still_reloads_the_thread() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    omp.fail_next(LABEL, "disk full", None);
    omp.fail_next(LABEL, "disk full", None);

    let id = start(&mut rig, &mut omp, 1);
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    rig.say(&json!({"type":"ch_restored","id":id,"turn":1}));
    omp.settle(&mut rig);
    assert!(noted(
        &rig,
        "omp did not save the move: disk full. If this session is resumed, the conversation will \
         reopen where it was before the restore. The thread now shows the conversation as the \
         agent sees it."
    ));
    assert!(sent_after(&rig, LABEL, HISTORY_COMMAND));
    let reloaded = rig.driver.take_history().expect("the reload ended");
    assert!(reloaded.replaces);
    assert!(reloaded.problem.is_none(), "{:?}", reloaded.problem);
}

#[test]
fn omp_stopping_during_the_move_leaves_both_halves_unchanged() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    omp.silent.push(NAVIGATE);
    assert!(rig.driver.restore(1));
    omp.wait_sent(&mut rig, NAVIGATE, 1);
    stop_omp(&mut rig);
    rig.pump_until("the restore to end", |d, _| !d.rewinding());

    assert!(noted(
        &rig,
        "omp stopped during the restore. Code and conversation were left unchanged."
    ));
    let sent = rig.sent();
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert!(!sent_after(&rig, NAVIGATE, HISTORY_COMMAND), "{sent:?}");
    assert!(!rig.driver.can_restore());
}

#[test]
fn omp_stopping_before_it_confirms_the_save_says_the_move_may_be_lost() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    let labels = of_type(&rig.sent(), LABEL).len();

    let id = start(&mut rig, &mut omp, 1);
    omp.silent.push(LABEL);
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    rig.say(&json!({"type":"ch_restored","id":id,"turn":1}));
    omp.wait_sent(&mut rig, LABEL, labels + 1);
    stop_omp(&mut rig);
    rig.pump_until("the restore to end", |d, _| !d.rewinding());

    assert!(noted(
        &rig,
        "The code is back at checkpoint 1, but omp stopped before it confirmed the \
         conversation's move was saved. If this session is resumed, the conversation may reopen \
         at checkpoint 1 or where it was before the restore."
    ));
    assert!(!sent_after(&rig, LABEL, HISTORY_COMMAND));
}
