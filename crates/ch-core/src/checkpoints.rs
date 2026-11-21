//! Turn checkpoints: when to save one, which diff a turn shows, and what a restore says.
//!
//! The runner saves each checkpoint as a commit in the session's worktree. Turn 0 is the
//! session's base. A restore first saves the tree it replaces, so it can be undone.

use ch_protocol::runner::{Checkpoint, CheckpointKind, MAX_CHECKPOINTS};

use crate::{Notice, SessionState};

/// The notice kind of restore notices. A newer one replaces the older.
pub const RESTORE_NOTICE: &str = "restored";

/// Whether a state change ends a run the agent did, which is when a checkpoint is saved.
/// A stop before any prompt ends no run, so it saves nothing.
pub fn run_ended(before: SessionState, after: SessionState) -> bool {
    matches!(before, SessionState::Running | SessionState::NeedsYou)
        && after == SessionState::NeedsReview
}

/// The checkpoint listed before `turn`, or 0 (the base). A turn's diff starts there.
pub fn previous_turn(checkpoints: &[Checkpoint], turn: u32) -> u32 {
    checkpoints
        .iter()
        .position(|c| c.turn == turn)
        .and_then(|at| at.checked_sub(1))
        .map_or(0, |at| checkpoints[at].turn)
}

/// Adds a saved checkpoint once, keeping the list in turn order and at most
/// [`MAX_CHECKPOINTS`] long.
pub(crate) fn add(checkpoints: &mut Vec<Checkpoint>, checkpoint: &Checkpoint) {
    if checkpoints.iter().any(|c| c.turn == checkpoint.turn) {
        return;
    }
    checkpoints.push(checkpoint.clone());
    checkpoints.sort_by_key(|c| c.turn);
    let extra = checkpoints.len().saturating_sub(MAX_CHECKPOINTS);
    checkpoints.drain(..extra);
}

/// Says where a restore left the tree, replacing an older restore notice.
pub(crate) fn note_restore(notices: &mut Vec<Notice>, checkpoints: &[Checkpoint], turn: u32) {
    let place = if turn == 0 {
        "The working tree is back where the session started.".to_owned()
    } else {
        format!("The working tree is back at checkpoint {turn}.")
    };
    // The restore saved the tree it replaced just before this.
    let undo = checkpoints
        .last()
        .filter(|c| c.kind == CheckpointKind::BeforeRestore)
        .map(|c| format!(" Checkpoint {} holds what it replaced.", c.turn))
        .unwrap_or_default();
    let message = format!("{place}{undo} Run the checks again before you land.");
    notices.retain(|n| n.kind != RESTORE_NOTICE);
    notices.push(Notice {
        kind: RESTORE_NOTICE.to_owned(),
        message,
    });
}
