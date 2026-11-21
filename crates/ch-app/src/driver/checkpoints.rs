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
    /// A save failed, so this runner is not asked again.
    failed: bool,
}

impl Driver {
    /// Whether a restore can go out now. The runner refuses one while a check, diff or
    /// restore runs, and a save in flight would race it, so every open request blocks it.
    pub fn can_restore(&self) -> bool {
        self.can_review() && self.session.checkpoints_on() && self.open.is_empty()
    }

    /// Puts the working tree back at checkpoint `turn`, 0 being the session's base.
    pub fn restore(&mut self, turn: u32) -> bool {
        if !self.can_restore() {
            return false;
        }
        let id = self.next_id();
        let frame = ClientFrame::Restore {
            id: id.clone(),
            turn,
        };
        self.send_request(id, Request::Restore, &frame, None)
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

    /// A save of the last run is still on its way, so a new prompt waits for it.
    pub fn saving_checkpoint(&self) -> bool {
        self.saves
            .waiting
            .as_ref()
            .is_some_and(|(_, at)| (self.clock)().saturating_sub(*at) < SAVE_WAIT_MS)
    }

    /// A restore is on its way, so the session waits for it.
    pub fn restoring(&self) -> bool {
        self.open.values().any(|kind| *kind == Request::Restore)
    }

    /// Saves a checkpoint when the state change `before` to now ended a run.
    pub(super) fn save_after_run(&mut self, before: SessionState) {
        let ended = run_ended(before, self.session.state());
        if !ended || self.closed || self.saves.failed || !self.session.checkpoints_on() {
            return;
        }
        let id = self.next_id();
        let frame = ClientFrame::Checkpoint { id: id.clone() };
        if self.send_request(id.clone(), Request::Checkpoint, &frame, None) {
            self.saves.waiting = Some((id, (self.clock)()));
        }
    }

    /// Handles the reply that closed a save or a restore.
    pub(super) fn on_checkpoint_answer(&mut self, kind: Request, frame: &RunnerFrame) {
        match kind {
            Request::Checkpoint => {
                let (id, failed) = match frame {
                    RunnerFrame::Checkpointed { id, .. } => (id, false),
                    RunnerFrame::Error { id: Some(id), .. } => (id, true),
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
                self.saves.failed |= failed;
            }
            // Done or refused, the tree may have changed, so the diff on screen is stale.
            Request::Restore => {
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
