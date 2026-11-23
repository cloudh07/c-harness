//! Restoring a checkpoint in both halves: omp's conversation and the runner's working tree.
//!
//! Each saved checkpoint is marked in omp's conversation with a label on the entry its run ended
//! at. A restore moves omp there first, in memory only, then asks the runner for the code, and
//! only then labels the entry omp left, which makes the move survive a resume. A step that fails
//! undoes the one before it, so both halves end at the same turn or both stay where they were.
//! omp must know `get_entries`, `navigate_tree` and `set_label`. The first answer that says it
//! does not know one of them turns marks and restores off for the session, with a note.

use std::collections::HashMap;

use ch_core::ENTRIES_COMMAND;
use ch_protocol::runner::{CheckpointKind, ClientFrame, RunnerFrame};
use serde_json::{Value, json};

use super::{Driver, OmpState, Request};

mod mark;
mod tree;

use mark::Mark;
use tree::{Reply, label_for, reply_to};

const LABEL_COMMAND: &str = "set_label";
const NAVIGATE_COMMAND: &str = "navigate_tree";
const UNKNOWN_SINCE: &str = "unknown_since";
/// How long omp has to answer one step of a mark or a restore.
const STEP_WAIT_MS: u64 = 15_000;
/// How long the runner may take over the code before the note says it is still at it. The
/// restore keeps waiting: its answer is the only word on what the tree now holds.
const CODE_WAIT_MS: u64 = 120_000;
/// How many times the label that keeps a restore across a resume is sent.
const PERSIST_TRIES: u8 = 2;
/// How long to wait before moving omp back again, after a move back that did not land.
const RETRY_PAUSE_MS: u64 = 1_000;
const UNCHANGED: &str = "Code and conversation were left unchanged.";
const GONE: &str =
    "The runner is gone, so the restore stopped. Code and conversation were left unchanged.";
const RUNNER_GONE: &str = "the runner is gone";
const LATE: &str = "it did not answer in time";

/// What the driver knows of omp's conversation tree, and the mark or restore under way.
#[derive(Default)]
pub(super) struct Rewind {
    /// A command omp said it does not know, which turns marks and restores off.
    missing: Option<&'static str>,
    /// The entry each checkpoint's run ended at, as the labels say.
    places: HashMap<u32, String>,
    /// The first prompt. Moving there leaves no message, which is checkpoint 0.
    first_user: Option<String>,
    /// Prompts and the entry before each. A move to one lands just before it, so none is
    /// marked.
    prompts: HashMap<String, Option<String>>,
    /// The last entry read, so the next read asks only for newer ones.
    cursor: Option<String>,
    job: Option<Job>,
    /// A restore reloaded the conversation, so the last run's reply is no longer in it.
    pub(super) reply_dropped: bool,
}

enum Job {
    Mark(Mark),
    Restore(Restore),
}

/// A request sent to omp: its command, and when.
struct Asked {
    id: String,
    at: u64,
    command: &'static str,
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
    /// Waiting for the runner to restore the code, asked at `at`. `told` once the note said
    /// it is taking long.
    Code { at: u64, told: bool },
    /// Moving omp back to where it was.
    Back(Undo),
    /// Reading where omp is, after a move back failed with `error` or did not answer.
    Check { undo: Undo, error: String },
    /// Waiting until `at` to move omp back again.
    Pause { undo: Undo, at: u64 },
    /// Labelling the entry omp left, so the move survives a resume.
    Persist { turns: Vec<u32>, tries: u8 },
    /// Labelling the entry omp went back to with `saved`, the checkpoint the runner made of
    /// the tree before a restore it did not finish.
    KeepUndo { turns: Vec<u32>, saved: u32 },
}

impl Step {
    /// The checkpoint of the tree before the restore, once a refusal shows the runner made one.
    fn saved(&self) -> Option<u32> {
        match self {
            Self::Back(undo) | Self::Check { undo, .. } | Self::Pause { undo, .. } => {
                undo.why.saved()
            }
            Self::KeepUndo { saved, .. } => Some(*saved),
            Self::Read | Self::Navigate | Self::Code { .. } | Self::Persist { .. } => None,
        }
    }
}

