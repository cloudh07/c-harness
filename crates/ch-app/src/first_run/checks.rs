//! The check rows the first run and Settings both show, and the poll that fills them.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use ch_ui::{Palette, Status};
use gpui_kit::base::SelectableText;
use gpui_kit::{Context, Div, FontWeight, ParentElement, SharedString, Styled, div, px, rgb};

use super::GLYPH;
use crate::assets::status_glyph;
use crate::preflight::{self, Check, Engine, Outcome};
use crate::settings::Settings;
use crate::widgets::muted;

const TICK: Duration = Duration::from_millis(50);

/// One row of the engine check. `None` while it runs.
#[derive(Debug, PartialEq)]
pub(super) struct Row {
    pub(super) name: &'static str,
    pub(super) outcome: Option<Outcome>,
}

pub(super) fn engine_rows(engine: Option<&Engine>, keychain: Option<&[Check; 2]>) -> Vec<Row> {
    let runner = match engine {
        Some(engine) => engine
            .checks
            .iter()
            .map(|c| Row {
                name: c.name,
                outcome: Some(c.outcome.clone()),
            })
            .collect(),
        None => [preflight::RUNNER, preflight::OMP, preflight::GIT]
            .into_iter()
            .map(|name| Row {
                name,
                outcome: None,
            })
            .collect(),
    };
    let keychain = Row {
        name: preflight::KEYCHAIN,
        outcome: keychain.map(|k| k[0].outcome.clone()),
    };
    let mut rows: Vec<Row> = runner;
    rows.push(keychain);
    rows
}

fn state_word(outcome: Option<&Outcome>) -> &'static str {
    match outcome {
        None => "Checking",
        Some(Outcome::Pass(_)) => "Passed",
        Some(Outcome::Fail { .. }) => "Failed",
        Some(Outcome::Unknown(_)) => "Not checked",
    }
}

fn outcome_status(outcome: Option<&Outcome>) -> Status {
    match outcome {
        None => Status::Running,
        Some(Outcome::Pass(_)) => Status::Done,
        Some(Outcome::Fail { .. }) => Status::Error,
        Some(Outcome::Unknown(_)) => Status::Idle,
    }
}

/// The worst of the rows, for the step header.
pub(super) fn rows_status<'a>(outcomes: impl IntoIterator<Item = Option<&'a Outcome>>) -> Status {
    let mut status = Status::Done;
    for outcome in outcomes {
        match outcome {
            Some(Outcome::Fail { .. }) => return Status::Error,
            None => status = Status::Running,
            Some(Outcome::Unknown(_)) if status == Status::Done => status = Status::Idle,
            Some(_) => {}
        }
    }
    status
}

/// Only a change that can move a check's result starts the checks again. A save with nothing
/// changed may have stored a new key, so it counts.
pub(super) fn needs_recheck(old: &Settings, new: &Settings) -> bool {
    let ignoring_verify_and_theme = Settings {
        verify: old.verify.clone(),
        theme: old.theme,
        ..new.clone()
    };
    old == new || ignoring_verify_and_theme != *old
}

pub(super) fn unknown_keychain(why: &str) -> [Check; 2] {
    [preflight::KEYCHAIN, preflight::MODEL_KEY].map(|name| Check {
        name,
        outcome: Outcome::unknown(why),
    })
}

/// Polls `rx` on the UI thread and hands the result to `done` once.
pub fn when_done<V: 'static, T: 'static>(
    rx: Receiver<Result<T, String>>,
    cx: &mut Context<V>,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Context<V>) + 'static,
) {
    cx.spawn(async move |this, cx| {
        let result = loop {
            match rx.try_recv() {
                Ok(result) => break result,
                Err(TryRecvError::Disconnected) => {
                    break Err("The check stopped without saying why.".to_owned());
                }
                Err(TryRecvError::Empty) => cx.background_executor().timer(TICK).await,
            }
        };
        this.update(cx, |view, cx| done(view, result, cx)).ok();
    })
    .detach();
}

pub fn row(p: Palette, id: &'static str, name: &'static str, outcome: Option<&Outcome>) -> Div {
    let word = state_word(outcome);
    let (detail, fix) = match outcome {
        None => ("Checking\u{2026}", None),
        Some(Outcome::Pass(detail) | Outcome::Unknown(detail)) => (detail.as_str(), None),
        Some(Outcome::Fail { why, fix }) => (why.as_str(), Some(fix.clone())),
    };
    div()
        .flex()
        .gap_3()
        .items_start()
        .py_2()
        .child(
            div()
                .pt(px(2.0))
                .child(status_glyph(p, outcome_status(outcome), px(GLYPH))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_baseline()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                        .child(muted(p, word)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(p.text_muted))
                        .child(detail.to_owned()),
                )
                .children(fix.map(|fix| {
                    div()
                        .text_sm()
                        .text_color(rgb(p.text))
                        .child(SelectableText::new(id, SharedString::from(fix)))
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &'static str, outcome: Outcome) -> Check {
        Check { name, outcome }
    }

    fn pass() -> Outcome {
        Outcome::Pass("ok".into())
    }

    #[test]
    fn rows_say_checking_until_each_check_answers() {
        let rows = engine_rows(None, None);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r.outcome.is_none()));
        assert_eq!(rows[3].name, preflight::KEYCHAIN);

        let keychain = [
            check(preflight::KEYCHAIN, pass()),
            check(preflight::MODEL_KEY, pass()),
        ];
        let rows = engine_rows(None, Some(&keychain));
        assert_eq!(rows[3].outcome, Some(pass()));
        assert_eq!(
            rows_status(rows.iter().map(|r| r.outcome.as_ref())),
            Status::Running
        );
    }

    #[test]
    fn one_failed_row_marks_the_step_failed() {
        let fail = Outcome::Fail {
            why: "no git".into(),
            fix: "install git".into(),
        };
        let unknown = Outcome::unknown("no repo");
        assert_eq!(
            rows_status([Some(&pass()), None, Some(&fail)]),
            Status::Error
        );
        assert_eq!(rows_status([Some(&pass()), Some(&unknown)]), Status::Idle);
        assert_eq!(rows_status([Some(&unknown), None]), Status::Running);
        assert_eq!(rows_status([Some(&pass())]), Status::Done);
        assert_eq!(state_word(Some(&fail)), "Failed");
        assert_eq!(state_word(None), "Checking");
    }

    #[test]
    fn only_changes_that_move_a_check_run_it_again() {
        let old = Settings {
            repo: "D:/work/app".into(),
            ..Settings::default()
        };
        let verify = Settings {
            verify: "cargo test".into(),
            ..old.clone()
        };
        assert!(!needs_recheck(&old, &verify));
        let repo = Settings {
            repo: "D:/work/other".into(),
            ..old.clone()
        };
        assert!(needs_recheck(&old, &repo));
        // Saved with no visible change: a new key may have gone into the keychain.
        assert!(needs_recheck(&old, &old.clone()));
    }

    #[test]
    fn a_failed_keychain_job_leaves_both_rows_unknown() {
        let rows = unknown_keychain("stopped");
        assert_eq!(rows[0].name, preflight::KEYCHAIN);
        assert_eq!(rows[1].name, preflight::MODEL_KEY);
        assert!(
            rows.iter()
                .all(|r| r.outcome == Outcome::unknown("stopped"))
        );
    }
}
