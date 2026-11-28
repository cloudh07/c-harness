//! Restores that move omp's conversation and the runner's tree together, against a fake omp that
//! knows every command a restore sends, or an old one that knows neither `set_label` nor
//! `navigate_tree`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use ch_core::{ENTRIES_COMMAND, HISTORY_COMMAND};

use super::checkpoints::{answer_save, checkpoint, end_run, first_run, flush, saves, wait_sent};
use super::*;

pub(super) const LABEL: &str = "set_label";
pub(super) const NAVIGATE: &str = "navigate_tree";

type Answer = Result<Value, (String, Option<&'static str>)>;

/// omp's conversation tree, answering the requests a mark or a restore sends.
#[derive(Default)]
pub(super) struct FakeOmp {
    entries: Vec<Value>,
    leaf: Option<String>,
    /// Knows `set_label` and `navigate_tree`.
    current: bool,
    answered: HashSet<String>,
    /// The next request of a command fails with this error and code.
    failures: Vec<(&'static str, &'static str, Option<&'static str>)>,
    /// The next request of a command goes unanswered.
    pub(super) silent: Vec<&'static str>,
    /// The next request of a command runs only when another request comes in, as a slow
    /// command does in an omp that runs one command at a time.
    hold: Vec<&'static str>,
    held: Vec<Value>,
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

    pub(super) fn fail_next(
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

    /// The messages from the root to the leaf, which a page request returns.
    fn branch(&self) -> Vec<Value> {
        let mut messages = Vec::new();
        let mut at = self.leaf.clone();
        while let Some(id) = at {
            let entry = self.entry(&id).expect("every parent is an entry");
            if let Some(message) = entry.get("message") {
                messages.push(message.clone());
            }
            at = entry["parentId"].as_str().map(str::to_owned);
        }
        messages.reverse();
        messages
    }

    /// The texts of the messages from the root to the leaf: what the agent sees.
    pub(super) fn path(&self) -> Vec<String> {
        self.branch()
            .iter()
            .filter_map(|message| message["content"].as_str().map(str::to_owned))
            .collect()
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
        let answer = if !self.current && ![ENTRIES_COMMAND, HISTORY_COMMAND].contains(&command) {
            Err((format!("Unknown command: {command}"), None))
        } else if let Some(at) = self.failures.iter().position(|f| f.0 == command) {
            let (_, error, code) = self.failures.remove(at);
            Err((error.to_owned(), code))
        } else if command == HISTORY_COMMAND {
            let messages = self.branch();
            Ok(json!({ "messages": messages, "totalMessages": messages.len() }))
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

    /// Answers every mark, restore or page request sent so far. A held request ends before the
    /// next one starts.
    fn serve(&mut self, rig: &Rig) {
        for request in rig.sent() {
            let kind = request["type"].as_str().unwrap_or_default();
            let Some(id) = request["id"].as_str() else {
                continue;
            };
            let ours = [ENTRIES_COMMAND, LABEL, NAVIGATE, HISTORY_COMMAND].contains(&kind);
            if !ours || !self.answered.insert(id.to_owned()) {
                continue;
            }
            for held in std::mem::take(&mut self.held) {
                self.reply(rig, &held);
            }
            if let Some(at) = self.hold.iter().position(|c| *c == kind) {
                self.hold.remove(at);
                self.held.push(request);
                continue;
            }
            self.reply(rig, &request);
        }
    }

    fn reply(&mut self, rig: &Rig, request: &Value) {
        if let Some(reply) = self.answer(request) {
            rig.say(&reply);
        }
    }

    pub(super) fn run_until(
        &mut self,
        rig: &mut Rig,
        what: &str,
        done: impl Fn(&Driver, &[Value]) -> bool,
    ) {
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

    /// Answers omp's side until a request of `kind` is held. The written frames are read on
    /// another thread, so a request can show up in `rig.sent()` before `serve` has seen it.
    fn wait_held(&mut self, rig: &mut Rig, kind: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.held.iter().any(|request| request["type"] == kind) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a held {kind}; sent {:?}",
                rig.sent()
            );
            rig.driver.pump();
            self.serve(rig);
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Answers omp's side until the mark or the restore under way has ended, along with the
    /// reload of the thread that follows a restore.
    pub(super) fn settle(&mut self, rig: &mut Rig) {
        self.run_until(rig, "omp's part to end", |d, _| {
            !d.marking() && !d.rewinding() && !d.loading_history()
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
pub(super) fn next_run(rig: &mut Rig, omp: &mut FakeOmp, turn: u32) {
    let before = saves(rig).len();
    assert!(rig.driver.prompt(&format!("prompt {turn}")));
    end_run(rig);
    omp.ran(turn);
    let request = wait_sent(rig, "ch_checkpoint", before + 1).remove(before);
    answer_save(rig, &request, turn);
    omp.settle(rig);
}

/// Sends a restore of `turn` and answers omp's side until the code request is out.
pub(super) fn start(rig: &mut Rig, omp: &mut FakeOmp, turn: u32) -> Value {
    let before = of_type(&rig.sent(), "ch_restore").len();
    assert!(rig.driver.can_restore());
    assert!(rig.driver.restore(turn));
    let request = omp.wait_sent(rig, "ch_restore", before + 1).remove(before);
    assert_eq!(request["turn"], turn);
    request["id"].clone()
}

/// Ends the code half of restore `id` and answers the diff refresh that follows it, which
/// would otherwise hold the next restore.
pub(super) fn restored(rig: &mut Rig, omp: &mut FakeOmp, id: &Value, turn: u32) {
    let diffs = of_type(&rig.sent(), "ch_diff").len();
    rig.say(&json!({"type":"ch_restored","id":id,"turn":turn}));
    let refresh = wait_sent(rig, "ch_diff", diffs + 1).remove(diffs);
    rig.say(&diff_result(&refresh["id"], "restored"));
    omp.settle(rig);
    rig.pump_until("the refreshed diff", |d, _| d.can_restore());
}

/// Restores `turn` with both halves agreeing; the runner saves the tree it replaces as `undo`.
pub(super) fn restore_both(rig: &mut Rig, omp: &mut FakeOmp, turn: u32, undo: u32) {
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

/// Starts a fresh driver on the saved session, as a resume does, with the runner's
/// `checkpoints`.
fn resumed(omp: &mut FakeOmp, checkpoints: &Value) -> Rig {
    omp.reload();
    let mut rig = Rig::new();
    assert!(rig.driver.resume(
        "s1",
        vec!["--resume=/home/me/.omp/sessions/s1.jsonl".to_owned()],
        BTreeMap::new(),
        Vec::new(),
    ));
    rig.hello();
    rig.say(&json!({"type":"ready","protocolVersion":1}));
    rig.say(&json!({"type":"ch_checkpoints","checkpoints":checkpoints}));
    rig
}

pub(super) fn noted(rig: &Rig, text: &str) -> bool {
    rig.driver.log().any(|line| line.contains(text))
}

pub(super) fn texts(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_owned()).collect()
}

pub(super) fn pair(prompt: &str, reply: &str) -> (String, String) {
    (prompt.to_owned(), reply.to_owned())
}

/// Whether a `then` frame was written after the last `first` frame.
pub(super) fn sent_after(rig: &Rig, first: &str, then: &str) -> bool {
    let sent = rig.sent();
    let last = sent.iter().rposition(|frame| frame["type"] == first);
    last.is_some_and(|at| sent[at..].iter().any(|frame| frame["type"] == then))
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
    assert!(noted(
        &rig,
        "omp does not know the set_label command, so checkpoints can't be marked"
    ));
    assert!(noted(
        &rig,
        "omp does not know the set_label command, which a restore needs. Code and conversation \
         were left unchanged."
    ));
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

    // No label was tried, so only the move shows omp lacks a command.
    assert!(rig.driver.restore(0));
    omp.settle(&mut rig);
    flush(&mut rig);
    let sent = rig.sent();
    assert_eq!(of_type(&sent, NAVIGATE).len(), 1, "{sent:?}");
    assert!(of_type(&sent, "ch_restore").is_empty(), "{sent:?}");
    assert!(noted(
        &rig,
        "omp does not know the navigate_tree command, which a restore needs."
    ));
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

    let mut rig = resumed(
        &mut omp,
        &json!([
            checkpoint(1, "run"),
            checkpoint(2, "run"),
            checkpoint(3, "before_restore")
        ]),
    );
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
    rig.pump_until("a page request", |d, sent| {
        d.session().checkpoints().len() == 3 && !of_type(sent, HISTORY_COMMAND).is_empty()
    });
    assert!(
        !rig.driver.can_restore(),
        "a move could race the earlier conversation's load"
    );
    omp.run_until(&mut rig, "the end of the load", |d, _| !d.loading_history());

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

static SLOW_MOVE_NOW: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_slow_move_that_lands_after_the_move_back_is_undone() {
    let mut rig = Rig::with_clock(|| SLOW_MOVE_NOW.load(Ordering::SeqCst));
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    omp.hold.push(NAVIGATE);
    assert!(rig.driver.restore(1));
    omp.wait_sent(&mut rig, NAVIGATE, 1);
    SLOW_MOVE_NOW.fetch_add(15_000, Ordering::SeqCst);
    // omp ends the first move, then runs the move back.
    omp.settle(&mut rig);
    let sent = rig.sent();
    assert_eq!(of_type(&sent, NAVIGATE).len(), 2, "{sent:?}");
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
    assert!(rig.driver.can_prompt());
}

static BUSY_ONCE_NOW: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_move_back_omp_is_too_busy_for_is_tried_again() {
    let mut rig = Rig::with_clock(|| BUSY_ONCE_NOW.load(Ordering::SeqCst));
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    let id = start(&mut rig, &mut omp, 1);
    omp.fail_next(NAVIGATE, "Session is busy", Some("session_busy"));
    rig.say(&json!({"type":"ch_error","id":id,"stage":"restore","message":"dirty tree"}));
    omp.run_until(&mut rig, "a pause before the next move back", |d, _| {
        d.pausing_restore()
    });
    assert_eq!(of_type(&rig.sent(), NAVIGATE).len(), 2);
    assert!(
        sent_after(&rig, NAVIGATE, ENTRIES_COMMAND),
        "where omp is was read after the failed move back"
    );
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));

    BUSY_ONCE_NOW.fetch_add(1_000, Ordering::SeqCst);
    omp.settle(&mut rig);
    assert_eq!(of_type(&rig.sent(), NAVIGATE).len(), 3);
    assert!(noted(
        &rig,
        "The runner would not restore checkpoint 1, so the conversation was moved back."
    ));
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
    assert!(rig.driver.can_prompt());
}

static ALWAYS_BUSY_NOW: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_move_back_that_never_lands_is_reported() {
    let mut rig = Rig::with_clock(|| ALWAYS_BUSY_NOW.load(Ordering::SeqCst));
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    let id = start(&mut rig, &mut omp, 1);
    omp.fail_next(NAVIGATE, "Session is busy", Some("session_busy"));
    omp.fail_next(NAVIGATE, "Session is busy", Some("session_busy"));
    rig.say(&json!({"type":"ch_error","id":id,"stage":"restore","message":"dirty tree"}));
    omp.run_until(&mut rig, "a pause before the next move back", |d, _| {
        d.pausing_restore()
    });
    ALWAYS_BUSY_NOW.fetch_add(15_000, Ordering::SeqCst);
    omp.settle(&mut rig);
    assert_eq!(of_type(&rig.sent(), NAVIGATE).len(), 3);
    assert!(noted(
        &rig,
        "The runner would not restore checkpoint 1, and omp would not move the conversation \
         back: Session is busy. The code did not change, but the agent may now see the \
         conversation as it was at checkpoint 1."
    ));
    assert_eq!(omp.path(), texts(&["prompt 1", "reply 1"]));
    assert!(rig.driver.can_prompt());
}

static LATE_BACK_NOW: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_late_move_back_is_checked_before_it_is_reported() {
    let mut rig = Rig::with_clock(|| LATE_BACK_NOW.load(Ordering::SeqCst));
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);

    let id = start(&mut rig, &mut omp, 1);
    omp.hold.push(NAVIGATE);
    rig.say(&json!({"type":"ch_error","id":id,"stage":"restore","message":"dirty tree"}));
    omp.wait_sent(&mut rig, NAVIGATE, 2);
    LATE_BACK_NOW.fetch_add(15_000, Ordering::SeqCst);
    // The read that follows lets omp end the held move back first.
    omp.settle(&mut rig);
    assert!(sent_after(&rig, NAVIGATE, ENTRIES_COMMAND));
    assert_eq!(of_type(&rig.sent(), NAVIGATE).len(), 2);
    assert!(noted(
        &rig,
        "The runner would not restore checkpoint 1, so the conversation was moved back."
    ));
    assert_eq!(
        omp.path(),
        texts(&["prompt 1", "reply 1", "prompt 2", "reply 2"])
    );
    assert!(rig.driver.can_prompt());
}

#[test]
fn after_a_restore_the_thread_shows_what_a_resume_would() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    restore_both(&mut rig, &mut omp, 1, 3);
    let reloaded = rig
        .driver
        .take_history()
        .expect("the thread was read again");
    assert!(reloaded.replaces);
    assert_eq!(reloaded.problem, None);
    assert_eq!(reloaded.turns, [pair("prompt 1", "reply 1")]);
    assert_eq!(
        rig.driver.reply(),
        "",
        "the last reply is in the thread read back, or gone with the turns after it"
    );
    drop(rig);

    let mut rig = resumed(
        &mut omp,
        &json!([
            checkpoint(1, "run"),
            checkpoint(2, "run"),
            checkpoint(3, "before_restore")
        ]),
    );
    omp.run_until(&mut rig, "the earlier conversation", |d, _| {
        !d.loading_history()
    });
    let earlier = rig.driver.take_history().expect("the earlier conversation");
    assert_eq!(earlier.problem, None);
    assert_eq!(earlier.turns, reloaded.turns);
}

#[test]
fn the_next_prompt_shows_its_reply_again() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    restore_both(&mut rig, &mut omp, 0, 2);
    let reloaded = rig
        .driver
        .take_history()
        .expect("the thread was read again");
    assert_eq!(reloaded.turns, []);
    assert_eq!(rig.driver.reply(), "");

    next_run(&mut rig, &mut omp, 3);
    assert_eq!(rig.driver.reply(), "Done.");
}

#[test]
fn a_prompt_waits_for_the_reload_after_a_restore() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    omp.hold.push(HISTORY_COMMAND);
    let id = start(&mut rig, &mut omp, 1);
    rig.say(&json!({"type":"ch_checkpointed","id":id,
        "checkpoint":checkpoint(3, "before_restore")}));
    let diffs = of_type(&rig.sent(), "ch_diff").len();
    rig.say(&json!({"type":"ch_restored","id":id,"turn":1}));
    let refresh = wait_sent(&mut rig, "ch_diff", diffs + 1).remove(diffs);
    rig.say(&diff_result(&refresh["id"], "restored"));
    omp.wait_held(&mut rig, HISTORY_COMMAND);
    assert!(rig.driver.loading_history());

    let prompts = of_type(&rig.sent(), "prompt").len();
    assert!(!rig.driver.can_prompt());
    assert!(!rig.driver.prompt("prompt 3"));
    assert_eq!(of_type(&rig.sent(), "prompt").len(), prompts);

    for held in std::mem::take(&mut omp.held) {
        omp.reply(&rig, &held);
    }
    omp.settle(&mut rig);
    let reloaded = rig
        .driver
        .take_history()
        .expect("the thread was read again");
    assert_eq!(reloaded.turns, [pair("prompt 1", "reply 1")]);
    next_run(&mut rig, &mut omp, 4);
    assert_eq!(rig.driver.reply(), "Done.");
}

#[test]
fn a_reload_that_fails_leaves_the_thread_and_says_so() {
    let mut rig = Rig::new();
    let mut omp = saved_run_on(&mut rig, FakeOmp::new());
    next_run(&mut rig, &mut omp, 2);
    omp.fail_next(HISTORY_COMMAND, "Session is busy", Some("session_busy"));
    restore_both(&mut rig, &mut omp, 1, 3);
    let reloaded = rig.driver.take_history().expect("the reload ended");
    assert!(reloaded.replaces);
    let problem = reloaded.problem.expect("the reload failed");
    assert!(
        problem.starts_with("The thread was not reloaded after the restore"),
        "{problem}"
    );
    assert_eq!(
        rig.driver.reply(),
        "Done.",
        "the thread keeps its last reply"
    );
    assert!(rig.driver.can_prompt());
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
