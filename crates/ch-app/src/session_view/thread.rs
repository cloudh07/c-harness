//! The thread in the middle of the screen: each prompt, the reply it got, and what the agent did
//! along the way.

use ch_core::SessionState;
use ch_ui::{Palette, Status};
use gpui_kit::base::SelectableText;
use gpui_kit::{ClickEvent, Context, Div, FontWeight, ParentElement, Styled, div, px, rgb};

use super::{CONTENT_WIDTH, Live, OpenSettings, Phase, SessionView, list};
use crate::assets::{Icon, icon};
use crate::brand;
use crate::panels;
use crate::view_model::last_chars;
use crate::widgets::{Tone, banner, button, card, muted};

const MARK_SIZE: f32 = 48.0;
const INLINE_ICON: f32 = 14.0;
const INTRO_WIDTH: f32 = 520.0;
const BUBBLE_SHARE: f32 = 0.8;

impl SessionView {
    pub(super) fn body(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        match &self.phase {
            Phase::Empty => self.empty_state(cx),
            Phase::Launching { prompt, .. } => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(bubble(p, prompt))
                .child(working(p, "Checking the runner and creating the worktree.")),
            Phase::Resuming { .. } => div().flex().flex_col().gap_4().child(working(
                p,
                "Starting the runner again in the session's worktree.",
            )),
            Phase::LaunchFailed { prompt, message } => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(bubble(p, prompt))
                .child(banner(
                    p,
                    p.status(Status::Error),
                    "The session could not start",
                    message.clone(),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(p, "retry", "Try again", Tone::Primary, true).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.retry(cx)),
                            ),
                        )
                        .child(
                            button(p, "failed-settings", "Open Settings", Tone::Plain, true)
                                .on_click(
                                    cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings)),
                                ),
                        ),
                ),
            Phase::Live(live) => self.live_thread(live, cx),
        }
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let problems = self.settings.problems();
        let intro = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .text_center()
            .child(brand::mark(p, px(MARK_SIZE)))
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("What should the agent change?"),
            )
            .child(
                div()
                    .max_w(px(INTRO_WIDTH))
                    .text_sm()
                    .text_color(rgb(p.text_muted))
                    .child(
                        "Describe one change below. The agent works on it in a fresh git \
                         worktree, asks before risky tool calls, and you review the diff and \
                         checks before anything is pushed.",
                    ),
            );
        let setup = (!problems.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .child(div().w_full().child(banner(
                    p,
                    p.status(Status::NeedsYou),
                    "Finish Settings first",
                    list(&problems),
                )))
                .child(
                    button(p, "open-settings", "Open Settings", Tone::Primary, true)
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings))),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_6()
            .pt(px(64.0))
            .child(intro)
            .children(setup)
            .children(self.leftovers_card(cx))
    }

    fn live_thread(&self, live: &Live, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let session = live.driver.session();
        let state = session.state();
        let waiting = matches!(state, SessionState::Starting | SessionState::Running)
            && !live.driver.closed();
        let ask_review = state == SessionState::NeedsReview && !self.review_open;
        let landing = if self.review_open {
            None
        } else {
            panels::landing(p, session.landing(), &self.settings.github_repo)
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children((live.hidden > 0).then(|| muted(p, hidden_note(live.hidden))))
            .children(
                live.turns
                    .iter()
                    .enumerate()
                    .map(|(i, (prompt, reply))| turn(p, format!("turn-{i}"), prompt, reply, false)),
            )
            .children(current_turn(p, live, waiting))
            .children(panels::failure(p, session.failure()))
            .children(panels::warnings(p, session.warnings()))
            .children(panels::notices(p, session.notices()))
            .children(self.disconnected().then(|| {
                banner(
                    p,
                    p.status(Status::NearCap),
                    "The runner disconnected",
                    "Its output ended, so nothing more will arrive. Work that has not landed \
                     stays in its worktree. Start a new session to keep going.",
                )
            }))
            .children(panels::tool_calls(p, session.tool_calls()))
            .children(ask_review.then(|| self.review_prompt(cx)))
            .children(panels::evidence(p, session.evidence()))
            .children(panels::diff(p, session.last_diff()))
            .children(landing)
            .children(panels::log(p, live.driver.log()))
    }

    /// Shown in the thread when the agent is done and the review panel is closed, so the next
    /// step is never hidden.
    fn review_prompt(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        card(p)
            .flex_row()
            .items_center()
            .gap_3()
            .child(icon(
                Icon::CircleDot,
                px(INLINE_ICON + 2.0),
                p.status(Status::Review),
            ))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .child("The agent finished. Review the changes and land them."),
            )
            .child(
                button(p, "open-review", "Review changes", Tone::Primary, true)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_review(cx))),
            )
    }
}

/// The turn in progress. A resumed session has none until the first prompt, only a line while
/// the earlier conversation loads.
fn current_turn(p: Palette, live: &Live, waiting: bool) -> Option<Div> {
    let reply = live.driver.session().reply();
    if live.prompt.is_empty() && reply.is_empty() {
        return (live.driver.loading_history() && !live.driver.closed())
            .then(|| working(p, "Loading the earlier conversation."));
    }
    Some(turn(
        p,
        "turn-now".to_string(),
        &live.prompt,
        reply,
        waiting,
    ))
}

fn hidden_note(hidden: usize) -> String {
    if hidden == 1 {
        "1 earlier turn is not shown.".to_string()
    } else {
        format!("{hidden} earlier turns are not shown.")
    }
}

/// One prompt and its reply. A turn read back from omp may have no prompt, e.g. after
/// compaction.
fn turn(p: Palette, id: String, prompt: &str, reply: &str, waiting: bool) -> Div {
    let (shown, clipped) = last_chars(reply, panels::SHOWN_CHARS);
    let body = if !shown.is_empty() {
        div()
            .text_sm()
            .child(SelectableText::new(id, shown.to_string()))
    } else if waiting {
        working(p, "The agent is working.")
    } else {
        muted(p, "No reply.")
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children((!prompt.is_empty()).then(|| bubble(p, prompt)))
        .children(clipped.then(|| muted(p, "Only the end of this reply is shown.")))
        .child(body)
        .children((waiting && !shown.is_empty()).then(|| working(p, "Still working.")))
}

fn bubble(p: Palette, prompt: &str) -> Div {
    div()
        .self_end()
        .max_w(px(CONTENT_WIDTH * BUBBLE_SHARE))
        .px_4()
        .py_2()
        .rounded_xl()
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.elevated))
        .text_sm()
        .child(prompt.to_string())
}

fn working(p: Palette, text: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(rgb(p.text_muted))
        .child(icon(
            Icon::LoaderCircle,
            px(INLINE_ICON),
            p.status(Status::Running),
        ))
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::hidden_note;

    #[test]
    fn the_hidden_note_counts_turns() {
        assert_eq!(hidden_note(1), "1 earlier turn is not shown.");
        assert_eq!(hidden_note(3), "3 earlier turns are not shown.");
    }
}
