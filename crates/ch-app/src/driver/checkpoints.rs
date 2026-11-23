//! Turn checkpoints: a save after each run that ends, restores, and diffs of one turn.

use ch_core::{SessionState, run_ended};
use ch_protocol::runner::{ClientFrame, RunnerFrame};

use super::{Driver, Request};

/// How long a save may hold prompts. The runner saves beside other work, so a slow disk
/// should not lock the session for good.
const SAVE_WAIT_MS: u64 = 15_000;

#[derive(Default)]
pub(super) struct Saves {
    /// The save that holds prompts, by request id, and when it went out.
    waiting: Option<(String, u64)>,
}

impl Driver {
    /// Whether a restore can go out now. The runner refuses one while a check, diff or
    /// restore runs, and a save in flight would race it, so every open request blocks it.
    /// omp must be idle too: a restore or a mark under way, or the earlier conversation
    /// still loading, would race the move in its conversation.
    pub fn can_restore(&self) -> bool {
        self.can_review()
            && self.session.checkpoints_on()
            && self.open.is_empty()
            && !self.rewinding()
            && !self.marking()
            && !self.loading_history()
    }

    /// Puts the conversation and the working tree back at checkpoint `turn`, 0 being the
    /// session's base. Returns false only if nothing could be sent; a refusal leaves a note.
    pub fn restore(&mut self, turn: u32) -> bool {
        if !self.can_restore() {
            return false;
        }
        self.start_restore(turn)
    }

    /// Asks for the changes from checkpoint `from` to `to`, or to the working tree.
    pub fn diff_range(&mut self, from: u32, to: Option<u32>) -> bool {
        if !self.can_review() || !self.session.checkpoints_on() {
            return false;
        }
        let id = self.next_id();
        let frame = ClientFrame::Diff {
            id: id.clone(),
            from: Some(from),
            to,
        };
        self.send_request(id, Request::TurnDiff { from, to }, &frame, None)
    }

    /// A save of the last run, or its mark in the conversation, is still on its way, so a new
    /// prompt waits for it.
    pub fn saving_checkpoint(&self) -> bool {
        self.save_open() || self.marking()
    }

    /// The runner has not yet answered the last save, and it has time left.
    pub(super) fn save_open(&self) -> bool {
        self.saves
            .waiting
            .as_ref()
            .is_some_and(|(_, at)| (self.clock)().saturating_sub(*at) < SAVE_WAIT_MS)
    }

    /// A restore is on its way, so the session waits for it.
    pub fn restoring(&self) -> bool {
        self.open.values().any(|kind| *kind == Request::Restore) || self.rewinding()
    }

    /// Saves a checkpoint when the state change `before` to now ended a run. A save that
    /// failed is not repeated; the next run's save holds its changes too. A run with no
    /// reply and no tool calls gets none, so the list holds only turns where the agent worked.
    pub(super) fn save_after_run(&mut self, before: SessionState) {
        let ended = run_ended(before, self.session.state());
        let skip = self.closed || self.session.run_was_empty();
        if !ended || skip || !self.session.checkpoints_on() {
            return;
        }
        let id = self.next_id();
        let frame = ClientFrame::Checkpoint { id: id.clone() };
        if self.send_request(id.clone(), Request::Checkpoint, &frame, None) {
            self.saves.waiting = Some((id.clone(), (self.clock)()));
            self.start_mark(id);
        }
    }

    /// Handles the reply that closed a save or a restore.
    pub(super) fn on_checkpoint_answer(&mut self, kind: Request, frame: &RunnerFrame) {
        match kind {
            Request::Checkpoint => {
                let (id, turn) = match frame {
                    RunnerFrame::Checkpointed { id, checkpoint } => (id, Some(checkpoint.turn)),
                    RunnerFrame::Error { id: Some(id), .. } => (id, None),
                    _ => return,
                };
                if self
                    .saves
                    .waiting
                    .as_ref()
                    .is_some_and(|(open, _)| open == id)
                {
                    self.saves.waiting = None;
                }
                self.mark_turn(id, turn);
            }
            // Done or refused, the tree may have changed, so the diff on screen is stale.
            Request::Restore => {
                self.on_code_answer(frame);
                self.diff();
            }
            Request::Verify
            | Request::Diff
            | Request::TurnDiff { .. }
            | Request::Stop
            | Request::Land => {}
        }
    }
}
