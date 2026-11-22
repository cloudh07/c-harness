//! Restoring a checkpoint in both halves: omp's conversation and the runner's working tree.
//!
//! Each saved checkpoint is marked in omp's conversation with a label on the entry its run ended
//! at. A restore moves omp there first, in memory only, then asks the runner for the code, and
//! only then labels the entry omp left, which makes the move survive a resume. A step that fails
//! undoes the one before it, so both halves end at the same turn or both stay where they were.
//! It needs omp 18.13.2 or newer, which knows `set_label` and `navigate_tree`.

use std::collections::{HashMap, HashSet};

use ch_core::ENTRIES_COMMAND;
use ch_protocol::runner::{CheckpointKind, ClientFrame, RunnerFrame};
use serde_json::{Value, json};

use super::{Driver, OmpState, Request};

pub(super) const LABEL_PREFIX: &str = "c-harness:turn:";
const LABEL_COMMAND: &str = "set_label";
const NAVIGATE_COMMAND: &str = "navigate_tree";
const UNKNOWN_SINCE: &str = "unknown_since";
/// How long omp has to answer one step of a mark or a restore.
const STEP_WAIT_MS: u64 = 15_000;
/// How many times the label that keeps a restore across a resume is sent.
const PERSIST_TRIES: u8 = 2;
pub(super) const TOO_OLD: &str =
    "Restore needs omp 18.13.2 or newer. Code and conversation were left unchanged.";
const UNCHANGED: &str = "Code and conversation were left unchanged.";
const GONE: &str =
    "The runner is gone, so the restore stopped. Code and conversation were left unchanged.";
const LATE: &str = "it did not answer in time";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Support {
    #[default]
    Unknown,
    Yes,
    No,
}

/// What the driver knows of omp's conversation tree, and the mark or restore under way.
#[derive(Default)]
pub(super) struct Rewind {
    support: Support,
    /// The entry each checkpoint's run ended at, as the labels say.
    places: HashMap<u32, String>,
    /// The first prompt. Moving there leaves no message, which is checkpoint 0.
    first_user: Option<String>,
    /// Prompts. A move to one lands just before it, so none is marked.
    prompts: HashSet<String>,
    /// The last entry read, so the next read asks only for newer ones.
    cursor: Option<String>,
    job: Option<Job>,
}

enum Job {
    Mark(Mark),
    Restore(Restore),
}

/// A request sent to omp, and when.
struct Asked {
    id: String,
    at: u64,
}

/// Labels the entry the last run ended at with the checkpoint the runner saved for it.
struct Mark {
    /// The runner save this marks.
    save: String,
    /// The entries read while `leaf` is unknown, then the label.
    asked: Option<Asked>,
    leaf: Option<String>,
    turn: Option<u32>,
}

struct Restore {
    turn: u32,
    step: Step,
    asked: Option<Asked>,
    place: Option<Place>,
}

/// Where omp was before the move, and the highest checkpoint before the restore. The checkpoint
/// the runner saves of the tree it replaces is the one above it.
struct Place {
    anchor: String,
    highest: u32,
}

enum Step {
    /// Reading omp's entries to find where the checkpoint's run ended.
    Read,
    /// Moving omp there, in memory only.
    Navigate,
    /// Waiting for the runner to restore the code.
    Code,
    /// Moving omp back to where it was.
    Back(Why),
    /// Labelling the entry omp left, so the move survives a resume.
    Persist { turns: Vec<u32>, tries: u8 },
}

#[derive(Clone, Copy)]
enum Why {
    Refused,
    Slow,
}

impl Why {
    fn text(self, turn: u32) -> String {
        match self {
            Self::Refused => format!("The runner would not restore checkpoint {turn}"),
            Self::Slow => {
                format!("omp took too long to move the conversation to checkpoint {turn}")
            }
        }
    }
}

/// omp's answer to one request.
enum Reply<'a> {
    Done(&'a Value),
    /// The command is not one this omp knows.
    Unknown,
    Failed {
        error: String,
        code: Option<String>,
    },
}

impl Reply<'_> {
    fn error(self) -> String {
        match self {
            Self::Done(_) => "it cancelled the move".to_owned(),
            Self::Unknown => "it does not know the command".to_owned(),
            Self::Failed { error, .. } => error,
        }
    }
}