/// A move back, tried again while omp has not moved and time is left.
struct Undo {
    why: Why,
    /// When to stop trying, set when the first move back does not land.
    until: Option<u64>,
}

impl Undo {
    fn new(why: Why) -> Self {
        Self { why, until: None }
    }
}

#[derive(Clone, Copy)]
enum Why {
    /// The runner answered the code step with an error. `saved` is the checkpoint it made of
    /// the tree first, when it got that far, so the code may be partly changed.
    Refused {
        saved: Option<u32>,
    },
    Slow,
}

impl Why {
    fn text(self, turn: u32) -> String {
        match self {
            Self::Refused { saved: None } => {
                format!("The runner would not restore checkpoint {turn}")
            }
            Self::Refused { saved: Some(_) } => {
                format!("The runner stopped partway through restoring checkpoint {turn}")
            }
            Self::Slow => {
                format!("omp took too long to move the conversation to checkpoint {turn}")
            }
        }
    }

    fn saved(self) -> Option<u32> {
        match self {
            Self::Refused { saved } => saved,
            Self::Slow => None,
        }
    }
}

fn partly_changed(saved: u32) -> String {
    format!(
        "The code may be partly changed; checkpoint {saved} holds it as it was before the restore."
    )
}

fn lacking(command: &str) -> String {
    format!("omp does not know the {command} command, which a restore needs. {UNCHANGED}")
}

/// A restore cut short by `cause`. omp's move is held in memory only, so it ended too.
fn cut_short(cause: &str, turn: u32, saved: Option<u32>) -> String {
    match saved {
        None => format!("{cause} during the restore. {UNCHANGED}"),
        Some(saved) => format!(
            "{cause} during the restore of checkpoint {turn}. {} The conversation was left \
             unchanged.",
            partly_changed(saved)
        ),
    }
}

fn unsaved(turn: u32, error: &str) -> String {
    format!(
        "Code and conversation are back at checkpoint {turn}, but omp did not save the move: \
         {error}. If this session is resumed, the conversation will reopen where it was before \
         the restore."
    )
}

fn unkept(saved: u32, error: &str) -> String {
    format!(
        "Checkpoint {saved} holds the code from before the restore, but omp did not mark it in \
         the conversation, so it can't be restored: {error}"
    )
}

fn omp_stopped_after_code(turn: u32) -> String {
    format!(
        "The code is back at checkpoint {turn}, but omp stopped before the conversation's move \
         was saved. If this session is resumed, the conversation will reopen where it was before \
         the restore."
    )
}

/// omp may have saved the move, but it ended before it said so.
fn persist_lost(turn: u32) -> String {
    format!(
        "The code is back at checkpoint {turn}, but omp stopped before it confirmed the \
         conversation's move was saved. If this session is resumed, the conversation may reopen \
         at checkpoint {turn} or where it was before the restore."
    )
}

impl Driver {
    /// The conversation half of a restore is under way.
    pub(super) fn rewinding(&self) -> bool {
        matches!(self.rewind.job, Some(Job::Restore(_)))
    }

    /// A move back did not land, and the next one waits for its pause to end.
    #[cfg(test)]
    pub(super) fn pausing_restore(&self) -> bool {
        matches!(
            &self.rewind.job,
            Some(Job::Restore(Restore {
                step: Step::Pause { .. },
                ..
            }))
        )
    }

