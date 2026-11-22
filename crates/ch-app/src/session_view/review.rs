//! The review panel on the right: what changed, whether the checks pass, and the land.

mod checklist;
mod checkpoints;

use std::time::Duration;

use ch_core::{Diff, Landing};
use ch_protocol::runner::SecretScan;
use ch_ui::{Palette, Status};
use gpui_kit::{
    Animation, AnimationElement, AnimationExt, App, ClickEvent, Context, Div, InteractiveElement,
    IntoElement, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, Window,
    div, ease_out_quint, px, rgb,
};

use super::{Live, OpenSettings, SessionView};
use crate::assets::{Icon, icon};
use crate::view_model::{
    LAND_STAGE, branch_for, check_outcome, check_passed, checklist, diff_files, diff_totals,
    elide_middle, first_chars, pr_link_allowed, secret_rows,
};
use crate::widgets::{MONO, Tone, button, eyebrow, icon_text_button, muted};

const PANEL_WIDTH: f32 = 288.0;
const SHOWN_FILES: usize = 12;
const SHOWN_CHECKS: usize = 6;
const SHOWN_SECRETS: usize = 6;
/// Characters of `path:line` a finding row shows before it drops the middle, so the line number
/// stays in view.
const PLACE_LIMIT: usize = 34;
/// What a land changes fades in, so the change in the panel reads as the answer to the click.
/// GPUI shows the end state at once when reduce motion is set.
const FADE: Duration = Duration::from_millis(200);
/// Faded parts start this visible, so reading them never waits on the fade.
const FADE_FLOOR: f32 = 0.25;
const SHORT_COMMIT: usize = 12;
const ROW_ICON: f32 = 14.0;

impl SessionView {
    pub(super) fn review_panel(&self, live: &Live, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = self.palette;
        div()
            .id("review-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(PANEL_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .bg(rgb(p.surface))
            .border_l_1()
            .border_color(rgb(p.border))
            .child(self.changes(live, cx))
            .children(self.checkpoints_section(live, cx))
            .child(self.checks(live, cx))
            .child(self.land_section(live, cx))
            .child(session_details(p, live))
    }

    fn changes(&self, live: &Live, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let diff = live.driver.session().last_diff();
        let label = if diff.is_some() {
            "Refresh diff"
        } else {
            "Show diff"
        };
        section(p, "Changes")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(icon(Icon::GitBranch, px(ROW_ICON), p.text_muted))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .child(branch_for(&live.session)),
                    ),
            )
            .child(diff_summary(p, diff))
            .child(start(
                icon_text_button(
                    p,
                    "diff",
                    Icon::FileDiff,
                    label,
                    Tone::Plain,
                    live.driver.can_review(),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.diff(cx))),
            ))
    }

    fn checks(&self, live: &Live, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let runs = live.driver.session().evidence();
        let has_check = !self.settings.verify.trim().is_empty();
        let skipped = runs.len().saturating_sub(SHOWN_CHECKS);
        let rows = runs[skipped..].iter().rev().map(|run| {
            let (glyph, status) = if check_passed(run) {
                (Icon::CircleCheck, Status::Done)
            } else {
                (Icon::CircleX, Status::Error)
            };
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(icon(glyph, px(ROW_ICON), p.status(status)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_family(MONO)
                                .truncate()
                                .child(run.command.clone()),
                        )
                        .child(muted(p, check_outcome(run))),
                )
        });
        let empty = if has_check {
            "No checks have run yet."
        } else {
            "No check command is set."
        };
        let action = if has_check {
            icon_text_button(
                p,
                "verify",
                Icon::ListChecks,
                "Run checks",
                Tone::Plain,
                live.driver.can_review(),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.verify(cx)))
        } else {
            icon_text_button(p, "add-check", Icon::Plus, "Add a check", Tone::Plain, true)
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings)))
        };
        section(p, "Checks")
            .children(runs.is_empty().then(|| muted(p, empty)))
            .children(rows)
            .child(start(action))
    }

    fn land_section(&self, live: &Live, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let driver = &live.driver;
        let section = section(p, "Land");
        if let Some(landing) = driver.session().landing() {
            return section.child(landed(p, landing, &self.settings.github_repo));
        }
        let session = driver.session();
        let list = checklist(
            &self.settings,
            &live.session,
            &live.first_prompt,
            session.evidence(),
            session.failure(),
            driver.runner_block(),
        );
        let can_land = driver.can_land() && list.blocked().is_none();
        let refused = session
            .failure()
            .filter(|failure| failure.stage == LAND_STAGE)
            .and_then(|failure| failure.secret_scan.as_ref());
        let busy = if driver.landing() {
            Some("Landing. The agent is stopped first, then the runner commits and pushes.")
        } else if driver.stopping() {
            Some("Stopping the agent.")
        } else if !driver.closed() && !driver.can_land() && driver.runner_block().is_none() {
            // The checklist can pass while the agent still works, so say why Land waits.
            Some("Land is off until the session is ready for review.")
        } else {
            None
        };
        let settings = (!self.settings.land_problems().is_empty()).then(|| {
            start(
                button(p, "land-settings", "Open Settings", Tone::Plain, true)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings))),
            )
        });
        section
            .child(checklist::checklist_rows(p, &list))
            .children(settings)
            .children(refused.map(|scan| self.secret_findings(scan, can_land, cx)))
            .child(checklist::pr_preview(p, &list))
            .child(
                icon_text_button(
                    p,
                    "land",
                    Icon::GitPullRequest,
                    "Land and open PR",
                    Tone::Primary,
                    can_land,
                )
                .w_full()
                .justify_center()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.land(cx))),
            )
            .children(busy.map(|text| muted(p, text)))
    }

    /// What the secret scan found in a refused land: where each hit is and what it looks like,
    /// never the value, and the way past it for a change the person has checked.
    fn secret_findings(
        &self,
        scan: &SecretScan,
        can_land: bool,
        cx: &mut Context<Self>,
    ) -> AnimationElement<Div> {
        let p = self.palette;
        let (rows, hidden) = secret_rows(scan, SHOWN_SECRETS);
        let summary = if scan.too_large {
            "The change is too large to scan for secrets, so nothing was landed."
        } else {
            "These added lines look like secrets, so nothing was landed."
        };
        let rows = rows.into_iter().map(|row| {
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(icon(
                    Icon::CircleAlert,
                    px(ROW_ICON),
                    p.status(Status::NeedsYou),
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_family(MONO)
                                .truncate()
                                .child(elide_middle(&row.place, PLACE_LIMIT)),
                        )
                        .child(muted(p, row.kind)),
                )
        });
        let tree = scan.tree.clone();
        let land_anyway = icon_text_button(
            p,
            "land-anyway",
            Icon::ShieldAlert,
            "Land anyway",
            Tone::Danger,
            can_land,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.land_anyway(tree.clone(), cx);
        }));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(muted(p, summary))
            .children(rows)
            .children((hidden > 0).then(|| muted(p, format!("{hidden} more not listed."))))
            .child(start(land_anyway))
            .child(muted(
                p,
                "Lands this exact change. Edit it and the scan runs again.",
            ))
            .with_animation(
                SharedString::from(format!("secret-findings-{}", scan.tree)),
                Animation::new(FADE).with_easing(ease_out_quint()),
                |findings, delta| findings.opacity(FADE_FLOOR + (1.0 - FADE_FLOOR) * delta),
            )
    }
}

