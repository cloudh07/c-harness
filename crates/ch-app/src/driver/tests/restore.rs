//! Restores that move omp's conversation and the runner's tree together, against a fake omp that
//! answers as 18.13.2 does, or as 18.13.1 does, which knows neither `set_label` nor
//! `navigate_tree`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use ch_core::{ENTRIES_COMMAND, HISTORY_COMMAND};

use super::checkpoints::{answer_save, checkpoint, end_run, first_run, flush, saves, wait_sent};
use super::*;

const LABEL: &str = "set_label";
const NAVIGATE: &str = "navigate_tree";
const TOO_OLD: &str =
    "Restore needs omp 18.13.2 or newer. Code and conversation were left unchanged.";

type Answer = Result<Value, (String, Option<&'static str>)>;

/// omp's conversation tree, answering the requests a mark or a restore sends.
#[derive(Default)]
pub(super) struct FakeOmp {
    entries: Vec<Value>,
    leaf: Option<String>,
    /// 18.13.2 or newer.
    current: bool,
    answered: HashSet<String>,
    /// The next request of a command fails with this error and code.
    failures: Vec<(&'static str, &'static str, Option<&'static str>)>,
    /// The next request of a command goes unanswered.
    silent: Vec<&'static str>,
}

impl FakeOmp {
    pub(super) fn new() -> Self {
        Self {
            current: true,
            ..Self::default()
        }
    }

    pub(super) fn old() -> Self {
        Self::default()
    }

    fn fail_next(
        &mut self,
        command: &'static str,
        error: &'static str,
        code: Option<&'static str>,
    ) {
        self.failures.push((command, error, code));
    }

    /// One run: the prompt and the agent's reply.
    pub(super) fn ran(&mut self, turn: u32) {
        for (role, text) in [("user", "prompt"), ("assistant", "reply")] {
            self.append(json!({ "type": "message",
                "message": { "role": role, "content": format!("{text} {turn}") } }));
        }
    }

    fn append(&mut self, mut entry: Value) {
        let id = format!("e{}", self.entries.len() + 1);
        entry["id"] = json!(id);
        entry["parentId"] = json!(self.leaf);
        self.entries.push(entry);
        self.leaf = Some(id);
    }

    fn entry(&self, id: &str) -> Option<&Value> {
        self.entries.iter().find(|entry| entry["id"] == id)
    }

    /// The messages from the root to the leaf: what the agent sees.
    pub(super) fn path(&self) -> Vec<String> {
        let mut texts = Vec::new();
        let mut at = self.leaf.clone();
        while let Some(id) = at {
            let entry = self.entry(&id).expect("every parent is an entry");
            if let Some(text) = entry.pointer("/message/content").and_then(Value::as_str) {
                texts.push(text.to_owned());
            }
            at = entry["parentId"].as_str().map(str::to_owned);
        }
        texts.reverse();
        texts
    }

    /// omp starts again on the saved session: at its last entry, with nothing answered yet.
    fn reload(&mut self) {
        self.leaf = self
            .entries
            .last()
            .and_then(|entry| entry["id"].as_str())
            .map(str::to_owned);
        self.answered.clear();
    }

    fn answer(&mut self, request: &Value) -> Option<Value> {
        let command = request["type"].as_str().unwrap_or_default();
        if let Some(at) = self.silent.iter().position(|c| *c == command) {
            self.silent.remove(at);
            return None;
        }
        let answer = if !self.current && command != ENTRIES_COMMAND {
            Err((format!("Unknown command: {command}"), None))
        } else if let Some(at) = self.failures.iter().position(|f| f.0 == command) {
            let (_, error, code) = self.failures.remove(at);
            Err((error.to_owned(), code))
        } else if command == ENTRIES_COMMAND {
            self.list(request.get("since").and_then(Value::as_str))
        } else if command == LABEL {
            self.label(request)
        } else {
            self.navigate(request)
        };
        let mut reply = json!({ "id": request["id"], "type": "response", "command": command });
        match answer {
            Ok(data) => {
                reply["success"] = json!(true);
                if !data.is_null() {
                    reply["data"] = data;
                }
            }
            Err((error, code)) => {
                reply["success"] = json!(false);
                reply["error"] = json!(error);
                if let Some(code) = code {
                    reply["code"] = json!(code);
                }
            }
        }
        Some(reply)
    }

    fn list(&self, since: Option<&str>) -> Answer {
        let start = match since {
            None => 0,
            Some(since) => match self.entries.iter().position(|e| e["id"] == since) {
                Some(at) => at + 1,
                None => {
                    return Err((
                        format!("Unknown entries cursor: {since}"),
                        Some("unknown_since"),
                    ));
                }
            },
        };
        Ok(json!({ "entries": self.entries[start..], "leafId": self.leaf }))
    }

    fn label(&mut self, request: &Value) -> Answer {
        let target = request["entryId"].as_str().unwrap_or_default();
        if self.entry(target).is_none() {
            return Err((format!("Entry {target} not found"), None));
        }
        let mut entry = json!({ "type": "label", "targetId": target });
        if let Some(label) = request.get("label") {
            entry["label"] = label.clone();
        }
        self.append(entry);
        Ok(Value::Null)
    }

    fn navigate(&mut self, request: &Value) -> Answer {
        assert_eq!(
            request["summarize"], false,
            "a move asked for a summary: {request}"
        );
        let target = request["entryId"].as_str().unwrap_or_default();
        let Some(entry) = self.entry(target) else {
            return Err((format!("Entry {target} not found"), Some("entry_not_found")));
        };
        // A move to a prompt lands just before it, so the prompt can be sent again.
        self.leaf = if entry.pointer("/message/role") == Some(&json!("user")) {
            entry["parentId"].as_str().map(str::to_owned)
        } else {
            Some(target.to_owned())
        };
        Ok(json!({ "cancelled": false, "aborted": false }))
    }

    /// Answers every mark or restore request sent so far.
    fn serve(&mut self, rig: &Rig) {
        for request in rig.sent() {
            let kind = request["type"].as_str().unwrap_or_default();
            let Some(id) = request["id"].as_str() else {
                continue;
            };
            let ours = [ENTRIES_COMMAND, LABEL, NAVIGATE].contains(&kind);
            if !ours || !self.answered.insert(id.to_owned()) {
                continue;
            }
            if let Some(reply) = self.answer(&request) {
                rig.say(&reply);
            }
        }
    }

    fn run_until(&mut self, rig: &mut Rig, what: &str, done: impl Fn(&Driver, &[Value]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            rig.driver.pump();
            self.serve(rig);
            if done(&rig.driver, &rig.sent()) {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("timed out waiting for {what}; sent {:?}", rig.sent());
    }

    /// Answers omp's side until the mark or the restore under way has ended.
    pub(super) fn settle(&mut self, rig: &mut Rig) {
        self.run_until(rig, "omp's part to end", |d, _| {
            !d.marking() && !d.rewinding()
        });
    }

    /// Answers omp's side until `count` frames of `kind` were written, and returns them.
    pub(super) fn wait_sent(&mut self, rig: &mut Rig, kind: &str, count: usize) -> Vec<Value> {
        self.run_until(rig, kind, |_, sent| of_type(sent, kind).len() == count);
        of_type(&rig.sent(), kind).into_iter().cloned().collect()
    }
}

/// One run, saved as checkpoint 1 and marked in `omp`'s conversation.
pub(super) fn saved_run_on(rig: &mut Rig, mut omp: FakeOmp) -> FakeOmp {
    first_run(rig, true);
    omp.ran(1);
    let request = saves(rig)[0].clone();
    answer_save(rig, &request, 1);
    omp.settle(rig);
    omp
}

/// Another run, saved as checkpoint `turn`.
fn next_run(rig: &mut Rig, omp: &mut FakeOmp, turn: u32) {
    let before = saves(rig).len();
    assert!(rig.driver.prompt(&format!("prompt {turn}")));
    end_run(rig);
    omp.ran(turn);
    let request = wait_sent(rig, "ch_checkpoint", before + 1).remove(before);
    answer_save(rig, &request, turn);
    omp.settle(rig);
}

/// Sends a restore of `turn` and answers omp's side until the code request is out.
fn start(rig: &mut Rig, omp: &mut FakeOmp, turn: u32) -> Value {
    let before = of_type(&rig.sent(), "ch_restore").len();
    assert!(rig.driver.can_restore());
    assert!(rig.driver.restore(turn));
    let request = omp.wait_sent(rig, "ch_restore", before + 1).remove(before);
    assert_eq!(request["turn"], turn);
    request["id"].clone()
}

/// Ends the code half of restore `id` and answers the diff refresh that follows it, which
/// would otherwise hold the next restore.
fn restored(rig: &mut Rig, omp: &mut FakeOmp, id: &Value, turn: u32) {
    let diffs = of_type(&rig.sent(), "ch_diff").len();
    rig.say(&json!({"type":"ch_restored","id":id,"turn":turn}));
    let refresh = wait_sent(rig, "ch_diff", diffs + 1).remove(diffs);
    rig.say(&diff_result(&refresh["id"], "restored"));
    omp.settle(rig);
    rig.pump_until("the refreshed diff", |d, _| d.can_restore());
}

/// Restores `turn` with both halves agreeing; the runner saves the tree it replaces as `undo`.
fn restore_both(rig: &mut Rig, omp: &mut FakeOmp, turn: u32, undo: u32) {
    let id = start(rig, omp, turn);
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(undo, "before_restore")}));
    restored(rig, omp, &id, turn);
    assert!(
        noted(
            rig,
            &format!("The conversation is back at checkpoint {turn} too.")
        ),
        "{:?}",
        rig.driver.log().collect::<Vec<_>>()
    );
}

fn noted(rig: &Rig, text: &str) -> bool {
    rig.driver.log().any(|line| line.contains(text))
}

fn texts(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_owned()).collect()
}

