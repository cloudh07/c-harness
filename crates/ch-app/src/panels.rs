//! Read-only parts of the session screen. Each returns `None` when it has nothing to show.

use ch_core::{Diff, Failure, Landing, Notice, ToolCall, ToolStatus};
use ch_protocol::runner::Evidence;
use ch_ui::{Palette, Status};
use gpui_kit::{App, ClickEvent, Div, FontWeight, ParentElement, Styled, Window, div, rgb};

use crate::view_model::{check_outcome, check_passed, first_chars, last_chars, pr_link_allowed};
use crate::widgets::{MONO, Tone, banner, button, card, mono_block, muted};

/// Enough to read a diff or a long check log in the app; the full text stays in the worktree.
pub const SHOWN_CHARS: usize = 20_000;
const SHOWN_TOOL_CALLS: usize = 30;
const SHOWN_LOG_LINES: usize = 50;
const SHORT_COMMIT: usize = 12;

fn title(text: impl Into<gpui_kit::SharedString>) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

pub fn failure(p: Palette, failure: Option<&Failure>) -> Option<Div> {
    let failure = failure?;
    Some(banner(
        p,
        Status::Error.color(),
        format!("Failed during {}", failure.stage),
        failure.message.clone(),
    ))
}

pub fn warnings(p: Palette, warnings: &[Failure]) -> Option<Div> {
    if warnings.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(warnings.iter().map(|w| {
                banner(
                    p,
                    Status::NearCap.color(),
                    format!("Could not {}", w.stage),
                    w.message.clone(),
                )
            })),
    )
}

pub fn notices(p: Palette, notices: &[Notice]) -> Option<Div> {
    if notices.is_empty() {
        return None;
    }
    Some(
        div().flex().flex_col().gap_2().children(
            notices
                .iter()
                .map(|n| banner(p, p.accent, n.kind.clone(), n.message.clone())),
        ),
    )
}

pub fn tool_calls(p: Palette, calls: &[ToolCall]) -> Option<Div> {
    if calls.is_empty() {
        return None;
    }
    let skipped = calls.len().saturating_sub(SHOWN_TOOL_CALLS);
    let rows = calls[skipped..].iter().map(|call| {
        let (label, status) = match call.status {
            ToolStatus::Running => ("running", Status::Running),
            ToolStatus::Succeeded => ("done", Status::Done),
            ToolStatus::Failed => ("failed", Status::Error),
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(div().size_2().rounded_full().bg(rgb(status.color())))
            .child(div().flex_1().font_family(MONO).child(call.name.clone()))
            .child(div().text_xs().text_color(rgb(p.text_muted)).child(label))
    });
    Some(
        card(p)
            .child(title(format!("Tool calls ({})", calls.len())))
            .children(
                (skipped > 0).then(|| muted(p, format!("{skipped} earlier calls not shown."))),
            )
            .children(rows),
    )
}

pub fn evidence(p: Palette, runs: &[Evidence]) -> Option<Div> {
    if runs.is_empty() {
        return None;
    }
    let items = runs.iter().enumerate().map(|(i, run)| {
        let color = if check_passed(run) {
            Status::Done.color()
        } else {
            Status::Error.color()
        };
        let at = run
            .head
            .as_deref()
            .map(|head| format!(" at {}", first_chars(head, SHORT_COMMIT).0))
            .unwrap_or_default();
        let (tail, clipped) = last_chars(&run.output_tail, SHOWN_CHARS);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().font_family(MONO).child(run.command.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(color))
                    .child(format!("{}{at}", check_outcome(run))),
            )
            .children(
                (!tail.is_empty()).then(|| mono_block(p, format!("check-{i}"), tail.to_string())),
            )
            .children(
                (run.truncated || clipped)
                    .then(|| muted(p, "Only the end of the output is shown.")),
            )
    });
    Some(card(p).child(title("Checks")).children(items))
}

pub fn diff(p: Palette, diff: Option<&Diff>) -> Option<Div> {
    let diff = diff?;
    if diff.patch.is_empty() {
        return Some(
            card(p)
                .child(title(format!("Diff against {}", diff.base)))
                .child(muted(p, "No changes yet.")),
        );
    }
    let (patch, clipped) = first_chars(&diff.patch, SHOWN_CHARS);
    Some(
        card(p)
            .child(title(format!("Diff against {}", diff.base)))
            .children(
                (!diff.stat.is_empty()).then(|| mono_block(p, "diff-stat", diff.stat.clone())),
            )
            .child(mono_block(p, "diff-patch", patch.to_string()))
            .children(
                (diff.truncated || clipped)
                    .then(|| muted(p, "The diff is cut short here. The worktree has all of it.")),
            ),
    )
}

pub fn landing(p: Palette, landing: Option<&Landing>, github_repo: &str) -> Option<Div> {
    let landing = landing?;
    let commit = first_chars(&landing.commit, SHORT_COMMIT).0;
    let link = landing.pull_request_url.clone().map(|url| {
        if pr_link_allowed(&url, github_repo) {
            let open = url.clone();
            div().flex().items_center().gap_2().child(
                button(p, "open-pr", "Open pull request", Tone::Primary, true).on_click(
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| cx.open_url(&open),
                ),
            )
            .child(muted(p, url))
        } else {
            div().child(muted(
                p,
                format!("The runner returned {url}, which is not a pull request in {github_repo}, so it is not opened."),
            ))
        }
    });
    Some(
        card(p)
            .child(title("Landed"))
            .child(
                div()
                    .text_sm()
                    .child(format!("Pushed {commit} to {}.", landing.branch)),
            )
            .children(link)
            .children(
                landing
                    .pull_request_url
                    .is_none()
                    .then(|| muted(p, "No pull request was opened.")),
            ),
    )
}

pub fn log<'a>(p: Palette, lines: impl Iterator<Item = &'a str>) -> Option<Div> {
    let lines: Vec<&str> = lines.collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(SHOWN_LOG_LINES);
    Some(card(p).child(title("Runner log")).child(mono_block(
        p,
        "runner-log",
        lines[start..].join("\n"),
    )))
}