fn reply_to<'a>(frame: &'a Value, id: &str) -> Option<Reply<'a>> {
    let answers = frame.get("type").and_then(Value::as_str) == Some("response")
        && frame.get("id").and_then(Value::as_str) == Some(id);
    if !answers {
        return None;
    }
    if frame.get("success").and_then(Value::as_bool) == Some(true) {
        return Some(Reply::Done(frame.get("data").unwrap_or(&Value::Null)));
    }
    let error = frame
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("no reason given");
    if error.to_ascii_lowercase().starts_with("unknown command") {
        return Some(Reply::Unknown);
    }
    Some(Reply::Failed {
        error: error.to_owned(),
        code: frame.get("code").and_then(Value::as_str).map(str::to_owned),
    })
}

/// The label that marks an entry as the end of these checkpoints.
pub(super) fn label_for(turns: &[u32]) -> String {
    let mut turns = turns.to_vec();
    turns.sort_unstable();
    turns.dedup();
    let list: Vec<String> = turns.iter().map(u32::to_string).collect();
    format!("{LABEL_PREFIX}{}", list.join(","))
}

/// The checkpoints a label marks, or `None` for a label this app did not write.
pub(super) fn turns_in(label: &str) -> Option<Vec<u32>> {
    label
        .strip_prefix(LABEL_PREFIX)?
        .split(',')
        .map(|turn| turn.parse().ok())
        .collect()
}

fn unsaved(turn: u32, error: &str) -> String {
    format!(
        "Code and conversation are back at checkpoint {turn}, but omp did not save the move: \
         {error}. If this session is resumed, the conversation will reopen where it was before \
         the restore."
    )
}

fn omp_stopped_after_code(turn: u32) -> String {
    format!(
        "The code is back at checkpoint {turn}, but omp stopped before the conversation's move \
         was saved. If this session is resumed, the conversation will reopen where it was before \
         the restore."
    )
}

impl Rewind {
    /// Folds in entries omp listed in the order it appended them, and returns its leaf.
    fn read(&mut self, data: &Value) -> Option<String> {
        let entries = data.get("entries").and_then(Value::as_array);
        for entry in entries.into_iter().flatten() {
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            self.cursor = Some(id.to_owned());
            match entry.get("type").and_then(Value::as_str) {
                Some("label") => {
                    let Some(target) = entry.get("targetId").and_then(Value::as_str) else {
                        continue;
                    };
                    // One label per entry: a newer one, ours or not, replaces the older.
                    let turns = entry
                        .get("label")
                        .and_then(Value::as_str)
                        .and_then(turns_in);
                    self.place(target, &turns.unwrap_or_default());
                }
                Some("message")
                    if entry.pointer("/message/role").and_then(Value::as_str) == Some("user") =>
                {
                    self.first_user.get_or_insert_with(|| id.to_owned());
                    self.prompts.insert(id.to_owned());
                }
                _ => {}
            }
        }
        data.get("leafId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn place(&mut self, entry: &str, turns: &[u32]) {
        self.places.retain(|_, at| at != entry);
        for turn in turns {
            self.places.insert(*turn, entry.to_owned());
        }
    }

    fn turns_at(&self, entry: &str) -> Vec<u32> {
        let mut turns: Vec<u32> = self
            .places
            .iter()
            .filter(|(_, at)| *at == entry)
            .map(|(turn, _)| *turn)
            .collect();
        turns.sort_unstable();
        turns
    }

    /// Drops what was read, when omp no longer knows the last entry read.
    fn forget(&mut self) {
        self.places.clear();
        self.first_user = None;
        self.prompts.clear();
        self.cursor = None;
    }
}

impl Driver {
    /// The conversation half of a restore is under way.
    pub(super) fn rewinding(&self) -> bool {
        matches!(self.rewind.job, Some(Job::Restore(_)))
    }

    /// The last checkpoint is being marked in the conversation, and omp has time left.
    pub(super) fn marking(&self) -> bool {
        let Some(Job::Mark(mark)) = &self.rewind.job else {
            return false;
        };
        match &mark.asked {
            Some(asked) => !self.late(asked),
            None => self.save_open(),
        }
    }

    /// Starts marking the checkpoint `save` asked for: reads where the run ended in omp.
    pub(super) fn start_mark(&mut self, save: String) {
        if self.rewind.support == Support::No || self.rewinding() {
            return;
        }
        let asked = self.ask_entries();
        self.rewind.job = asked.map(|asked| {
            Job::Mark(Mark {
                save,
                asked: Some(asked),
                leaf: None,
                turn: None,
            })
        });
    }

    /// The runner answered the save `save`: with the checkpoint's turn, or `None` if it failed.
    pub(super) fn mark_turn(&mut self, save: &str, turn: Option<u32>) {
        let Some(Job::Mark(mut mark)) = self
            .rewind
            .job
            .take_if(|job| matches!(job, Job::Mark(mark) if mark.save == save))
        else {
            return;
        };
        let Some(turn) = turn else {
            return;
        };
        mark.turn = Some(turn);
        self.rewind.job = self.label_mark(mark);
    }

