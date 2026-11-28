//! The finished notice against a real driver. A stop the user asked for has to be seen before
//! the frame that ends it is read, even when both land between two ticks.

use super::checkpoints::wait_sent;
use super::cost::{running, turn};
use super::*;
use crate::session_view::finished::{Change, Run};

/// One tick of the session screen, as far as the notice goes.
fn tick(run: Run, driver: &mut Driver) -> (Run, Change) {
    let (run, _) = run.read_frames(driver);
    run.after(driver.session().state())
}

/// A working session the notice has already seen running.
fn seen_running(rig: &mut Rig) -> Run {
    running(rig);
    let (run, change) = tick(Run::default(), &mut rig.driver);
    assert_eq!(change, Change::Nothing);
    run
}

/// The user presses Stop, and the runner's answer is queued before the next tick.
fn stop_answered(rig: &mut Rig) {
    assert!(rig.driver.stop());
    let stop = wait_sent(rig, "ch_stop", 1).remove(0);
    rig.queue(&json!({"type":"ch_stopped","id":stop["id"],"exit_code":0}));
}

#[test]
fn a_stop_ended_within_one_tick_is_not_told() {
    let mut rig = Rig::new();
    let run = seen_running(&mut rig);
    stop_answered(&mut rig);

    let (_, change) = tick(run, &mut rig.driver);
    assert!(!rig.driver.stopping(), "the end was read in this tick");
    assert_eq!(rig.driver.session().state(), SessionState::NeedsReview);
    assert_eq!(change, Change::Nothing, "the user stopped it");
}

#[test]
fn reading_the_end_before_the_stop_would_tell_it() {
    let mut rig = Rig::new();
    let run = seen_running(&mut rig);
    stop_answered(&mut rig);

    rig.driver.pump();
    let state = rig.driver.session().state();
    let (_, change) = run.before(state, rig.driver.stopping()).after(state);
    assert_eq!(
        change,
        Change::Ended,
        "why read_frames looks before it pumps"
    );
}

#[test]
fn a_run_the_cost_cap_ended_is_told() {
    let mut rig = Rig::new();
    let run = seen_running(&mut rig);
    turn(&mut rig, Some(0.29));
    assert!(rig.driver.enforce_cost_cap(Some(29)).is_some());
    rig.pump_until("the abort", |_, sent| !of_type(sent, "abort").is_empty());
    rig.queue(&json!({"type":"session_settled"}));

    let (_, change) = tick(run, &mut rig.driver);
    assert_eq!(rig.driver.session().state(), SessionState::NeedsReview);
    assert_eq!(change, Change::Ended, "nobody here stopped it");
}