#[test]
fn restore_without_rollback_support_is_refused() {
    let mut rig = Rig::new();
    let omp = saved_run_on(&mut rig, FakeOmp::old());
    assert!(rig.driver.can_restore());

    assert!(
        rig.driver.restore(1),
        "the refusal is an answer, not a send failure"
    );
    flush(&mut rig);
    let sent = rig.sent();
    assert!(of_type(&sent, NAVIGATE).is_empty(), "{sent:?}");
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert!(noted(&rig, TOO_OLD));
    assert!(!rig.driver.restoring());
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
}

#[test]
fn an_old_omp_found_only_at_the_move_changes_nothing() {
    let mut rig = Rig::new();
    let mut omp = FakeOmp::old();
    first_run(&mut rig, true);
    omp.ran(1);
    let id = saves(&rig)[0]["id"].clone();
    rig.say(&json!({"type":"ch_error","id":id,"stage":"checkpoint","message":"disk full"}));
    omp.settle(&mut rig);

    // No label was tried, so only the move shows omp is too old.
    assert!(rig.driver.restore(0));
    omp.settle(&mut rig);
    flush(&mut rig);
    let sent = rig.sent();
    assert_eq!(of_type(&sent, NAVIGATE).len(), 1, "{sent:?}");
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert!(noted(&rig, TOO_OLD));
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
}

