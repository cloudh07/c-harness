//! First run: the engine check, the repository and keys, then the verify command.

pub mod checks;

use std::time::Duration;

use ch_ui::{Palette, Status};
use gpui_kit::base::SelectableText;
use gpui_kit::{
    Animation, AnimationExt, ClickEvent, Context, Div, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, ease_out_quint, px, rgb,
};

use self::checks::{Checks, Model, needs_recheck, row, rows_status, when_done};
use crate::assets::{Icon, icon, status_glyph};
use crate::preflight::{self, Outcome, Suggestion};
use crate::session_view::OpenSettings;
use crate::settings::Settings;
use crate::widgets::{MONO, Tone, button, card, icon_text_button, muted, tip};

const STEP_FADE: Duration = Duration::from_millis(200);
const STEP_FLOOR: f32 = 0.25;
const GLYPH: f32 = 16.0;

/// The person picked a detected verify command.
pub struct UseVerify(pub String);

/// The person finished or skipped setup.
pub struct SetupDone;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Engine,
    Repo,
    Verify,
}

impl Step {
    const ALL: [Step; 3] = [Step::Engine, Step::Repo, Step::Verify];

    fn number(self) -> usize {
        match self {
            Step::Engine => 1,
            Step::Repo => 2,
            Step::Verify => 3,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Step::Engine => "Engine check",
            Step::Repo => "Repository and keys",
            Step::Verify => "Verify command",
        }
    }

    fn next(self) -> Option<Step> {
        Step::ALL.get(self.number()).copied()
    }

    fn back(self) -> Option<Step> {
        self.number().checked_sub(2).map(|i| Step::ALL[i])
    }
}

pub struct FirstRunView {
    palette: Palette,
    settings: Settings,
    step: Step,
    checks: Checks,
    verify_error: Option<String>,
}

impl EventEmitter<OpenSettings> for FirstRunView {}
impl EventEmitter<UseVerify> for FirstRunView {}
impl EventEmitter<SetupDone> for FirstRunView {}

/// The glyph each step shows in the header.
fn step_status(step: Step, checks: &Checks, settings: &Settings) -> Status {
    match step {
        Step::Engine => rows_status(checks.engine_rows().iter().map(|r| r.outcome.as_ref())),
        Step::Repo if settings.problems().is_empty() => rows_status([checks.model_key()]),
        Step::Repo => Status::NeedsYou,
        Step::Verify if settings.verify.trim().is_empty() => Status::Idle,
        Step::Verify => Status::Done,
    }
}

/// Why Finish is off: a session cannot start until step 2 is clear.
fn finish_blocked(settings: &Settings) -> Option<&'static str> {
    (!settings.problems().is_empty())
        .then_some("Finish turns on once step 2 has nothing left to fix.")
}