    /// Moves omp's conversation to checkpoint `turn`, then the code. Refused with a note,
    /// changing nothing, when omp cannot. Returns false only if nothing could be sent.
    pub(super) fn start_restore(&mut self, turn: u32) -> bool {
        if self.rewind.support == Support::No {
            self.note(TOO_OLD);
            return true;
        }
        let Some(asked) = self.ask_entries() else {
            return false;
        };
        self.rewind.job = Some(Job::Restore(Restore {
            turn,
            step: Step::Read,
            asked: Some(asked),
            place: None,
        }));
        true
    }

    /// The runner answered the code half of a restore.
    pub(super) fn on_code_answer(&mut self, frame: &RunnerFrame) {
        let Some(Job::Restore(restore)) = self
            .rewind
            .job
            .take_if(|job| matches!(job, Job::Restore(r) if matches!(r.step, Step::Code)))
        else {
            return;
        };
        let turn = restore.turn;
        let omp_gone = self.closed || self.omp != OmpState::Running;
        self.rewind.job = match frame {
            RunnerFrame::Restored { .. } if omp_gone => {
                self.note(&omp_stopped_after_code(turn));
                None
            }
            RunnerFrame::Restored { .. } => self.persist(restore, 1),
            // omp ended, so its move, kept in memory only, ended with it.
            _ if omp_gone => {
                self.note(&format!(
                    "The runner would not restore checkpoint {turn}. {UNCHANGED}"
                ));
                None
            }
            _ => self.move_back(restore, Why::Refused),
        };
    }

    /// Reads omp's answer to a mark or restore request.
    pub(super) fn on_rewind_frame(&mut self, frame: &Value) {
        let Some(job) = self.rewind.job.take() else {
            return;
        };
        let asked = match &job {
            Job::Mark(mark) => mark.asked.as_ref(),
            Job::Restore(restore) => restore.asked.as_ref(),
        };
        let Some(reply) = asked.and_then(|asked| reply_to(frame, &asked.id)) else {
            self.rewind.job = Some(job);
            return;
        };
        self.rewind.job = match job {
            Job::Mark(mark) => self.on_mark_reply(mark, reply),
            Job::Restore(restore) => self.on_restore_reply(restore, reply),
        };
    }

    /// Ends a step omp took too long on, or one that can no longer be answered.
    pub(super) fn rewind_tick(&mut self) {
        let Some(job) = self.rewind.job.take() else {
            return;
        };
        self.rewind.job = match job {
            Job::Mark(mark) => self.tick_mark(mark),
            Job::Restore(restore) => self.tick_restore(restore),
        };
    }

    fn on_mark_reply(&mut self, mut mark: Mark, reply: Reply) -> Option<Job> {
        mark.asked = None;
        let Some(leaf) = mark.leaf.clone() else {
            return self.on_mark_entries(mark, reply);
        };
        let turn = mark.turn.unwrap_or_default();
        match reply {
            Reply::Done(_) => {
                let mut turns = self.rewind.turns_at(&leaf);
                turns.push(turn);
                self.rewind.place(&leaf, &turns);
                self.rewind.support = Support::Yes;
            }
            Reply::Unknown => self.rewind.support = Support::No,
            Reply::Failed { error, .. } => self.note(&format!(
                "Checkpoint {turn} could not be marked in the conversation, so it can't be \
                 restored: {error}"
            )),
        }
        None
    }

    fn on_mark_entries(&mut self, mut mark: Mark, reply: Reply) -> Option<Job> {
        match reply {
            Reply::Done(data) => {
                let leaf = self.rewind.read(data);
                mark.leaf = Some(leaf.filter(|leaf| !self.rewind.prompts.contains(leaf))?);
                self.label_mark(mark)
            }
            Reply::Unknown => {
                self.rewind.support = Support::No;
                None
            }
            Reply::Failed {
                code: Some(code), ..
            } if code == UNKNOWN_SINCE && self.rewind.cursor.is_some() => {
                self.rewind.forget();
                mark.asked = Some(self.ask_entries()?);
                Some(Job::Mark(mark))
            }
            Reply::Failed { error, .. } => {
                self.note(&format!(
                    "The last checkpoint could not be marked in the conversation, so it can't \
                     be restored: omp could not list the conversation: {error}"
                ));
                None
            }
        }
    }