    /// Moves omp's conversation to checkpoint `turn`, then the code. Refused with a note,
    /// changing nothing, when omp cannot. Returns false only if nothing could be sent.
    pub(super) fn start_restore(&mut self, turn: u32) -> bool {
        if let Some(command) = self.rewind.missing {
            self.note(&lacking(command));
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
            .take_if(|job| matches!(job, Job::Restore(r) if matches!(r.step, Step::Code { .. })))
        else {
            return;
        };
        let turn = restore.turn;
        let omp_gone = self.closed || self.omp != OmpState::Running;
        // The runner saves the tree it replaces before it touches it, so a refusal after that
        // save may have left the code partly changed.
        let saved = restore
            .place
            .as_ref()
            .and_then(|place| self.undo_turn(place.highest));
        self.rewind.job = match frame {
            RunnerFrame::Restored { .. } if omp_gone => {
                self.note(&omp_stopped_after_code(turn));
                None
            }
            RunnerFrame::Restored { .. } => self.persist(restore, 1),
            // omp ended, so its move, kept in memory only, ended with it.
            _ if omp_gone => {
                let text = match saved {
                    None => format!("The runner would not restore checkpoint {turn}. {UNCHANGED}"),
                    Some(saved) => format!(
                        "{}. {} The conversation was left unchanged.",
                        Why::Refused { saved: Some(saved) }.text(turn),
                        partly_changed(saved)
                    ),
                };
                self.note(&text);
                None
            }
            _ => self.move_back(restore, Undo::new(Why::Refused { saved })),
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
        let Some(reply) = asked.and_then(|asked| reply_to(frame, asked)) else {
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

    fn on_restore_reply(&mut self, mut restore: Restore, reply: Reply) -> Option<Job> {
        restore.asked = None;
        if let Reply::Unknown(command) = &reply {
            self.rewind.missing = Some(command);
        }
        let turn = restore.turn;
        let step = std::mem::replace(&mut restore.step, Step::Read);
        match (step, reply) {
            (Step::Read, Reply::Done(data)) => self.navigate(restore, data),
            (
                step @ (Step::Read | Step::Check { .. }),
                Reply::Failed {
                    code: Some(code), ..
                },
            ) if code == UNKNOWN_SINCE && self.rewind.cursor.is_some() => {
                self.rewind.forget();
                restore.step = step;
                restore.asked = self.ask_entries();
                if restore.asked.is_none() {
                    self.note(GONE);
                    return None;
                }
                Some(Job::Restore(restore))
            }
            (Step::Read | Step::Navigate, Reply::Unknown(command)) => {
                self.note(&lacking(command));
                None
            }
            (Step::Navigate, Reply::Done(data))
                if data.get("cancelled").and_then(Value::as_bool) != Some(true) =>
            {
                self.restore_code(restore)
            }
            (Step::Back(undo), Reply::Done(data))
                if data.get("cancelled").and_then(Value::as_bool) != Some(true) =>
            {
                self.moved_back(restore, undo.why)
            }
            (Step::Check { undo, error }, Reply::Done(data)) => {
                self.check_back(restore, undo, &error, data)
            }
            (Step::Persist { turns, .. }, Reply::Done(_)) => {
                if let Some(place) = &restore.place {
                    self.rewind.place(&place.anchor, &turns);
                }
                self.note(&format!(
                    "The conversation is back at checkpoint {turn} too. The thread now shows it \
                     as the agent sees it."
                ));
                self.reload_history();
                None
            }
            (Step::KeepUndo { turns, .. }, Reply::Done(_)) => {
                if let Some(place) = &restore.place {
                    self.rewind.place(&place.anchor, &turns);
                }
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
                 {UNCHANGED}"
            ));
            return None;
        };
        let highest = self.session.checkpoints().iter().map(|c| c.turn).max();
        restore.place = Some(Place {
            anchor,
            highest: highest.unwrap_or(0),
        });
        restore.step = Step::Navigate;
        let frame = json!({ "entryId": target, "summarize": false });
        self.ask_for(restore, NAVIGATE_COMMAND, frame)
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
        restore.step = Step::Code {
            at: (self.clock)(),
            told: false,
        };
        Some(Job::Restore(restore))
    }

    fn move_back(&mut self, mut restore: Restore, undo: Undo) -> Option<Job> {
        let anchor = restore.place.as_ref()?.anchor.clone();
        restore.step = Step::Back(undo);
        let frame = json!({ "entryId": anchor, "summarize": false });
        self.ask_for(restore, NAVIGATE_COMMAND, frame)
    }

    /// A move back failed or is late. omp runs one command at a time, so the entries read next
    /// show where every move sent so far left it.
    fn verify_back(&mut self, mut restore: Restore, mut undo: Undo, error: &str) -> Option<Job> {
        undo.until.get_or_insert((self.clock)() + STEP_WAIT_MS);
        restore.step = Step::Check {
            undo,
            error: error.to_owned(),
        };
        let Some(asked) = self.ask_entries() else {
            self.note(GONE);
            return None;
        };
        restore.asked = Some(asked);
        Some(Job::Restore(restore))
    }

    /// Ends the restore once omp is back where it was, or tries again while time is left.
    fn check_back(
        &mut self,
        mut restore: Restore,
        undo: Undo,
        error: &str,
        data: &Value,
    ) -> Option<Job> {
        let leaf = self.rewind.read(data);
        let back = restore
            .place
            .as_ref()
            .is_some_and(|place| leaf == self.rewind.landing(&place.anchor));
        if back {
            return self.moved_back(restore, undo.why);
        }
        let now = (self.clock)();
        if undo.until.is_some_and(|until| now < until) {
            restore.step = Step::Pause {
                undo,
                at: now + RETRY_PAUSE_MS,
            };
            return Some(Job::Restore(restore));
        }
        self.not_moved_back(restore.turn, undo.why, error);
        None
    }

    /// omp is back where it was. A refusal that came after the runner saved the tree it was
    /// replacing labels that save, so it can be restored like any other checkpoint.
    fn moved_back(&mut self, mut restore: Restore, why: Why) -> Option<Job> {
        let text = why.text(restore.turn);
        let Some(saved) = why.saved() else {
            self.note(&format!(
                "{text}, so the conversation was moved back. {UNCHANGED}"
            ));
            return None;
        };
        self.note(&format!(
            "{text}, so the conversation was moved back. {}",
            partly_changed(saved)
        ));
        let anchor = restore.place.as_ref()?.anchor.clone();
        let mut turns = self.rewind.turns_at(&anchor);
        turns.push(saved);
        let frame = json!({ "entryId": anchor, "label": label_for(&turns) });
        restore.step = Step::KeepUndo { turns, saved };
        let Some(asked) = self.ask(LABEL_COMMAND, frame) else {
            self.note(&unkept(saved, RUNNER_GONE));
            return None;
        };
        restore.asked = Some(asked);
        Some(Job::Restore(restore))
    }

    fn not_moved_back(&mut self, turn: u32, why: Why, error: &str) {
        let text = why.text(turn);
        self.note(&match why.saved() {
            None => format!(
                "{text}, and omp would not move the conversation back: {error}. The code did \
                 not change, but the agent may now see the conversation as it was at checkpoint \
                 {turn}."
            ),
            Some(saved) => format!(
                "{text}, and omp would not move the conversation back: {error}. The code may be \
                 partly changed, and the agent may now see the conversation as it was at \
                 checkpoint {turn}. Checkpoint {saved} holds the code as it was before the \
                 restore."
            ),
        });
    }

    /// Labels the entry omp left with the checkpoint the runner saved of the tree it replaced.
    /// The label is the newest entry, so a resume reopens at the restored turn, and restoring
    /// that checkpoint later brings both halves back.
    fn persist(&mut self, mut restore: Restore, tries: u8) -> Option<Job> {
        let place = restore.place.as_ref()?;
        let mut turns = self.rewind.turns_at(&place.anchor);
        turns.extend(self.undo_turn(place.highest));
        let mut frame = json!({ "entryId": place.anchor });
        if !turns.is_empty() {
            frame["label"] = json!(label_for(&turns));
        }
        restore.step = Step::Persist { turns, tries };
        let Some(asked) = self.ask(LABEL_COMMAND, frame) else {
            self.note(&unsaved(restore.turn, RUNNER_GONE));
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

    fn step_failed(&mut self, mut restore: Restore, error: &str) -> Option<Job> {
        let turn = restore.turn;
        let text = match std::mem::replace(&mut restore.step, Step::Read) {
            Step::Read => format!("omp could not list the conversation: {error}. {UNCHANGED}"),
            Step::Navigate => format!(
                "omp would not move the conversation to checkpoint {turn}: {error}. {UNCHANGED}"
            ),
            step @ (Step::Code { .. } | Step::Pause { .. }) => {
                restore.step = step;
                return Some(Job::Restore(restore));
            }
            Step::Back(undo) => return self.verify_back(restore, undo, error),
            // The entries could not be read, so the move back's own error says the most.
            Step::Check { undo, error } => {
                self.not_moved_back(turn, undo.why, &error);
                return None;
            }
            Step::Persist { tries, .. } if tries < PERSIST_TRIES => {
                return self.persist(restore, tries + 1);
            }
            // omp holds the move in memory, so the agent sees the restored conversation now
            // even though a resume would not.
            Step::Persist { .. } => {
                self.note(&format!(
                    "{} The thread now shows the conversation as the agent sees it.",
                    unsaved(turn, error)
                ));
                self.reload_history();
                return None;
            }
            Step::KeepUndo { saved, .. } => unkept(saved, error),
        };
        self.note(&text);
        None
    }

    fn tick_restore(&mut self, mut restore: Restore) -> Option<Job> {
        let turn = restore.turn;
        let saved = restore.step.saved();
        if self.closed {
            let text = match restore.step {
                Step::Code { .. } => format!(
                    "The runner closed during the restore of checkpoint {turn}, so the code may \
                     have changed. The conversation was left unchanged."
                ),
                Step::Persist { .. } => persist_lost(turn),
                Step::KeepUndo { saved, .. } => unkept(saved, RUNNER_GONE),
                Step::Read
                | Step::Navigate
                | Step::Back(_)
                | Step::Check { .. }
                | Step::Pause { .. } => cut_short("The runner closed", turn, saved),
            };
            self.note(&text);
            return None;
        }
        // The code step waits on the runner, which answers every restore.
        if let Step::Code { at, told } = &mut restore.step {
            if !*told && (self.clock)().saturating_sub(*at) >= CODE_WAIT_MS {
                *told = true;
                self.note(&format!(
                    "The runner is still restoring the code of checkpoint {turn}. Prompts, \
                     review and land wait until it finishes."
                ));
            }
            return Some(Job::Restore(restore));
        }
        if self.omp != OmpState::Running {
            let text = match restore.step {
                Step::Persist { .. } => persist_lost(turn),
                Step::KeepUndo { saved, .. } => unkept(saved, "omp stopped"),
                Step::Read
                | Step::Navigate
                | Step::Code { .. }
                | Step::Back(_)
                | Step::Check { .. }
                | Step::Pause { .. } => cut_short("omp stopped", turn, saved),
            };
            self.note(&text);
            return None;
        }
        let late = restore.asked.as_ref().is_some_and(|asked| self.late(asked));
        match std::mem::replace(&mut restore.step, Step::Read) {
            Step::Pause { undo, at } if (self.clock)() >= at => self.move_back(restore, undo),
            // It may still move. omp runs one command at a time, so a move back sent now lands
            // after it.
            Step::Navigate if late => self.move_back(restore, Undo::new(Why::Slow)),
            step if late => {
                restore.step = step;
                self.step_failed(restore, LATE)
            }
            step => {
                restore.step = step;
                Some(Job::Restore(restore))
            }
        }
    }

    fn late(&self, asked: &Asked) -> bool {
        (self.clock)().saturating_sub(asked.at) >= STEP_WAIT_MS
    }

    fn ask_entries(&mut self) -> Option<Asked> {
        let mut frame = json!({});
        if let Some(since) = &self.rewind.cursor {
            frame["since"] = json!(since);
        }
        self.ask(ENTRIES_COMMAND, frame)
    }

    fn ask_for(
        &mut self,
        mut restore: Restore,
        command: &'static str,
        frame: Value,
    ) -> Option<Job> {
        let Some(asked) = self.ask(command, frame) else {
            self.note(GONE);
            return None;
        };
        restore.asked = Some(asked);
        Some(Job::Restore(restore))
    }

    /// Sends `command` to omp with the fields in `frame`.
    fn ask(&mut self, command: &'static str, mut frame: Value) -> Option<Asked> {
        let id = self.next_id();
        frame["type"] = json!(command);
        frame["id"] = json!(id);
        let at = (self.clock)();
        self.send_omp(&frame, None)
            .then_some(Asked { id, at, command })
    }
}