#[test]
fn restore_on_a_branched_session_leaves_code_and_conversation_at_the_same_turn() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    let id = start(&mut rig, &mut omp, 1);
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1"]),
        "the conversation moves before the code"
    );
    assert!(!rig.driver.can_prompt() && !rig.driver.can_restore());
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    restored(&mut rig, &mut omp, &id, 1);
    assert!(rig.driver.can_prompt());

    // A new run branches off checkpoint 1; restoring 2 goes back to the other branch.
    next_run(&mut rig, &mut omp, 4);
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 4", "reply 4"])
    );
    restore_both(&mut rig, &mut omp, 2, 5);
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
    let codes: Vec<Value> = of_type(&rig.sent(), "ch_restore")
        .iter()
        .map(|r| r["turn"].clone())
        .collect();
    assert_eq!(codes, [json!(1), json!(2)]);

    omp.reload();
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"]),
        "a reload keeps the move"
    );
    assert_eq!(drops(&rig.driver), 0);
}

#[test]
fn restoring_the_base_empties_the_conversation() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    restore_both(&mut rig, &mut omp, 0, 2);
    assert!(omp.path().is_empty(), "{:?}", omp.path());
    omp.reload();
    assert!(omp.path().is_empty(), "{:?}", omp.path());
}

#[test]
fn undo_through_the_before_restore_checkpoint_brings_both_halves_back() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    restore_both(&mut rig, &mut omp, 1, 3);
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));

    restore_both(&mut rig, &mut omp, 3, 4);
    let both = texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"]);
    assert_eq!(omp.path(), both);
    omp.reload();
    assert_eq!(omp.path(), both);
}

