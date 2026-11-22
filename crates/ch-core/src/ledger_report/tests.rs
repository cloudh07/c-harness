use serde_json::json;

use super::*;
use crate::usage::UsageStatus;

fn row(turn: u32, model: Option<&str>, input: u64, output: u64, cost: Option<f64>) -> UsageRow {
    UsageRow {
        turn,
        at_ms: u64::from(turn) * 1_000,
        provider: model.map(|_| "anthropic".to_string()),
        model: model.map(str::to_string),
        input: Some(input),
        output: Some(output),
        cache_read: Some(input * 3),
        cache_write: Some(10),
        cost_usd: cost,
        status: UsageStatus::Complete,
    }
}

fn omp(input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) -> OmpTotals {
    OmpTotals {
        input,
        output,
        cache_read,
        cache_write,
        cost_usd: cost,
    }
}

fn line(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

fn stats(cost: &Value) -> Value {
    json!({
        "type": "response",
        "command": "get_session_stats",
        "success": true,
        "data": {
            "tokens": {"input": 100, "output": 20, "reasoning": 0, "cacheRead": 300,
                       "cacheWrite": 10, "total": 430},
            "cost": cost,
        },
    })
}

#[test]
fn stats_replies_read_as_omp_totals() {
    assert_eq!(
        OmpTotals::from_stats(&stats(&json!(0.25))),
        Some(omp(100, 20, 300, 10, 0.25))
    );
    assert_eq!(
        OmpTotals::from_stats(&stats(&json!(0))),
        Some(omp(100, 20, 300, 10, 0.0)),
        "omp prices an unpriced session at zero"
    );
    let mut failed = stats(&json!(0.25));
    failed["success"] = json!(false);
    let mut other = stats(&json!(0.25));
    other["command"] = json!("get_state");
    let mut short = stats(&json!(0.25));
    short["data"]["tokens"]
        .as_object_mut()
        .unwrap()
        .remove("cacheWrite");
    for frame in [
        failed,
        other,
        short,
        stats(&json!(-1.0)),
        stats(&json!("1")),
    ] {
        assert_eq!(OmpTotals::from_stats(&frame), None, "{frame}");
    }
}

#[test]
fn a_line_with_a_kind_never_reads_as_a_row() {
    let turn = row(1, Some("m"), 5, 1, Some(0.1));
    assert_eq!(
        LedgerLine::parse(&line(&turn)),
        Some(LedgerLine::Row(turn.clone()))
    );

    let mark = LedgerMark::Verified { turn: 1, at_ms: 9 };
    assert_eq!(
        LedgerLine::parse(&line(&mark)),
        Some(LedgerLine::Mark(mark))
    );
    let reading = LedgerMark::OmpTotals {
        turn: 1,
        at_ms: 9,
        totals: omp(5, 1, 15, 10, 0.1),
    };
    assert_eq!(
        LedgerLine::parse(&line(&reading)),
        Some(LedgerLine::Mark(reading))
    );

    let mut unknown = serde_json::to_value(&turn).unwrap();
    unknown["kind"] = json!("from_a_later_version");
    assert_eq!(LedgerLine::parse(&unknown.to_string()), None);
    assert_eq!(LedgerLine::parse("not json"), None);
}

#[test]
fn parse_keeps_the_first_row_of_a_turn_and_every_mark() {
    let first = row(1, Some("m"), 5, 1, Some(0.1));
    let retry = row(1, Some("m"), 50, 10, Some(1.0));
    let text = [
        line(&first),
        line(&retry),
        "{\"half a line".to_string(),
        line(&LedgerMark::Verified { turn: 1, at_ms: 2 }),
    ]
    .join("\n");
    let ledger = SessionLedger::parse(&text);
    assert_eq!(ledger.rows, vec![first]);
    assert_eq!(
        ledger.marks,
        vec![LedgerMark::Verified { turn: 1, at_ms: 2 }]
    );
}

#[test]
fn totals_split_by_model() {
    let ledger = SessionLedger {
        rows: vec![
            row(1, Some("opus"), 100, 10, Some(0.5)),
            row(2, Some("haiku"), 40, 4, None),
            row(3, Some("opus"), 200, 20, Some(1.0)),
            row(4, None, 1, 1, Some(0.01)),
        ],
        marks: Vec::new(),
    };
    let models = ledger.by_model();
    assert_eq!(
        models.keys().map(String::as_str).collect::<Vec<_>>(),
        ["anthropic/haiku", "anthropic/opus", "unknown model"]
    );
    let opus = &models["anthropic/opus"];
    assert_eq!((opus.turns, opus.input, opus.output), (2, 300, 30));
    assert_eq!((opus.cache_read, opus.cache_write), (900, 20));
    assert_eq!(opus.cost_usd, Some(1.5));
    let haiku = &models["anthropic/haiku"];
    assert_eq!((haiku.cost_usd, haiku.unpriced_turns), (None, 1));
    assert_eq!(models["unknown model"].turns, 1);
}

#[test]
fn a_verified_task_costs_what_the_session_spent_up_to_its_last_pass() {
    let rows = vec![
        row(1, Some("m"), 100, 10, Some(0.25)),
        row(2, Some("m"), 100, 10, None),
        row(3, Some("m"), 100, 10, Some(0.5)),
        row(4, Some("m"), 100, 10, Some(2.0)),
    ];
    let unverified = SessionLedger {
        rows: rows.clone(),
        marks: vec![LedgerMark::OmpTotals {
            turn: 4,
            at_ms: 0,
            totals: omp(0, 0, 0, 0, 0.0),
        }],
    };
    assert_eq!(unverified.verified_cost(), None);

    let verified = SessionLedger {
        rows,
        marks: vec![
            LedgerMark::Verified { turn: 1, at_ms: 0 },
            LedgerMark::Verified { turn: 3, at_ms: 0 },
        ],
    };
    let cost = verified.verified_cost().unwrap();
    assert_eq!(cost.turns, 3);
    assert_eq!(cost.cost_usd, Some(0.75));
    assert_eq!(cost.unpriced_turns, 1);
}

#[test]
fn reconcile_names_every_count_that_differs() {
    let ledger = UsageTotals::of(&[
        row(1, Some("m"), 100, 10, Some(0.25)),
        row(2, Some("m"), 50, 5, None),
    ]);
    let same = omp(150, 15, 450, 20, 0.25 + 1e-9);
    assert_eq!(
        reconcile(&ledger, &same),
        Vec::new(),
        "rounding is not a gap"
    );

    let gaps = reconcile(&ledger, &omp(170, 15, 450, 20, 0.1));
    assert_eq!(
        gaps,
        vec![
            Gap::Tokens {
                kind: "input",
                ledger: 150,
                omp: 170
            },
            Gap::Cost {
                ledger: 0.25,
                omp: 0.1
            },
        ]
    );
    assert!(gaps[0].likely_cause().contains("subagent"));
    assert!(gaps[1].likely_cause().contains("compaction"));
}

#[test]
fn each_reading_is_checked_against_the_rows_before_it() {
    let ledger = SessionLedger {
        rows: vec![
            row(1, Some("m"), 100, 10, Some(0.25)),
            row(2, Some("m"), 50, 5, Some(0.25)),
            row(3, Some("m"), 70, 7, Some(0.25)),
        ],
        marks: vec![
            LedgerMark::OmpTotals {
                turn: 2,
                at_ms: 0,
                totals: omp(150, 15, 450, 20, 0.5),
            },
            LedgerMark::Verified { turn: 2, at_ms: 0 },
            LedgerMark::OmpTotals {
                turn: 3,
                at_ms: 0,
                totals: omp(220, 22, 660, 30, 0.7),
            },
        ],
    };
    let readings = ledger.readings();
    assert_eq!(readings.len(), 2);
    assert_eq!(
        readings[0],
        Reading {
            turn: 2,
            gaps: Vec::new()
        }
    );
    assert_eq!(readings[1].turn, 3);
    assert_eq!(
        readings[1].gaps,
        vec![Gap::Cost {
            ledger: 0.75,
            omp: 0.7
        }]
    );
}

#[test]
fn without_ledgers_the_report_says_not_run_and_gives_no_numbers() {
    let empty = report(&[]);
    assert!(empty.contains("not run"), "{empty}");
    let blank = report(&[("s1".into(), SessionLedger::default())]);
    assert_eq!(blank, empty);
    assert!(!empty.contains('$'));
}

#[test]
fn the_report_holds_totals_models_verified_cost_and_mismatches() {
    let checked = SessionLedger {
        rows: vec![
            row(1, Some("opus"), 100, 10, Some(0.5)),
            row(2, Some("opus"), 100, 10, None),
        ],
        marks: vec![
            LedgerMark::Verified { turn: 2, at_ms: 0 },
            LedgerMark::OmpTotals {
                turn: 2,
                at_ms: 0,
                totals: omp(200, 20, 600, 20, 0.5),
            },
        ],
    };
    let compacted = SessionLedger {
        rows: vec![row(1, Some("haiku"), 300, 30, Some(1.5))],
        marks: vec![LedgerMark::OmpTotals {
            turn: 1,
            at_ms: 0,
            totals: omp(10, 30, 900, 10, 1.5),
        }],
    };
    let text = report(&[("a".into(), checked), ("b".into(), compacted)]);
    for expected in [
        "Sessions: 2. Turns: 3, 1 without a cost.",
        "Tokens: input 500, output 50, cache read 1500, cache write 30.",
        "Cache share (cache read / (input + cache read)): 75%.",
        "Cost: $2.00 plus 1 turn without a cost.",
        "  anthropic/haiku: 1 turn, input 300, output 30, cache read 900, cache write 10, $1.50.",
        "  anthropic/opus: 2 turns,",
        "  Verified tasks: 1. Average $0.50, total $0.50.",
        "  Of these, 1 had turns without a cost, so the real cost is higher.",
        "  Readings: 2, matching: 1.",
        "  b after turn 1: input ledger 300, omp 10. Likely: omp counts only messages still in context",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

#[test]
fn a_report_without_omp_readings_says_it_did_not_check() {
    let text = report(&[(
        "old".into(),
        SessionLedger {
            rows: vec![row(1, Some("m"), 1, 1, Some(0.001))],
            marks: Vec::new(),
        },
    )]);
    assert!(
        text.contains("Not checked: no ledger holds omp's totals yet."),
        "{text}"
    );
    assert!(text.contains("No session has a passing Verify"), "{text}");
    assert!(text.contains("Cost: <$0.01."), "{text}");
}