fn section(p: Palette, title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .border_b_1()
        .border_color(rgb(p.border))
        .child(eyebrow(p, title))
}

/// Keeps a button at its own width inside a column that would stretch it.
fn start(element: impl IntoElement) -> Div {
    div().flex().child(element)
}

fn diff_summary(p: Palette, diff: Option<&Diff>) -> Div {
    let Some(diff) = diff else {
        return muted(p, "No diff yet.");
    };
    if diff.patch.is_empty() {
        return muted(p, format!("No changes against {}.", diff.base));
    }
    let files = diff_files(&diff.stat);
    let hidden = files.len().saturating_sub(SHOWN_FILES);
    let totals = diff_totals(&diff.stat).map(|t| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(format!(
                "{} {}",
                t.files,
                if t.files == 1 { "file" } else { "files" }
            ))
            .child(
                div()
                    .text_color(rgb(p.status(Status::Done)))
                    .child(format!("+{}", t.insertions)),
            )
            .child(
                div()
                    .text_color(rgb(p.status(Status::Error)))
                    .child(format!("−{}", t.deletions)),
            )
    });
    let rows = files.into_iter().take(SHOWN_FILES).map(|(path, change)| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .child(path.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(p.text_faint))
                    .child(change.to_string()),
            )
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(totals)
        .children(rows)
        .children((hidden > 0).then(|| muted(p, format!("{hidden} more in the full diff."))))
        .children(
            diff.truncated
                .then(|| muted(p, "The diff is cut short. The worktree has all of it.")),
        )
}

fn landed(p: Palette, landing: &Landing, github_repo: &str) -> Div {
    let commit = first_chars(&landing.commit, SHORT_COMMIT).0;
    let link = match landing.pull_request_url.clone() {
        Some(url) if pr_link_allowed(&url, github_repo) => {
            let open = url.clone();
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_1()
                .child(
                    icon_text_button(
                        p,
                        "open-pr",
                        Icon::ExternalLink,
                        "Open pull request",
                        Tone::Primary,
                        true,
                    )
                    .on_click(
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            cx.open_url(&open);
                        },
                    ),
                )
                .child(muted(p, url).w_full().truncate())
        }
        Some(url) => muted(
            p,
            format!(
                "The runner returned {url}, which is not a pull request in {github_repo}, so \
                 it is not opened."
            ),
        ),
        None => muted(p, "No pull request was opened."),
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
                .text_sm()
                .child(icon(Icon::GitCommit, px(ROW_ICON), p.status(Status::Done)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(format!("Pushed {commit} to {}", landing.branch)),
                ),
        )
        .child(link)
}

fn session_details(p: Palette, live: &Live) -> Div {
    let item = |label: &'static str, value: String| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(muted(p, label))
            .child(div().text_xs().font_family(MONO).child(value))
    };
    section(p, "Session")
        .child(item("ID", live.session.clone()))
        .children((!live.version.is_empty()).then(|| item("Runner", live.version.clone())))
        .children(
            live.worktree
                .as_ref()
                .map(|path| item("Worktree", path.display().to_string())),
        )
}