    /// Sends the label once both the entry and the checkpoint are known.
    fn label_mark(&mut self, mut mark: Mark) -> Option<Job> {
        let (Some(leaf), Some(turn)) = (&mark.leaf, mark.turn) else {
            return Some(Job::Mark(mark));
        };
        let mut turns = self.rewind.turns_at(leaf);
        turns.push(turn);
        let frame = json!({ "type": LABEL_COMMAND, "entryId": leaf, "label": label_for(&turns) });
        mark.asked = Some(self.ask(frame)?);
        Some(Job::Mark(mark))
    }

    fn on_restore_reply(&mut self, mut restore: Restore, reply: Reply) -> Option<Job> {
        restore.asked = None;
        let turn = restore.turn;
        let step = std::mem::replace(&mut restore.step, Step::Read);
        match (step, reply) {
            (Step::Read, Reply::Done(data)) => self.navigate(restore, data),
            (
                Step::Read,
                Reply::Failed {
                    code: Some(code), ..
                },
            ) if code == UNKNOWN_SINCE && self.rewind.cursor.is_some() => {
                self.rewind.forget();
                restore.asked = self.ask_entries();
                if restore.asked.is_none() {
                    self.note(GONE);
                    return None;
                }
                Some(Job::Restore(restore))
            }
            (Step::Read | Step::Navigate, Reply::Unknown) => {
                self.rewind.support = Support::No;
                self.note(TOO_OLD);
                None
            }
            (Step::Navigate, Reply::Done(data))
                if data.get("cancelled").and_then(Value::as_bool) != Some(true) =>
            {
                self.rewind.support = Support::Yes;
                self.restore_code(restore)
            }
            (Step::Back(why), Reply::Done(_)) => {
                self.note(&format!(
                    "{}, so the conversation was moved back. {UNCHANGED}",
                    why.text(turn)
                ));
                None
            }
            (Step::Persist { turns, .. }, Reply::Done(_)) => {
                if let Some(place) = &restore.place {
                    self.rewind.place(&place.anchor, &turns);
                }
                self.note(&format!(
                    "The conversation is back at checkpoint {turn} too. Turns after it stay on \
                     screen, but the agent no longer sees them."
                ));
                None
            }
            (step, reply) => {
                restore.step = step;
                self.step_failed(restore, &reply.error())
            }
        }
    }

    /// Finds where checkpoint `turn` sits in the entries omp listed, and moves omp there.
    fn navigate(&mut self, mut restore: Restore, data: &Value) -> Option<Job> {
        let leaf = self.rewind.read(data);
        let turn = restore.turn;
        let target = if turn == 0 {
            self.rewind.first_user.clone()
        } else {
            self.rewind.places.get(&turn).cloned()
        };
        // An empty conversation has no leaf. Moving to the first prompt empties it again.
        let anchor = leaf.or_else(|| self.rewind.first_user.clone());
        let (Some(target), Some(anchor)) = (target, anchor) else {
            self.note(&format!(
                "Checkpoint {turn} has no mark in the conversation, so it can't be restored. \
                 Only checkpoints saved with omp 18.13.2 or newer have one. {UNCHANGED}"
            ));
            return None;
        };
        let highest = self.session.checkpoints().iter().map(|c| c.turn).max();
        restore.place = Some(Place {
            anchor,
            highest: highest.unwrap_or(0),
        });
        restore.step = Step::Navigate;
        let frame = json!({ "type": NAVIGATE_COMMAND, "entryId": target, "summarize": false });
        self.ask_for(restore, frame)
    }

    fn restore_code(&mut self, mut restore: Restore) -> Option<Job> {
        let id = self.next_id();
        let frame = ClientFrame::Restore {
            id: id.clone(),
            turn: restore.turn,
        };
        if !self.send_request(id, Request::Restore, &frame, None) {
            self.note(GONE);
            return None;
        }
        restore.step = Step::Code;
        Some(Job::Restore(restore))
    }

    fn move_back(&mut self, mut restore: Restore, why: Why) -> Option<Job> {
        let anchor = restore.place.as_ref()?.anchor.clone();
        restore.step = Step::Back(why);
        let frame = json!({ "type": NAVIGATE_COMMAND, "entryId": anchor, "summarize": false });
        self.ask_for(restore, frame)
    }

    /// Labels the entry omp left with the checkpoint the runner saved of the tree it replaced.
    /// The label is the newest entry, so a resume reopens at the restored turn, and restoring
    /// that checkpoint later brings both halves back.
    fn persist(&mut self, mut restore: Restore, tries: u8) -> Option<Job> {
        let place = restore.place.as_ref()?;
        let mut turns = self.rewind.turns_at(&place.anchor);
        turns.extend(self.undo_turn(place.highest));
        let mut frame = json!({ "type": LABEL_COMMAND, "entryId": place.anchor });
        if !turns.is_empty() {
            frame["label"] = json!(label_for(&turns));
        }
        restore.step = Step::Persist { turns, tries };
        let Some(asked) = self.ask(frame) else {
            self.note(&unsaved(restore.turn, "the runner is gone"));
            return None;
        };
        restore.asked = Some(asked);
        Some(Job::Restore(restore))
    }