impl FirstRunView {
    pub fn new(settings: Settings, palette: Palette, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            palette,
            settings,
            step: Step::Engine,
            checks: Checks::default(),
            verify_error: None,
        };
        view.recheck(cx);
        view
    }

    pub fn set_palette(&mut self, palette: Palette, cx: &mut Context<Self>) {
        self.palette = palette;
        cx.notify();
    }

    pub fn set_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let recheck = needs_recheck(&self.settings, &settings);
        self.settings = settings;
        self.verify_error = None;
        if recheck {
            self.checks.settings_changed();
            self.recheck(cx);
        }
        cx.notify();
    }

    /// Back to the first step with the free checks run again.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Engine;
        self.verify_error = None;
        self.recheck(cx);
    }

    pub fn verify_failed(&mut self, why: String, cx: &mut Context<Self>) {
        self.verify_error = Some(why);
        cx.notify();
    }

    /// Runs the free checks again. Never the model test: that one costs money.
    fn recheck(&mut self, cx: &mut Context<Self>) {
        let round = self.checks.start_round();
        when_done(
            preflight::check_engine(self.settings.clone()),
            cx,
            move |view, result, cx| {
                if view.checks.engine_done(round, result) {
                    cx.notify();
                }
            },
        );
        when_done(
            preflight::check_keychain(self.settings.clone()),
            cx,
            move |view, result, cx| {
                if view.checks.keychain_done(round, result) {
                    cx.notify();
                }
            },
        );
        cx.notify();
    }

    fn test_model(&mut self, cx: &mut Context<Self>) {
        let Some(round) = self.checks.start_model_test() else {
            return;
        };
        when_done(
            preflight::test_model(self.settings.clone()),
            cx,
            move |view, result, cx| {
                view.checks.model_done(round, result);
                cx.notify();
            },
        );
        cx.notify();
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        cx.notify();
    }
    fn header(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let steps = Step::ALL.map(|step| {
            let status = step_status(step, &self.checks, &self.settings);
            let current = step == self.step;
            let label = format!(
                "Step {} of 3: {}{}",
                step.number(),
                step.title(),
                if current { ", current" } else { "" }
            );
            gpui_kit::base::Button::new(SharedString::from(format!("setup-step-{}", step.number())))
                .selected(current)
                .accessibility_label(label)
                .flex_1()
                .flex()
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(if current { p.border_strong } else { p.border }))
                .bg(rgb(if current { p.elevated } else { p.surface }))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(p.hover)))
                .child(status_glyph(p, status, px(GLYPH)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(muted(p, format!("Step {}", step.number())))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(if current {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(rgb(if current { p.text } else { p.text_muted }))
                                .child(step.title()),
                        ),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(step, cx)))
        });
        div().flex().gap_2().children(steps)
    }

    fn engine_step(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let rows = self.checks.engine_rows();
        let checking = self.checks.engine.is_none() || self.checks.keychain.is_none();
        let ids = [
            "setup-fix-runner",
            "setup-fix-omp",
            "setup-fix-git",
            "setup-fix-keychain",
        ];
        let rows = rows
            .iter()
            .zip(ids)
            .map(|(r, id)| row(p, id, r.name, r.outcome.as_ref()));
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(card(p).gap_0().children(rows).child(div().pt_2().child(
                button(p, "setup-recheck", "Check again", Tone::Plain, !checking).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.recheck(cx);
                    }),
                ),
            )))
            .child(self.model_card(cx))
    }

    fn model_card(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let untested = preflight::model_test_untested();
        let (outcome, label) = match &self.checks.model {
            Model::Untested => (Some(&untested), "Test the model"),
            Model::Testing => (None, "Testing\u{2026}"),
            Model::Done(outcome) => (Some(outcome), "Test again"),
        };
        let testing = matches!(self.checks.model, Model::Testing);
        let ready = self.checks.runner_ready();
        let test = icon_text_button(
            p,
            "setup-test-model",
            Icon::Sparkles,
            label,
            Tone::Plain,
            ready && !testing,
        )
        .accessibility_label(format!("{label}. Costs money."));
        let test = if ready {
            test.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.test_model(cx)))
        } else {
            test.tooltip(tip(p, "Fix the ch-runner row first."))
        };
        card(p)
            .gap_0()
            .child(row(p, "setup-fix-model", preflight::MODEL_TEST, outcome))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pt_2()
                    .child(muted(p, preflight::model_test_cost(&self.settings)))
                    .child(div().child(test)),
            )
    }

    fn repo_step(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let problems = self.settings.problems();
        let summary = if problems.is_empty() {
            row(
                p,
                "setup-fix-settings",
                "Settings",
                Some(&Outcome::Pass(
                    "The repository, runner and model are filled in.".to_owned(),
                )),
            )
        } else {
            div()
                .flex()
                .flex_col()
                .children(problems.into_iter().map(|problem| {
                    div()
                        .flex()
                        .gap_3()
                        .items_start()
                        .py_2()
                        .child(div().pt(px(2.0)).child(status_glyph(
                            p,
                            Status::NeedsYou,
                            px(GLYPH),
                        )))
                        .child(div().text_sm().child(problem.message))
                }))
        };
        div().flex().flex_col().gap_4().child(
            card(p)
                .gap_0()
                .child(summary)
                .child(row(
                    p,
                    "setup-fix-key",
                    preflight::MODEL_KEY,
                    self.checks.model_key(),
                ))
                .child(
                    div().pt_2().child(
                        icon_text_button(
                            p,
                            "setup-open-settings",
                            Icon::Settings,
                            "Open Settings",
                            Tone::Plain,
                            true,
                        )
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings))),
                    ),
                ),
        )
    }

    fn verify_step(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let open_settings = || {
            icon_text_button(
                p,
                "setup-open-settings",
                Icon::Settings,
                "Open Settings",
                Tone::Plain,
                true,
            )
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings)))
        };
        let body = match preflight::verify_suggestion(
            &self.settings.verify,
            self.checks.engine.as_ref(),
        ) {
            Suggestion::Checking => row(p, "setup-fix-verify", "Test command", None),
            Suggestion::Unavailable(outcome) => {
                row(p, "setup-fix-verify", "Test command", Some(&outcome))
            }
            Suggestion::Keep(command) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(row(
                    p,
                    "setup-fix-verify",
                    "Test command",
                    Some(&Outcome::Pass(
                        "Verify runs this in the session's checkout.".to_owned(),
                    )),
                ))
                .child(command_text(p, command))
                .child(muted(p, "Change it in Settings, under Verify command.")),
            Suggestion::Offer(commands) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().child(
                    "The repository points to these. Pick one, or type your own in Settings.",
                ))
                .child(div().flex().flex_wrap().gap_2().children(
                    commands.into_iter().enumerate().map(|(i, command)| {
                        let label = format!("Use {command}");
                        button(p, ("setup-use-verify", i), label, Tone::Plain, true).on_click(
                            cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.emit(UseVerify(command.clone()));
                            }),
                        )
                    }),
                ))
                .child(div().child(open_settings())),
            Suggestion::Ask => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().child(
                    "Nothing in the repository names a test command. Type the one you run in \
                     Settings, under Verify command.",
                ))
                .child(div().child(open_settings())),
        };
        card(p)
            .child(body)
            .children(self.verify_error.as_ref().map(|why| {
                div()
                    .flex()
                    .gap_2()
                    .items_start()
                    .child(status_glyph(p, Status::Error, px(GLYPH)))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.status(Status::Error)))
                            .child(why.clone()),
                    )
            }))
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let back = self.step.back().map(|step| {
            button(p, "setup-back", "Back", Tone::Plain, true)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(step, cx)))
        });
        let skip = self.step.next().map(|_| {
            button(p, "setup-skip", "Skip setup", Tone::Ghost, true)
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(SetupDone)))
        });
        let blocked = self
            .step
            .next()
            .is_none()
            .then(|| finish_blocked(&self.settings))
            .flatten();
        let forward = match self.step.next() {
            Some(step) => button(p, "setup-next", "Next", Tone::Primary, true)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(step, cx))),
            None if blocked.is_some() => {
                button(p, "setup-finish", "Finish setup", Tone::Primary, false)
            }
            None => button(p, "setup-finish", "Finish setup", Tone::Primary, true)
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(SetupDone))),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(back)
                    .child(div().flex_1())
                    .children(skip)
                    .child(forward),
            )
            .children(blocked.map(|why| div().flex().justify_end().child(muted(p, why))))
    }
}

