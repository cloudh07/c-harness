//! Marking each saved checkpoint in omp's conversation, with a label on the entry its run ended
//! at. A restore can only move the conversation to a checkpoint that has a mark.

use serde_json::json;

use super::tree::{Reply, label_for};
use super::{Asked, Job, LABEL_COMMAND, RUNNER_GONE, UNKNOWN_SINCE};
use crate::driver::{Driver, OmpState};

/// Labels the entry the last run ended at with the checkpoint the runner saved for it.
pub(super) struct Mark {
    /// The runner save this marks.
    save: String,
    /// The entries read while `leaf` is unknown, then the label.
    pub(super) asked: Option<Asked>,
    leaf: Option<String>,
    turn: Option<u32>,
    /// Why the mark failed before the save said which checkpoint it was for.
    lost: Option<String>,
}

fn unmarked(turn: Option<u32>, why: &str) -> String {
    let which = turn.map_or_else(
        || "The last checkpoint".to_owned(),
        |turn| format!("Checkpoint {turn}"),
    );
    format!("{which} could not be marked in the conversation, so it can't be restored: {why}")
}

impl Driver {
    /// The last checkpoint is being marked in the conversation, and omp has time left.
    pub(in crate::driver) fn marking(&self) -> bool {
        let Some(Job::Mark(mark)) = &self.rewind.job else {
            return false;
        };
        match &mark.asked {
            Some(asked) => !self.late(asked),
            None => self.save_open(),
        }
    }

    /// Starts marking the checkpoint `save` asked for: reads where the run ended in omp.
    pub(in crate::driver) fn start_mark(&mut self, save: String) {
        if self.rewind.missing.is_some() || self.rewinding() {
            return;
        }
        let Some(asked) = self.ask_entries() else {
            return;
        };
        self.rewind.job = Some(Job::Mark(Mark {
            save,
            asked: Some(asked),
            leaf: None,
            turn: None,
            lost: None,
        }));
    }

    /// The runner answered the save `save`: with the checkpoint's turn, or `None` if it failed.
    pub(in crate::driver) fn mark_turn(&mut self, save: &str, turn: Option<u32>) {
        let Some(Job::Mark(mut mark)) = self
            .rewind
            .job
            .take_if(|job| matches!(job, Job::Mark(mark) if mark.save == save))
        else {
            return;
        };
        // No checkpoint was saved, so there is nothing to mark.
        let Some(turn) = turn else {
            return;
        };
        mark.turn = Some(turn);
        if let Some(why) = mark.lost.take() {
            self.note(&unmarked(Some(turn), &why));
            return;
        }
        self.rewind.job = self.label_mark(mark);
    }

    pub(super) fn on_mark_reply(&mut self, mut mark: Mark, reply: Reply) -> Option<Job> {
        mark.asked = None;
        let Some(leaf) = mark.leaf.clone() else {
            return self.on_mark_entries(mark, reply);
        };
        match reply {
            Reply::Done(_) => {
                let mut turns = self.rewind.turns_at(&leaf);
                turns.extend(mark.turn);
                self.rewind.place(&leaf, &turns);
                None
            }
            Reply::Unknown(command) => self.omp_lacks(command),
            Reply::Failed { error, .. } => self.mark_failed(mark, error),
        }
    }

    fn on_mark_entries(&mut self, mut mark: Mark, reply: Reply) -> Option<Job> {
        match reply {
            Reply::Done(data) => {
                let leaf = self.rewind.read(data);
                match leaf.filter(|leaf| !self.rewind.prompts.contains_key(leaf)) {
                    Some(leaf) => {
                        mark.leaf = Some(leaf);
                        self.label_mark(mark)
                    }
                    None => self.mark_failed(
                        mark,
                        "the conversation holds nothing from this run to mark".to_owned(),
                    ),
                }
            }
            Reply::Unknown(command) => self.omp_lacks(command),
            Reply::Failed {
                code: Some(code), ..
            } if code == UNKNOWN_SINCE && self.rewind.cursor.is_some() => {
                self.rewind.forget();
                match self.ask_entries() {
                    Some(asked) => {
                        mark.asked = Some(asked);
                        Some(Job::Mark(mark))
                    }
                    None => self.mark_failed(mark, RUNNER_GONE.to_owned()),
                }
            }
            Reply::Failed { error, .. } => self.mark_failed(
                mark,
                format!("omp could not list the conversation: {error}"),
            ),
        }
    }

    /// Sends the label once both the entry and the checkpoint are known.
    fn label_mark(&mut self, mut mark: Mark) -> Option<Job> {
        let (Some(leaf), Some(turn)) = (&mark.leaf, mark.turn) else {
            return Some(Job::Mark(mark));
        };
        let mut turns = self.rewind.turns_at(leaf);
        turns.push(turn);
        let frame = json!({ "entryId": leaf, "label": label_for(&turns) });
        match self.ask(LABEL_COMMAND, frame) {
            Some(asked) => {
                mark.asked = Some(asked);
                Some(Job::Mark(mark))
            }
            None => self.mark_failed(mark, RUNNER_GONE.to_owned()),
        }
    }

    /// Says the mark failed. Until the save answers, nothing says which checkpoint it was for,
    /// so the note waits for that answer.
    fn mark_failed(&mut self, mut mark: Mark, why: String) -> Option<Job> {
        if let Some(turn) = mark.turn {
            self.note(&unmarked(Some(turn), &why));
            return None;
        }
        // The save will never answer, and the runner's own notice says it is gone.
        if self.closed {
            return None;
        }
        if self.save_open() {
            mark.asked = None;
            mark.lost = Some(why);
            return Some(Job::Mark(mark));
        }
        self.note(&unmarked(None, &why));
        None
    }

    /// omp does not know a command a mark needs, so no checkpoint can be marked or restored.
    fn omp_lacks(&mut self, command: &'static str) -> Option<Job> {
        self.rewind.missing = Some(command);
        self.note(&format!(
            "omp does not know the {command} command, so checkpoints can't be marked in the \
             conversation or restored."
        ));
        None
    }

    pub(super) fn tick_mark(&mut self, mut mark: Mark) -> Option<Job> {
        if self.closed {
            return self.mark_failed(mark, RUNNER_GONE.to_owned());
        }
        if self.omp != OmpState::Running {
            let why = mark.lost.take().unwrap_or_else(|| "omp stopped".to_owned());
            return self.mark_failed(mark, why);
        }
        match &mark.asked {
            Some(asked) if self.late(asked) => {
                self.mark_failed(mark, "omp did not answer in time".to_owned())
            }
            Some(_) => Some(Job::Mark(mark)),
            None if self.save_open() => Some(Job::Mark(mark)),
            None => {
                let why = mark
                    .lost
                    .take()
                    .unwrap_or_else(|| "the runner took too long to save it".to_owned());
                self.mark_failed(mark, why)
            }
        }
    }
}