    /// The checkpoint the runner saved of the tree a restore replaced.
    fn undo_turn(&self, highest: u32) -> Option<u32> {
        self.session
            .checkpoints()
            .iter()
            .filter(|c| c.kind == CheckpointKind::BeforeRestore && c.turn > highest)
            .map(|c| c.turn)
            .max()
    }

    fn step_failed(&mut self, restore: Restore, error: &str) -> Option<Job> {
        let turn = restore.turn;
        let text = match &restore.step {
            Step::Read => format!("omp could not list the conversation: {error}. {UNCHANGED}"),
            Step::Navigate => format!(
                "omp would not move the conversation to checkpoint {turn}: {error}. {UNCHANGED}"
            ),
            Step::Code => return Some(Job::Restore(restore)),
            Step::Back(why) => format!(
                "{}, and omp would not move the conversation back: {error}. The code did not \
                 change, but the agent may now see the conversation as it was at checkpoint \
                 {turn}.",
                why.text(turn)
            ),
            Step::Persist { tries, .. } if *tries < PERSIST_TRIES => {
                let tries = *tries;
                return self.persist(restore, tries + 1);
            }
            Step::Persist { .. } => unsaved(turn, error),
        };
        self.note(&text);
        None
    }

    fn tick_mark(&mut self, mark: Mark) -> Option<Job> {
        if self.closed || self.omp != OmpState::Running {
            return None;
        }
        match &mark.asked {
            Some(asked) if self.late(asked) => {
                if let (Some(_), Some(turn)) = (&mark.leaf, mark.turn) {
                    self.note(&format!(
                        "omp did not answer in time, so checkpoint {turn} was not marked in the \
                         conversation and can't be restored."
                    ));
                }
                None
            }
            Some(_) => Some(Job::Mark(mark)),
            None if self.save_open() => Some(Job::Mark(mark)),
            None => None,
        }
    }

    fn tick_restore(&mut self, restore: Restore) -> Option<Job> {
        let turn = restore.turn;
        if self.closed {
            let text = match restore.step {
                Step::Code => format!(
                    "The runner closed during the restore of checkpoint {turn}, so the code may \
                     have changed. The conversation was left unchanged."
                ),
                Step::Persist { .. } => omp_stopped_after_code(turn),
                // omp's move is held in memory only, so it ends with the runner.
                Step::Read | Step::Navigate | Step::Back(_) => {
                    format!("The runner closed during the restore. {UNCHANGED}")
                }
            };
            self.note(&text);
            return None;
        }
        // The code step waits on the runner, which answers every restore.
        let Some(asked) = &restore.asked else {
            return Some(Job::Restore(restore));
        };
        if self.omp != OmpState::Running {
            let text = match restore.step {
                Step::Persist { .. } => omp_stopped_after_code(turn),
                Step::Read | Step::Navigate | Step::Code | Step::Back(_) => {
                    format!("omp stopped during the restore. {UNCHANGED}")
                }
            };
            self.note(&text);
            return None;
        }
        if !self.late(asked) {
            return Some(Job::Restore(restore));
        }
        if matches!(restore.step, Step::Navigate) {
            // It may still move. Moving back after it leaves the conversation where it was.
            return self.move_back(restore, Why::Slow);
        }
        self.step_failed(restore, LATE)
    }

    fn late(&self, asked: &Asked) -> bool {
        (self.clock)().saturating_sub(asked.at) >= STEP_WAIT_MS
    }

    fn ask_entries(&mut self) -> Option<Asked> {
        let mut frame = json!({ "type": ENTRIES_COMMAND });
        if let Some(since) = &self.rewind.cursor {
            frame["since"] = json!(since);
        }
        self.ask(frame)
    }

    fn ask_for(&mut self, mut restore: Restore, frame: Value) -> Option<Job> {
        let Some(asked) = self.ask(frame) else {
            self.note(GONE);
            return None;
        };
        restore.asked = Some(asked);
        Some(Job::Restore(restore))
    }

    fn ask(&mut self, mut frame: Value) -> Option<Asked> {
        let id = self.next_id();
        frame["id"] = json!(id);
        let at = (self.clock)();
        self.send_omp(&frame, None).then_some(Asked { id, at })
    }
}
