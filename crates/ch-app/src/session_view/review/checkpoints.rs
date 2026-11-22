//! The checkpoints section of the review panel: one per run, each with its diff and a restore.

use ch_core::previous_turn;
use ch_protocol::runner::{Checkpoint, CheckpointKind};
use gpui_kit::{ClickEvent, Context, Div, ParentElement, Styled, div, px};

use super::{ROW_ICON, section};
use crate::assets::{Icon, icon};
use crate::driver;
use crate::session_view::{Live, Phase, SessionView};
use crate::view_model::age_label;
use crate::widgets::{Tone, button, icon_button, muted};

/// Enough to pick a recent one. Older ones stay in the list the runner keeps.
const SHOWN_CHECKPOINTS: usize = 8;
const EMPTY: &str = "None yet. One is saved each time the agent finishes a run.";

impl SessionView {
    /// `None` when the runner keeps no checkpoints for this session.
    pub(super) fn checkpoints_section(&self, live: &Live, cx: &mut Context<Self>) -> Option<Div> {
        let driver = &live.driver;
        let session = driver.session();
        if !session.checkpoints_on() {
            return None;
        }
        let p = self.palette;
        let can_review = driver.can_review();
        let can_restore = driver.can_restore();
        let (shown, hidden) = newest_first(session.checkpoints(), SHOWN_CHECKPOINTS);
        let now = driver::epoch_ms() / 1_000;
        let rows: Vec<Div> = shown
            .into_iter()
            .map(|c| self.checkpoint_row(c, now, can_review, can_restore, cx))
            .collect();
        Some(
            section(p, "Checkpoints")
                .children(busy(driver.restoring(), driver.saving_checkpoint()).map(|t| muted(p, t)))
                .children(session.checkpoints().is_empty().then(|| muted(p, EMPTY)))
                .children(rows)
                .children((hidden > 0).then(|| muted(p, format!("{hidden} older not shown."))))
                .child(self.start_row(can_restore, cx)),
        )
    }

    /// `now` in seconds since the Unix epoch, like `checkpoint.at`.
    fn checkpoint_row(
        &self,
        checkpoint: &Checkpoint,
        now: u64,
        can_review: bool,
        can_restore: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = self.palette;
        let turn = checkpoint.turn;
        let note = format!(
            "{} · {}",
            kind_note(checkpoint.kind),
            age_label(now, checkpoint.at)
        );
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(icon(Icon::GitCommit, px(ROW_ICON), p.text_muted))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_xs().child(format!("Checkpoint {turn}")))
                    .child(muted(p, note)),
            )
            .child(
                icon_button(
                    p,
                    format!("turn-diff-{turn}"),
                    Icon::FileDiff,
                    "Show what changed since the checkpoint before",
                    Tone::Plain,
                    can_review,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.show_turn_diff(turn, cx);
                })),
            )
            .child(
                button(
                    p,
                    format!("restore-{turn}"),
                    "Restore",
                    Tone::Plain,
                    can_restore,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.restore_checkpoint(turn, cx);
                })),
            )
    }

    /// Turn 0: the commit the session started from.
    fn start_row(&self, can_restore: bool, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(icon(Icon::GitBranch, px(ROW_ICON), p.text_muted))
            .child(div().flex_1().min_w_0().text_xs().child("Session start"))
            .child(
                button(p, "restore-0", "Restore", Tone::Plain, can_restore).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.restore_checkpoint(0, cx)),
                ),
            )
    }

    fn restore_checkpoint(&mut self, turn: u32, cx: &mut Context<Self>) {
        if let Phase::Live(live) = &mut self.phase
            && live.driver.can_restore()
            && !live.driver.restore(turn)
        {
            self.message = Some(Err(
                "The runner is gone, so nothing was restored.".to_string()
            ));
        }
        cx.notify();
    }

    fn show_turn_diff(&mut self, turn: u32, cx: &mut Context<Self>) {
        if let Phase::Live(live) = &mut self.phase
            && live.driver.can_review()
        {
            let from = previous_turn(live.driver.session().checkpoints(), turn);
            if !live.driver.diff_range(from, Some(turn)) {
                self.message = Some(Err("The runner is gone, so no diff came back.".to_string()));
            }
        }
        cx.notify();
    }
}

/// The last `max` checkpoints, newest first, and how many older ones are left out.
fn newest_first(checkpoints: &[Checkpoint], max: usize) -> (Vec<&Checkpoint>, usize) {
    let hidden = checkpoints.len().saturating_sub(max);
    (checkpoints[hidden..].iter().rev().collect(), hidden)
}

fn kind_note(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Run => "After a run",
        CheckpointKind::BeforeRestore => "Saved before a restore",
    }
}

fn busy(restoring: bool, saving: bool) -> Option<&'static str> {
    if restoring {
        Some("Restoring. The tree it replaces is saved first, so this can be undone.")
    } else if saving {
        Some("Saving a checkpoint of the last run.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(turn: u32) -> Checkpoint {
        Checkpoint {
            turn,
            commit: format!("{turn:040}"),
            kind: CheckpointKind::Run,
            at: 1,
        }
    }

    #[test]
    fn the_newest_checkpoints_come_first() {
        let list: Vec<Checkpoint> = (1..=10).map(saved).collect();
        let (shown, hidden) = newest_first(&list, 8);
        let turns: Vec<u32> = shown.iter().map(|c| c.turn).collect();
        assert_eq!(turns, [10, 9, 8, 7, 6, 5, 4, 3]);
        assert_eq!(hidden, 2);

        let (shown, hidden) = newest_first(&list[..2], 8);
        assert_eq!(shown.len(), 2);
        assert_eq!(hidden, 0);
        assert_eq!(newest_first(&[], 8), (Vec::new(), 0));
    }

    #[test]
    fn a_restore_outranks_a_save() {
        assert!(busy(true, true).is_some_and(|t| t.starts_with("Restoring")));
        assert!(busy(false, true).is_some_and(|t| t.starts_with("Saving")));
        assert_eq!(busy(false, false), None);
    }

    #[test]
    fn each_kind_has_its_own_note() {
        assert_ne!(
            kind_note(CheckpointKind::Run),
            kind_note(CheckpointKind::BeforeRestore)
        );
    }
}
