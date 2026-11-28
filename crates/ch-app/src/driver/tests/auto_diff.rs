//! The diff a run's end asks for, and the land it holds back until the runner answers.
//!
//! The runner refuses a land while a diff runs, so a land asked for in that time waits.

use std::sync::atomic::{AtomicU64, Ordering};

use super::checkpoints::{first_run, open_diffs, wait_sent};
use super::*;
use crate::settings::Settings;
use crate::view_model::{Mark, checklist};

fn diff_result(id: &Value, tree: &str) -> Value {
    json!({"type":"ch_diff_result","id":id,"base":"b","stat":" a | 1 +","patch":"+x",
        "truncated":false,"tree":tree})
}

/// Whether the driver has sent a land. Read from the transcript, which the driver writes
/// as it sends, so a land still on its way to stdin counts too.
fn land_sent(rig: &Rig) -> bool {
    rig.driver.transcript().contains(r#""type":"ch_land""#)
}

/// Lets a run end, then asks for a land, which stops omp. Returns the diff the run's end
/// asked for, still unanswered, once omp has stopped.
fn land_after_settle(rig: &mut Rig) -> Value {
    settle(rig);
    let diff = wait_sent(rig, "ch_diff", 1).remove(0);
    assert!(rig.driver.land(land_request()));
    let stop = wait_sent(rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_stopped","id":stop["id"],"exit_code":0}));
    rig.pump_until("omp to stop", |d, _| !d.stopping());
    diff
}

#[test]
fn a_run_that_ends_asks_for_a_diff_of_the_whole_session() {
    let mut rig = Rig::new();
    settle(&mut rig);
    let diff = wait_sent(&mut rig, "ch_diff", 1).remove(0);
    assert_eq!(diff.get("from"), None, "{diff}");
    assert_eq!(diff.get("to"), None, "{diff}");

    rig.say(&diff_result(&diff["id"], "t1"));
    rig.pump_until("the diff", |d, _| d.session().current_diff().is_some());
    let shown = rig.driver.session().current_diff().unwrap();
    assert_eq!(shown.tree.as_deref(), Some("t1"));
}

/// The run's own diff is enough for the Checks row: a check on the same work passes with no
/// diff asked for by hand.
#[test]
fn a_check_on_the_work_the_run_diff_shows_passes_without_a_click() {
    let mut rig = Rig::new();
    settle(&mut rig);
    let diff = wait_sent(&mut rig, "ch_diff", 1).remove(0);
    rig.say(&diff_result(&diff["id"], "t1"));
    rig.pump_until("the diff", |d, _| d.session().current_diff().is_some());

    assert!(rig.driver.verify("cargo test"));
    let check = wait_sent(&mut rig, "ch_verify", 1).remove(0);
    rig.say(
        &json!({"type":"ch_evidence","id":check["id"],"command":"cargo test","exit_code":0,
        "duration_ms":10,"output_tail":"ok","truncated":false,"timed_out":false,
        "tree_before":"t1","tree_after":"t1"}),
    );
    rig.pump_until("the evidence", |d, _| d.session().evidence().len() == 1);

    let settings = Settings {
        verify: "cargo test".into(),
        ..Settings::default()
    };
    let session = rig.driver.session();
    let list = checklist(
        &settings,
        "s1",
        "hello",
        session.evidence(),
        session.current_diff(),
        session.failure(),
        None,
    );
    let checks = list
        .items
        .iter()
        .find(|item| item.name == "Checks")
        .expect("a Checks row");
    assert_eq!(checks.mark, Mark::Pass, "{checks:?}");
    assert_eq!(
        open_diffs(&rig),
        Vec::<String>::new(),
        "a second diff was asked for"
    );
}

#[test]
fn a_land_waits_for_the_diff_a_run_end_asked_for() {
    let mut rig = Rig::new();
    let diff = land_after_settle(&mut rig);
    assert!(rig.driver.landing());
    assert!(!land_sent(&rig), "the land went out while the diff ran");

    rig.say(&diff_result(&diff["id"], "t1"));
    wait_sent(&mut rig, "ch_land", 1);
    assert!(rig.driver.landing());
}

#[test]
fn a_diff_that_fails_lets_the_held_land_go() {
    let mut rig = Rig::new();
    let diff = land_after_settle(&mut rig);
    assert!(!land_sent(&rig));

    rig.say(&json!({"type":"ch_error","id":diff["id"],"stage":"diff","message":"git failed"}));
    wait_sent(&mut rig, "ch_land", 1);
}

#[test]
fn a_land_after_omp_stopped_waits_for_an_open_diff() {
    let mut rig = Rig::new();
    settle(&mut rig);
    let auto = wait_sent(&mut rig, "ch_diff", 1).remove(0);
    rig.say(&diff_result(&auto["id"], "t1"));
    rig.pump_until("the diff", |d, _| d.session().current_diff().is_some());
    assert!(rig.driver.stop());
    let stop = wait_sent(&mut rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_stopped","id":stop["id"],"exit_code":0}));
    rig.pump_until("omp to stop", |d, _| !d.stopping());

    assert!(rig.driver.diff());
    let asked = wait_sent(&mut rig, "ch_diff", 2).remove(1);
    assert!(rig.driver.land(land_request()));
    rig.driver.pump();
    assert!(rig.driver.landing());
    assert!(!land_sent(&rig), "the land went out while the diff ran");

    rig.say(&diff_result(&asked["id"], "t2"));
    wait_sent(&mut rig, "ch_land", 1);
}

#[test]
fn a_turn_diff_holds_a_land_too() {
    let mut rig = Rig::new();
    first_run(&mut rig, true);
    assert!(rig.driver.session().current_diff().is_some());

    assert!(rig.driver.diff_range(0, None));
    let turn = wait_sent(&mut rig, "ch_diff", 2).remove(1);
    assert_eq!(turn["from"], 0, "{turn}");
    assert!(rig.driver.land(land_request()));
    let stop = wait_sent(&mut rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_stopped","id":stop["id"],"exit_code":0}));
    rig.pump_until("omp to stop", |d, _| !d.stopping());
    assert!(
        !land_sent(&rig),
        "the land went out while the turn diff ran"
    );

    rig.say(
        &json!({"type":"ch_diff_result","id":turn["id"],"base":"b","from":0,
        "stat":"","patch":"","truncated":false,"tree":"t1"}),
    );
    wait_sent(&mut rig, "ch_land", 1);
}

#[test]
fn a_held_land_the_runner_cannot_take_is_noted() {
    let mut rig = Rig::new();
    let diff = land_after_settle(&mut rig);
    rig.driver.link.close_stdin();

    rig.say(&diff_result(&diff["id"], "t1"));
    rig.pump_until("the land to be given up", |d, _| !d.landing());
    assert!(!land_sent(&rig), "a land went out to a closed runner");
    assert!(
        rig.driver
            .log()
            .any(|line| line.contains("The runner is gone, so the land was not sent.")),
        "{:?}",
        rig.driver.log().collect::<Vec<_>>()
    );
}

#[test]
fn a_stop_that_fails_after_omp_exited_drops_the_held_land() {
    let mut rig = Rig::new();
    settle(&mut rig);
    let diff = wait_sent(&mut rig, "ch_diff", 1).remove(0);
    assert!(rig.driver.land(land_request()));
    let stop = wait_sent(&mut rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_omp_exited","exit_code":1,"stderr_tail":[]}));
    rig.pump_until("omp to end", |d, _| !d.stopping());
    assert!(rig.driver.landing(), "the land was not held for the diff");

    rig.say(&json!({"type":"ch_error","id":stop["id"],"stage":"stop","message":"omp hung"}));
    rig.pump_until("the land to be dropped", |d, _| !d.landing());
    rig.say(&diff_result(&diff["id"], "t1"));
    rig.pump_until("the diff", |d, _| d.session().current_diff().is_some());
    assert!(!land_sent(&rig), "the land went out after the stop failed");
}

#[test]
fn a_diff_that_never_answers_holds_the_land_only_so_long() {
    static NOW: AtomicU64 = AtomicU64::new(1);
    fn now() -> u64 {
        NOW.load(Ordering::SeqCst)
    }
    let mut rig = Rig::with_clock(now);
    land_after_settle(&mut rig);

    NOW.fetch_add(DIFF_WAIT_MS - 1, Ordering::SeqCst);
    rig.driver.pump();
    assert!(
        !land_sent(&rig),
        "the land went out before its wait ran out"
    );

    NOW.fetch_add(1, Ordering::SeqCst);
    wait_sent(&mut rig, "ch_land", 1);
}

/// Lets a run end and answers its diff, so nothing but the stop holds a land back.
fn settle_with_diff(rig: &mut Rig) {
    settle(rig);
    let diff = wait_sent(rig, "ch_diff", 1).remove(0);
    rig.say(&diff_result(&diff["id"], "t1"));
    rig.pump_until("the diff", |d, _| d.session().current_diff().is_some());
}

fn stop_failed(rig: &mut Rig) {
    let stop = wait_sent(rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_error","id":stop["id"],"stage":"stop","message":"omp hung"}));
    rig.pump_until("the stop to end", |d, _| !d.stopping());
}

#[test]
fn a_stop_that_fails_drops_the_held_land() {
    let mut rig = Rig::new();
    settle_with_diff(&mut rig);
    assert!(rig.driver.land(land_request()));
    stop_failed(&mut rig);

    assert!(!rig.driver.landing());
    assert!(!rig.driver.can_land());
    assert!(!land_sent(&rig), "the land went out after the stop failed");
    assert!(
        rig.driver
            .log()
            .any(|line| line.contains("the land was not sent")),
        "{:?}",
        rig.driver.log().collect::<Vec<_>>()
    );
}

#[test]
fn a_stop_that_fails_is_not_waited_on() {
    let mut rig = Rig::new();
    settle_with_diff(&mut rig);
    assert!(rig.driver.stop());
    stop_failed(&mut rig);

    let failure = rig.driver.session().failure().map(|f| f.stage.as_str());
    assert_eq!(failure, Some("stop"));
}

#[test]
fn a_stop_refused_before_it_reached_omp_can_be_sent_again() {
    let mut rig = Rig::new();
    settle_with_diff(&mut rig);
    assert!(rig.driver.land(land_request()));
    let stop = wait_sent(&mut rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_error","id":stop["id"],"stage":"protocol","message":"bad line"}));
    rig.pump_until("the stop to be refused", |d, _| !d.stopping());

    assert!(!rig.driver.landing());
    assert!(
        !land_sent(&rig),
        "the land went out after the stop was refused"
    );
    assert!(
        rig.driver
            .log()
            .any(|line| line.contains("omp is still running")),
        "{:?}",
        rig.driver.log().collect::<Vec<_>>()
    );
    assert!(rig.driver.stop());
    wait_sent(&mut rig, "ch_stop", 2);
}

#[test]
fn a_refused_stop_after_omp_exited_leaves_omp_ended() {
    let mut rig = Rig::new();
    settle_with_diff(&mut rig);
    assert!(rig.driver.land(land_request()));
    let stop = wait_sent(&mut rig, "ch_stop", 1).remove(0);
    rig.say(&json!({"type":"ch_omp_exited","exit_code":1,"stderr_tail":[]}));
    wait_sent(&mut rig, "ch_land", 1);

    rig.say(&json!({"type":"ch_error","id":stop["id"],"stage":"protocol","message":"bad line"}));
    rig.pump_until("the stop to be answered", |d, _| {
        d.transcript().contains("bad line")
    });
    assert!(rig.driver.stop());
    rig.driver.pump();
    let stops = rig
        .driver
        .transcript()
        .matches(r#""type":"ch_stop""#)
        .count();
    assert_eq!(stops, 1, "a stop went out to an omp that had exited");
}

#[test]
fn a_diff_with_no_answer_is_noted_once_and_still_shown_late() {
    static NOW: AtomicU64 = AtomicU64::new(1);
    fn now() -> u64 {
        NOW.load(Ordering::SeqCst)
    }
    fn overdue_notes(rig: &Rig) -> usize {
        rig.driver
            .log()
            .filter(|line| line.contains("no answer for two minutes"))
            .count()
    }
    let mut rig = Rig::with_clock(now);
    settle(&mut rig);
    let diff = wait_sent(&mut rig, "ch_diff", 1).remove(0);

    NOW.fetch_add(DIFF_OVERDUE_MS - 1, Ordering::SeqCst);
    rig.driver.pump();
    assert!(
        !rig.driver.pump(),
        "a pump with nothing new reported a change"
    );
    assert_eq!(overdue_notes(&rig), 0, "noted before the diff was overdue");

    NOW.fetch_add(1, Ordering::SeqCst);
    assert!(
        rig.driver.pump(),
        "the pump that noted the overdue diff reported no change, so the view would not repaint"
    );
    assert!(!rig.driver.pump());
    assert_eq!(overdue_notes(&rig), 1);
    assert_eq!(open_diffs(&rig).len(), 1, "the overdue diff was dropped");

    rig.say(&diff_result(&diff["id"], "t1"));
    rig.pump_until("the late diff", |d, _| d.session().current_diff().is_some());
    assert_eq!(open_diffs(&rig), Vec::<String>::new());
    assert_eq!(overdue_notes(&rig), 1);
}

#[test]
fn a_turn_diff_with_no_answer_is_noted_once() {
    static NOW: AtomicU64 = AtomicU64::new(1);
    fn now() -> u64 {
        NOW.load(Ordering::SeqCst)
    }
    let mut rig = Rig::with_clock(now);
    first_run(&mut rig, true);
    assert!(rig.driver.diff_range(0, None));
    wait_sent(&mut rig, "ch_diff", 2);

    NOW.fetch_add(DIFF_OVERDUE_MS, Ordering::SeqCst);
    rig.driver.pump();
    rig.driver.pump();
    let notes = rig
        .driver
        .log()
        .filter(|line| line.contains("no answer for two minutes"))
        .count();
    assert_eq!(notes, 1);
}
