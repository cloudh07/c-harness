//! The cost cap aborts a run once the priced turns reach it, and the ledger marks note omp's
//! own totals and each passing check.

use super::*;
use ch_core::LedgerMark;

/// A session whose agent is working, with the runner's hello already said.
fn running(rig: &mut Rig) {
    rig.hello();
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| sent.len() == 3);
    rig.say(&json!({"type":"agent_start"}));
    rig.pump_until("the agent to run", |d, _| {
        d.session().state() == SessionState::Running
    });
}

/// Ends a turn that cost `cost` dollars, or one omp could not price.
fn turn(rig: &mut Rig, cost: Option<f64>) {
    let mut usage = json!({"input":10,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":12});
    if let Some(cost) = cost {
        usage["cost"] = json!({ "total": cost });
    }
    let count = rig.driver.usage().rows().len();
    rig.say(&json!({
        "type":"turn_end","turnIndex":count,"toolResults":[],
        "message":{"role":"assistant","provider":"anthropic","model":"m","stopReason":"stop",
            "usage":usage}
    }));
    rig.pump_until("the turn", |d, _| d.usage().rows().len() == count + 1);
}

fn hit(cents: u32, abort_sent: bool) -> CapReached {
    CapReached { cents, abort_sent }
}

fn aborts(rig: &Rig) -> usize {
    of_type(&rig.sent(), "abort").len()
}

/// Waits until everything sent so far was written: the writer keeps order.
fn flush(rig: &mut Rig) {
    let count = of_type(&rig.sent(), "ch_diff").len();
    assert!(rig.driver.diff());
    rig.pump_until("the probe", |_, sent| {
        of_type(sent, "ch_diff").len() == count + 1
    });
}

#[test]
fn reaching_the_cap_aborts_once_and_holds_prompts_until_it_is_raised() {
    let mut rig = Rig::new();
    running(&mut rig);
    turn(&mut rig, Some(0.29));

    assert_eq!(
        rig.driver.enforce_cost_cap(Some(29)),
        Some(hit(29, true)),
        "at the cap"
    );
    rig.pump_until("the abort", |_, sent| !of_type(sent, "abort").is_empty());
    assert_eq!(rig.driver.enforce_cost_cap(Some(29)), None, "said once");
    assert_eq!(rig.driver.cap_hit(), Some(29));
    assert_eq!(
        rig.driver.held_at_cap(),
        None,
        "the run, not the cap, holds prompts"
    );
    assert!(
        rig.driver
            .log()
            .any(|line| line.contains("Sent an abort") && line.contains("cost cap"))
    );

    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    assert_eq!(rig.driver.enforce_cost_cap(Some(29)), None);
    assert!(!rig.driver.can_prompt(), "held at the cap");
    assert_eq!(rig.driver.held_at_cap(), Some(29));
    assert_eq!(aborts(&rig), 1, "{:?}", rig.sent());

    assert_eq!(rig.driver.enforce_cost_cap(Some(30)), None, "raised");
    assert_eq!(rig.driver.cap_hit(), None);
    assert_eq!(rig.driver.held_at_cap(), None);
    assert!(rig.driver.can_prompt());
    assert_eq!(
        rig.driver.enforce_cost_cap(Some(29)),
        Some(hit(29, false)),
        "lowered again, with no run to abort"
    );
    assert!(!rig.driver.can_prompt());
    assert_eq!(rig.driver.enforce_cost_cap(None), None, "cleared");
    assert!(rig.driver.can_prompt());
    flush(&mut rig);
    assert_eq!(aborts(&rig), 1, "no abort without a run to stop");
}

#[test]
fn below_the_cap_nothing_is_aborted() {
    let mut rig = Rig::new();
    running(&mut rig);
    turn(&mut rig, Some(0.28));
    assert_eq!(rig.driver.enforce_cost_cap(Some(29)), None);
    assert_eq!(rig.driver.enforce_cost_cap(None), None, "no cap");
    assert_eq!(rig.driver.cap_hit(), None);
    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    flush(&mut rig);
    assert_eq!(aborts(&rig), 0);
}

#[test]
fn turns_that_add_up_to_the_cap_reach_it() {
    // Summed as f64, 0.7 + 0.1 falls just short of 0.8 and 0.1 + 0.2 lands just past 0.3.
    for (costs, cents) in [([0.7, 0.1], 80), ([0.1, 0.2], 30)] {
        let mut rig = Rig::new();
        running(&mut rig);
        for cost in costs {
            turn(&mut rig, Some(cost));
        }
        assert_eq!(
            rig.driver.enforce_cost_cap(Some(cents)),
            Some(hit(cents, true)),
            "{costs:?}"
        );
    }
}

#[test]
fn a_sent_stop_gets_no_abort_on_top() {
    let mut rig = Rig::new();
    running(&mut rig);
    turn(&mut rig, Some(0.3));
    assert!(rig.driver.stop());
    assert!(rig.driver.stopping());
    assert_eq!(rig.driver.session().state(), SessionState::Running);

    assert_eq!(rig.driver.enforce_cost_cap(Some(30)), Some(hit(30, false)));
    assert_eq!(rig.driver.cap_hit(), Some(30), "held all the same");
    flush(&mut rig);
    assert_eq!(aborts(&rig), 0, "{:?}", rig.sent());
}

#[test]
fn a_queued_land_gets_no_abort_on_top() {
    let mut rig = Rig::new();
    running(&mut rig);
    turn(&mut rig, Some(0.3));
    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    assert!(rig.driver.land(land_request()));
    assert!(rig.driver.landing() && rig.driver.stopping());
    rig.say(&json!({"type":"agent_start"}));
    rig.pump_until("a late run", |d, _| {
        d.session().state() == SessionState::Running
    });

    assert_eq!(rig.driver.enforce_cost_cap(Some(30)), Some(hit(30, false)));
    flush(&mut rig);
    assert_eq!(aborts(&rig), 0, "{:?}", rig.sent());
}

#[test]
fn turns_omp_cannot_price_do_not_count_toward_the_cap() {
    let mut rig = Rig::new();
    running(&mut rig);
    turn(&mut rig, None);
    turn(&mut rig, None);
    assert_eq!(rig.driver.enforce_cost_cap(Some(1)), None, "nothing priced");
    turn(&mut rig, Some(0.004));
    turn(&mut rig, None);
    assert_eq!(
        rig.driver.enforce_cost_cap(Some(1)),
        None,
        "less than a cent priced"
    );
    turn(&mut rig, Some(0.006));
    assert_eq!(rig.driver.enforce_cost_cap(Some(1)), Some(hit(1, true)));
    rig.pump_until("the abort", |_, sent| !of_type(sent, "abort").is_empty());
}

fn stats(id: &Value, cost: f64) -> Value {
    json!({
        "id": id, "type": "response", "command": STATS_COMMAND, "success": true,
        "data": { "tokens": { "input": 10, "output": 2, "cacheRead": 0, "cacheWrite": 0,
            "total": 12 }, "cost": cost },
    })
}

fn stats_ids(rig: &Rig) -> Vec<Value> {
    of_type(&rig.sent(), STATS_COMMAND)
        .into_iter()
        .map(|line| line["id"].clone())
        .collect()
}

#[test]
fn omp_totals_are_marked_against_the_turns_counted_so_far() {
    let mut rig = Rig::new();
    running(&mut rig);
    let first = stats_ids(&rig);
    assert_eq!(first.len(), 1, "asked once omp was ready");
    rig.say(&stats(&first[0], 0.0));
    turn(&mut rig, Some(0.001));
    rig.pump_until("a second stats request", |_, sent| {
        of_type(sent, STATS_COMMAND).len() == 2
    });
    rig.say(&stats(&stats_ids(&rig)[1], 0.001));
    rig.pump_until("the mark", |d, _| !d.marks().is_empty());

    let [LedgerMark::OmpTotals { turn, totals, .. }] = rig.driver.marks() else {
        panic!("one mark, after the turn: {:?}", rig.driver.marks());
    };
    assert_eq!(*turn, 1);
    assert_eq!(
        (totals.input, totals.output, totals.cost_usd),
        (10, 2, 0.001)
    );
}

#[test]
fn only_a_passing_check_that_was_asked_for_is_marked_verified() {
    let mut rig = Rig::new();
    running(&mut rig);
    turn(&mut rig, Some(0.001));
    rig.say(&json!({"type":"session_settled"}));
    rig.pump_until("review", |d, _| {
        d.session().state() == SessionState::NeedsReview
    });
    let evidence = |id: &Value, exit_code: i32| {
        json!({"type":"ch_evidence","id":id,"command":"cargo test","exit_code":exit_code,
            "duration_ms":10,"output_tail":"ok","truncated":false,"timed_out":false})
    };
    let verified = |d: &Driver| {
        d.marks()
            .iter()
            .filter(|m| matches!(m, LedgerMark::Verified { .. }))
            .count()
    };

    rig.say(&evidence(&json!("req_99"), 0));
    assert!(rig.driver.verify("cargo test"));
    rig.pump_until("the verify", |_, sent| {
        !of_type(sent, "ch_verify").is_empty()
    });
    let failing = of_type(&rig.sent(), "ch_verify")[0]["id"].clone();
    rig.say(&evidence(&failing, 1));
    rig.pump_until("the failing evidence", |d, _| {
        d.session().evidence().len() == 1
    });
    assert_eq!(verified(&rig.driver), 0, "{:?}", rig.driver.marks());

    assert!(rig.driver.verify("cargo test"));
    rig.pump_until("the second verify", |_, sent| {
        of_type(sent, "ch_verify").len() == 2
    });
    let passing = of_type(&rig.sent(), "ch_verify")[1]["id"].clone();
    rig.say(&evidence(&passing, 0));
    rig.pump_until("the passing evidence", |d, _| {
        d.session().evidence().len() == 2
    });
    assert_eq!(
        rig.driver.marks().last(),
        Some(&LedgerMark::Verified { turn: 1, at_ms: 1 })
    );
    assert_eq!(verified(&rig.driver), 1);
}