#[test]
fn a_restore_survives_a_resume() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    restore_both(&mut rig, &mut omp, 1, 3);
    drop(rig);

    omp.reload();
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
    let mut rig = Rig::new();
    assert!(rig.driver.resume(
        "s1",
        vec!["--resume=/home/me/.omp/sessions/s1.jsonl".to_owned()],
        BTreeMap::new(),
        Vec::new(),
    ));
    rig.hello();
    rig.say(&json!({"type":"ready","protocolVersion":1}));
    rig.say(
        &json!({"type":"ch_checkpoints","checkpoints":[checkpoint(1, "run"),
        checkpoint(2, "run"), checkpoint(3, "before_restore")]}),
    );
    rig.pump_until("a page request", |d, sent| {
        d.session().checkpoints().len() == 3 && !of_type(sent, HISTORY_COMMAND).is_empty()
    });
    assert!(
        !rig.driver.can_restore(),
        "a move could race the earlier conversation's load"
    );
    let page = of_type(&rig.sent(), HISTORY_COMMAND)[0].clone();
    rig.say(
        &json!({"id":page["id"],"type":"response","command":HISTORY_COMMAND,"success":true,
        "data":{"messages":[],"totalMessages":0}}),
    );
    rig.pump_until("the end of the load", |d, _| !d.loading_history());

    // The fresh driver reads the marks back from omp's labels.
    restore_both(&mut rig, &mut omp, 2, 4);
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
    restore_both(&mut rig, &mut omp, 4, 5);
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
}

#[test]
fn a_failed_move_after_the_mark_changes_neither_half() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    let labels = of_type(&rig.sent(), LABEL).len();
    assert_eq!(labels, 2, "both runs were marked");

    omp.fail_next(NAVIGATE, "Session is busy", Some("session_busy"));
    assert!(rig.driver.restore(1));
    omp.settle(&mut rig);
    flush(&mut rig);
    let sent = rig.sent();
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert_eq!(
        of_type(&sent, LABEL).len(),
        labels,
        "nothing to undo or save"
    );
    assert!(noted(
        &rig,
        "omp would not move the conversation to checkpoint 1: Session is busy. Code and \
         conversation were left unchanged."
    ));
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
    assert!(rig.driver.can_prompt());
}