fn command_text(p: Palette, command: String) -> Div {
    div()
        .flex()
        .gap_2()
        .items_center()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(p.bg))
        .font_family(MONO)
        .text_sm()
        .child(icon(Icon::SquareTerminal, px(GLYPH - 2.0), p.text_muted))
        .child(SelectableText::new(
            "setup-verify-command",
            SharedString::from(command),
        ))
}

impl Render for FirstRunView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let body = match self.step {
            Step::Engine => self.engine_step(cx),
            Step::Repo => self.repo_step(cx),
            Step::Verify => self.verify_step(cx),
        };
        let body = body.with_animation(
            SharedString::from(format!("first-run-step-{}", self.step.number())),
            Animation::new(STEP_FADE).with_easing(ease_out_quint()),
            |body, delta| body.opacity(STEP_FLOOR + (1.0 - STEP_FLOOR) * delta),
        );
        div()
            .id("first-run")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(p.bg))
            .text_color(rgb(p.text))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .w_full()
                    .max_w(px(640.0))
                    .mx_auto()
                    .p_6()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Set up C-Harness"),
                    )
                    .child(muted(
                        p,
                        "Three steps before the first session. Skip any of them and come back \
                         from Settings.",
                    ))
                    .child(self.header(cx))
                    .child(body)
                    .child(self.footer(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preflight::Check;

    fn filled_in() -> Settings {
        Settings {
            repo: "D:/work/app".into(),
            ..Settings::default()
        }
    }

    fn keychain(model_key: Outcome) -> [Check; 2] {
        [
            Check {
                name: preflight::KEYCHAIN,
                outcome: Outcome::Pass("ok".into()),
            },
            Check {
                name: preflight::MODEL_KEY,
                outcome: model_key,
            },
        ]
    }

    #[test]
    fn step_two_needs_you_until_its_fields_are_filled_in() {
        let mut checks = Checks::default();
        assert_eq!(
            step_status(Step::Repo, &checks, &Settings::default()),
            Status::NeedsYou
        );
        // Filled in, with the key row still checking.
        assert_eq!(
            step_status(Step::Repo, &checks, &filled_in()),
            Status::Running
        );

        let round = checks.start_round();
        checks.keychain_done(round, Ok(keychain(Outcome::Pass("found".into()))));
        assert_eq!(step_status(Step::Repo, &checks, &filled_in()), Status::Done);
        assert_eq!(
            step_status(Step::Repo, &checks, &Settings::default()),
            Status::NeedsYou
        );
        // The engine row is still out, so step 1 keeps spinning.
        assert_eq!(
            step_status(Step::Engine, &checks, &filled_in()),
            Status::Running
        );
    }

    #[test]
    fn step_three_is_done_once_a_verify_command_is_set() {
        let checks = Checks::default();
        let blank = Settings {
            verify: "   ".into(),
            ..filled_in()
        };
        let verify = Settings {
            verify: "cargo test".into(),
            ..filled_in()
        };
        assert_eq!(step_status(Step::Verify, &checks, &blank), Status::Idle);
        assert_eq!(step_status(Step::Verify, &checks, &verify), Status::Done);
    }

    #[test]
    fn finish_stays_off_while_step_two_has_problems() {
        assert!(finish_blocked(&Settings::default()).is_some());
        assert_eq!(finish_blocked(&filled_in()), None);
    }

    #[test]
    fn steps_walk_forward_and_back_in_order() {
        assert_eq!(Step::Engine.next(), Some(Step::Repo));
        assert_eq!(Step::Repo.next(), Some(Step::Verify));
        assert_eq!(Step::Verify.next(), None);
        assert_eq!(Step::Engine.back(), None);
        assert_eq!(Step::Verify.back(), Some(Step::Repo));
    }
}
