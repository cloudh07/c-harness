//! The prompt box at the bottom of the screen, and the approvals waiting just above it.

use ch_core::SessionState;
use gpui_kit::base::{Button, Textarea};
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, div, px, rgb,
};

use super::{OpenSettings, Phase, SessionView, is_working};
use crate::approval_card::approval_card;
use crate::assets::{Icon, icon, provider_logo};
use crate::widgets::{Tone, icon_button, tip};

const APPROVALS_HEIGHT: f32 = 360.0;
const CHIP_ICON: f32 = 14.0;
const CHEVRON: f32 = 12.0;
const SEND_HINT: &str = "Enter sends. Shift+Enter adds a line.";

/// The line under the prompt while a session is live: what the person can do next.
fn live_hint(closed: bool, state: SessionState, can_prompt: bool) -> &'static str {
    if closed {
        "The runner disconnected. Start a new session to keep going."
    } else if state == SessionState::Landed {
        "This session has landed. Start a new session for the next change."
    } else if state == SessionState::NeedsYou {
        "Answer the approval above so the agent can go on."
    } else if is_working(state) {
        "The agent is working. Send a follow-up once it is ready for review."
    } else if can_prompt {
        SEND_HINT
    } else {
        "The agent cannot take a prompt right now."
    }
}

impl SessionView {
    pub(super) fn approvals(&self, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let Phase::Live(live) = &self.phase else {
            return None;
        };
        let pending = live.driver.session().pending_approvals();
        if pending.is_empty() {
            return None;
        }
        let p = self.palette;
        let cards: Vec<_> = pending.iter().map(|r| approval_card(p, r, cx)).collect();
        Some(
            div()
                .id("approvals")
                .flex()
                .flex_col()
                .gap_2()
                .max_h(px(APPROVALS_HEIGHT))
                .overflow_y_scroll()
                .children(cards),
        )
    }

    pub(super) fn composer(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let needs_setup = self.needs_setup();
        let (can_send, hint, can_stop) = match &self.phase {
            Phase::Live(live) => {
                let driver = &live.driver;
                let state = driver.session().state();
                let can_prompt = driver.can_prompt();
                (
                    can_prompt,
                    live_hint(driver.closed(), state, can_prompt),
                    is_working(state) && !driver.stopping() && !driver.closed(),
                )
            }
            Phase::Launching { .. } => (false, "Starting the session.", false),
            Phase::Empty | Phase::LaunchFailed { .. } if needs_setup => {
                (false, "Finish Settings to start a session.", false)
            }
            Phase::Empty | Phase::LaunchFailed { .. } => (true, SEND_HINT, false),
        };
        let action = if can_stop {
            icon_button(
                p,
                "stop",
                Icon::Square,
                "Stop the agent",
                Tone::Danger,
                true,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
        } else {
            let tone = if matches!(self.phase, Phase::Empty) && !needs_setup {
                Tone::Primary
            } else {
                Tone::Plain
            };
            icon_button(p, "send", Icon::ArrowUp, "Send", tone, can_send)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)))
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_xl()
            .border_1()
            .border_color(rgb(p.border_strong))
            .bg(rgb(p.elevated))
            .child(div().px_1().text_sm().child(Textarea::new(&self.prompt)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.model_chip(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(p.text_faint))
                            .truncate()
                            .child(hint),
                    )
                    .child(action),
            )
    }

    /// The provider and model the next session runs with. Clicking it opens Settings, where
    /// both are chosen.
    fn model_chip(&self, cx: &mut Context<Self>) -> Button {
        let p = self.palette;
        let model = self.settings.model.trim();
        let label = if model.is_empty() {
            "Default model".to_string()
        } else {
            model.to_string()
        };
        let logo = provider_logo(&self.settings.provider, px(CHIP_ICON), p.text)
            .unwrap_or_else(|| icon(Icon::Bot, px(CHIP_ICON), p.text_muted));
        Button::new("model-chip")
            .flex_none()
            .gap(px(6.0))
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .text_color(rgb(p.text_muted))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(p.hover)))
            .accessibility_label(format!("Model: {label}"))
            .tooltip(tip(p, "Change the provider and model in Settings"))
            .child(logo)
            .child(label)
            .child(icon(Icon::ChevronDown, px(CHEVRON), p.text_faint))
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_runner_outranks_every_state() {
        for state in [
            SessionState::Running,
            SessionState::NeedsReview,
            SessionState::Landed,
        ] {
            assert!(
                live_hint(true, state, true).contains("disconnected"),
                "{state:?}"
            );
        }
    }

    #[test]
    fn the_hint_names_the_next_step() {
        assert!(live_hint(false, SessionState::NeedsYou, false).contains("approval"));
        assert!(live_hint(false, SessionState::Running, false).contains("working"));
        assert!(live_hint(false, SessionState::Landed, false).contains("landed"));
        assert_eq!(live_hint(false, SessionState::NeedsReview, true), SEND_HINT);
        assert!(live_hint(false, SessionState::Failed, false).contains("cannot"));
    }
}
