//! Read-only parts of the session screen. Each returns `None` when it has nothing to show.

use ch_core::{Diff, Failure, Landing, Notice, ToolCall, ToolStatus, TurnDiff};
use ch_protocol::runner::Evidence;
use ch_ui::{Palette, Status, ToolGroup};
use gpui_kit::{App, ClickEvent, Div, FontWeight, ParentElement, Styled, Window, div, px, rgb};

use crate::assets::{icon, tool_icon};
use crate::view_model::{
    check_outcome, check_passed, first_chars, last_chars, pr_link_allowed, turn_diff_title,
};
use crate::widgets::{MONO, Tone, banner, button, card, mono_block, muted};

/// Enough to read a diff or a long check log in the app; the full text stays in the worktree.
pub const SHOWN_CHARS: usize = 20_000;
const SHOWN_TOOL_CALLS: usize = 30;
const SHOWN_LOG_LINES: usize = 50;
const SHORT_COMMIT: usize = 12;
const TOOL_ICON: f32 = 14.0;

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
        p.status(Status::Error),
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
                    p.status(Status::NearCap),
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
        let group = ToolGroup::of(&call.name);
        let (label, label_color) = match call.status {
            ToolStatus::Running => ("running", p.status(Status::Running)),
            ToolStatus::Succeeded => ("done", p.text_muted),
            ToolStatus::Failed => ("failed", p.status(Status::Error)),
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(icon(tool_icon(group), px(TOOL_ICON), p.tool(group)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .child(call.name.clone()),
            )
            .child(div().text_xs().text_color(rgb(label_color)).child(label))
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
            p.status(Status::Done)
        } else {
            p.status(Status::Error)
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
    let heading = format!("Diff against {}", diff.base);
    Some(diff_card(p, heading, "diff", "No changes yet.", diff))
}

/// The changes between two checkpoints, or from one to the working tree.
pub fn turn_diff(p: Palette, turn: Option<&TurnDiff>) -> Option<Div> {
    let turn = turn?;
    let heading = turn_diff_title(turn.from, turn.to);
    Some(diff_card(
        p,
        heading,
        "turn-diff",
        "No changes.",
        &turn.diff,
    ))
}

/// `id` keeps the two diff cards' scroll blocks apart.
fn diff_card(p: Palette, heading: String, id: &str, empty: &'static str, diff: &Diff) -> Div {
    let card = card(p).child(title(heading));
    if diff.patch.is_empty() {
        return card.child(muted(p, empty));
    }
    let (patch, clipped) = first_chars(&diff.patch, SHOWN_CHARS);
    card.children(
        (!diff.stat.is_empty()).then(|| mono_block(p, format!("{id}-stat"), diff.stat.clone())),
    )
    .child(mono_block(p, format!("{id}-patch"), patch.to_string()))
    .children(
        (diff.truncated || clipped)
            .then(|| muted(p, "The diff is cut short here. The worktree has all of it.")),
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
