//! "Find the test command" under the verify field. The runner reads the repository's files and
//! names the test commands they point to; picking one fills the empty field, and Save keeps it.

use gpui_kit::base::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    ClickEvent, Context, Div, ParentElement, StatefulInteractiveElement, Styled, Window, div,
};

use super::{SettingsView, value};
use crate::assets::Icon;
use crate::first_run::{row, when_done};
use crate::preflight::{self, Engine, Suggestion};
use crate::settings::Settings;
use crate::widgets::{Tone, button, field, icon_text_button, muted, tip};

pub(super) enum Detect {
    Idle,
    Looking,
    Done(Engine),
}

impl SettingsView {
    /// Drops an answer, or one still on its way, once the place it was read from changes.
    pub(super) fn forget_detected(&mut self) {
        self.detect_round += 1;
        self.detect = Detect::Idle;
    }

    fn find_verify(&mut self, cx: &mut Context<Self>) {
        let Ok(draft) = self.draft(cx) else {
            return;
        };
        self.forget_detected();
        self.detect = Detect::Looking;
        let round = self.detect_round;
        when_done(
            preflight::check_engine(draft),
            cx,
            move |view, result, cx| {
                if view.detect_round == round {
                    view.detect = Detect::Done(result.unwrap_or_else(|why| Engine::unknown(&why)));
                    cx.notify();
                }
            },
        );
        cx.notify();
    }

    /// Fills the field only while it is empty: a typed command is never replaced.
    fn use_detected(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        if value(&self.fields.verify, cx).is_empty() {
            self.fields
                .verify
                .update(cx, |state, cx| state.set_value(command, window, cx));
        }
    }

    pub(super) fn verify_field(
        &self,
        draft: &Result<Settings, String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let hint =
            "Runs in the session's checkout when you press Verify. Leave empty to hide the button.";
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field(
                self.palette,
                "Verify command",
                Some(hint),
                Input::new(&self.fields.verify),
            ))
            .child(self.verify_finder(draft, cx))
    }

    fn verify_finder(&self, draft: &Result<Settings, String>, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let ready = draft.as_ref().is_ok_and(preflight::repo_chosen);
        let looking = matches!(self.detect, Detect::Looking);
        let label = if looking {
            "Looking\u{2026}"
        } else {
            "Find the test command"
        };
        let find = icon_text_button(
            p,
            "find-verify",
            Icon::SquareTerminal,
            label,
            Tone::Plain,
            ready && !looking,
        )
        .when(!ready, |b| b.tooltip(tip(p, "Pick the repository first.")))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.find_verify(cx)));
        let answer = match &self.detect {
            Detect::Done(engine) => {
                let typed = value(&self.fields.verify, cx);
                self.detected(preflight::verify_suggestion(&typed, Some(engine)), cx)
            }
            Detect::Idle | Detect::Looking => None,
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().child(find))
            .children(answer)
    }

    fn detected(&self, suggestion: Suggestion, cx: &mut Context<Self>) -> Option<Div> {
        let p = self.palette;
        match suggestion {
            // A typed command wins, and nothing is offered over it.
            Suggestion::Keep(_) | Suggestion::Checking => None,
            Suggestion::Unavailable(outcome) => {
                Some(row(p, "find-verify-fix", "Test command", Some(&outcome)))
            }
            Suggestion::Ask => Some(muted(
                p,
                "Nothing in the repository names a test command. Type the one you run.",
            )),
            Suggestion::Offer(commands) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(muted(
                        p,
                        "The repository points to these. Save keeps the one you pick.",
                    ))
                    .child(div().flex().flex_wrap().gap_2().children(
                        commands.into_iter().enumerate().map(|(i, command)| {
                            button(
                                p,
                                ("use-verify", i),
                                format!("Use {command}"),
                                Tone::Plain,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.use_detected(command.clone(), window, cx);
                                },
                            ))
                        }),
                    )),
            ),
        }
    }
}