#[test]
fn a_refused_code_restore_moves_the_conversation_back() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    let labels = of_type(&rig.sent(), LABEL).len();

    let id = start(&mut rig, &mut omp, 1);
    rig.say(&json!({"type":"ch_error","id":id,"stage":"restore","message":"dirty tree"}));
    omp.settle(&mut rig);
    assert!(noted(
        &rig,
        "The runner would not restore checkpoint 1, so the conversation was moved back."
    ));
    assert_eq!(of_type(&rig.sent(), NAVIGATE).len(), 2);
    assert_eq!(of_type(&rig.sent(), LABEL).len(), labels, "no move to save");
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
    assert!(rig.driver.can_prompt());
}

#[test]
fn a_move_that_will_not_save_is_tried_twice_then_reported() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    let labels = of_type(&rig.sent(), LABEL).len();

    let id = start(&mut rig, &mut omp, 1);
    omp.fail_next(LABEL, "disk full", None);
    omp.fail_next(LABEL, "disk full", None);
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    rig.say(&json!({"type":"ch_restored","id":id,"turn":1}));
    omp.settle(&mut rig);
    assert_eq!(of_type(&rig.sent(), LABEL).len(), labels + 2);
    assert!(noted(
        &rig,
        "Code and conversation are back at checkpoint 1, but omp did not save the move: disk full."
    ));
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
    assert!(rig.driver.can_prompt());

    omp.reload();
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"]),
        "the note said a resume reopens where it was"
    );
}

#[test]
fn a_checkpoint_left_unmarked_is_refused() {
    let mut rig = Rig::new();
    let mut fresh = FakeOmp::new();
    fresh.fail_next(LABEL, "Entry e2 not found", None);
    let mut omp = saved_run_on(&mut rig, fresh);
    assert!(noted(
        &rig,
        "Checkpoint 1 could not be marked in the conversation"
    ));

    assert!(rig.driver.restore(1));
    omp.settle(&mut rig);
    flush(&mut rig);
    let sent = rig.sent();
    assert!(of_type(&sent, NAVIGATE).is_empty(), "{sent:?}");
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert!(noted(&rig, "Checkpoint 1 has no mark in the conversation"));
}

#[test]
fn a_restore_waits_for_the_mark_of_the_last_run() {
    let mut rig = Rig::new();
    let mut omp = FakeOmp::new();
    first_run(&mut rig, true);
    omp.ran(1);
    let request = saves(&rig)[0].clone();
    answer_save(&mut rig, &request, 1);
    assert!(rig.driver.saving_checkpoint(), "the mark is still out");
    assert!(!rig.driver.can_restore());
    assert!(!rig.driver.restore(1));

    omp.settle(&mut rig);
    assert!(rig.driver.can_restore());
}

static NOW: AtomicU64 = AtomicU64::new(1);

fn now() -> u64 {
    NOW.load(Ordering::SeqCst)
}

#[test]
fn a_move_omp_never_answers_is_undone() {
    let mut rig = Rig::with_clock(now);
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    omp.silent.push(NAVIGATE);
    assert!(rig.driver.restore(1));
    omp.wait_sent(&mut rig, NAVIGATE, 1);
    NOW.fetch_add(15_000, Ordering::SeqCst);
    omp.settle(&mut rig);
    assert_eq!(
        of_type(&rig.sent(), NAVIGATE).len(),
        2,
        "a move back follows"
    );
    let sent = rig.sent();
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert!(noted(
        &rig,
        "omp took too long to move the conversation to checkpoint 1, so the conversation was \
         moved back."
    ));
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
}

#[test]
fn a_restore_leaves_the_usage_ledger_alone() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    rig.say(&json!({
        "type":"turn_end","turnIndex":0,"toolResults":[],
        "message":{"role":"assistant","provider":"anthropic","model":"m","stopReason":"stop",
            "usage":{"input":10,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":12,
                "cost":{"total":0.001}}}
    }));
    rig.pump_until("a usage row", |d, _| d.usage().rows().len() == 1);
    let rows = rig.driver.usage().rows().to_vec();

    restore_both(&mut rig, &mut omp, 0, 2);
    assert_eq!(rig.driver.usage().rows(), rows.as_slice());
}
