//! The bar above the thread: what the session is, where it works, and its own actions.

use std::path::Path;

use ch_ui::Status;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    ClickEvent, Context, Div, FontWeight, ParentElement, SharedString, Styled, div, px, rgb,
};

use super::{Phase, SessionView, status_for};
use crate::assets::{Icon, icon, status_icon};
use crate::view_model::{
    Place, Sandbox, branch_for, context_label, context_near_full, elide_middle, live_title,
    session_title, state_label, usage_label,
};
use crate::widgets::{MONO, Tone, icon_button};

const STATUS_ICON: f32 = 16.0;
const META_ICON: f32 = 12.0;
/// Characters of the worktree path the header shows before it drops the middle.
const PATH_LIMIT: usize = 64;

impl SessionView {
    pub(super) fn header(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let state = self.state();
        let (glyph, color, label) = if self.disconnected() {
            (Icon::Plug, p.status(Status::NearCap), "Disconnected")
        } else {
            let status = state.map_or(Status::Idle, status_for);
            (
                status_icon(status),
                p.status(status),
                state.map_or("Not started", state_label),
            )
        };
        let (title, branch) = match &self.phase {
            Phase::Empty => ("New session".to_string(), None),
            Phase::Launching {
                session, prompt, ..
            } => (session_title(prompt), Some(branch_for(session))),
            Phase::Resuming { session, .. } => {
                ("Resuming a session".to_string(), Some(branch_for(session)))
            }
            Phase::LaunchFailed { prompt, .. } => (session_title(prompt), None),
            Phase::Live(live) => (
                live_title(&live.first_prompt),
                Some(branch_for(&live.session)),
            ),
        };
        let live = matches!(self.phase, Phase::Live(_));
        let (place, worktree) = self.place();
        let meta = self.meta(color, label, &place.repo, branch);
        let where_row = self.where_row(&place, worktree);

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(rgb(p.border))
            .child(icon(glyph, px(STATUS_ICON), color))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(title),
                    )
                    .child(meta)
                    .child(where_row),
            )
            .when(live, |bar| {
                bar.child(
                    icon_button(
                        p,
                        "export",
                        Icon::Download,
                        "Export transcript",
                        Tone::Ghost,
                        true,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.export(cx))),
                )
                .child(
                    icon_button(
                        p,
                        "toggle-review",
                        Icon::PanelRight,
                        "Toggle review panel",
                        Tone::Ghost,
                        true,
                    )
                    .when(self.review_open, |b| b.bg(rgb(p.hover)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_review(cx))),
                )
            })
    }
}

impl SessionView {
    /// Where the session works. A session that started keeps the place it started in; before
    /// that, the header shows where the next one would run.
    fn place(&self) -> (Place, Option<&Path>) {
        match &self.phase {
            Phase::Launching { place, .. } | Phase::Resuming { place, .. } => (place.clone(), None),
            Phase::Live(live) => (live.place.clone(), live.worktree.as_deref()),
            Phase::Empty | Phase::LaunchFailed { .. } => (Place::of(&self.settings), None),
        }
    }

    /// The line under the title: state, repository, branch and, once a turn ends, usage and
    /// context, the context drawn as a warning when the window is nearly full.
    fn meta(&self, color: u32, label: &'static str, repo: &str, branch: Option<String>) -> Div {
        let p = self.palette;
        let (usage, context, near_full) = match &self.phase {
            Phase::Live(live) => (
                usage_label(&live.driver.usage().totals()),
                context_label(live.driver.context()),
                context_near_full(live.driver.context()),
            ),
            _ => (None, None, false),
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_xs()
            .text_color(rgb(p.text_muted))
            .child(div().flex_none().text_color(rgb(color)).child(label))
            .when(!repo.is_empty(), |row| {
                row.child(dot(p.text_faint)).child(meta_item(
                    p,
                    Icon::FolderGit,
                    repo.to_string(),
                    None,
                ))
            })
            .children(branch.map(|branch| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(dot(p.text_faint))
                    .child(meta_item(p, Icon::GitBranch, branch, Some(MONO)))
            }))
            .when_some(usage, |row, usage| {
                row.child(dot(p.text_faint))
                    .child(meta_item(p, Icon::Sparkles, usage, None))
            })
            .when_some(context, |row, context| {
                row.child(dot(p.text_faint)).child(
                    meta_item(p, Icon::Gauge, context, None).when(near_full, |item| {
                        item.text_color(rgb(p.status(Status::NearCap)))
                    }),
                )
            })
    }

    /// The second line: this computer or the SSH host, then the folder the agent writes in. The
    /// path keeps its start and its end, where the session's own folder is.
    fn where_row(&self, place: &Place, worktree: Option<&Path>) -> Div {
        let p = self.palette;
        let glyph = match place.sandbox {
            Sandbox::Local => Icon::Monitor,
            Sandbox::Ssh { .. } => Icon::Globe,
        };
        let sandbox = place.sandbox_label();
        let has_sandbox = !sandbox.is_empty();
        let folder = place.folder(worktree);
        div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_xs()
            .text_color(rgb(p.text_muted))
            .when(has_sandbox, |row| {
                row.child(div().flex_none().child(meta_item(p, glyph, sandbox, None)))
            })
            .when_some(folder, |row, folder| {
                row.when(has_sandbox, |row| row.child(dot(p.text_faint)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .child(elide_middle(&folder, PATH_LIMIT)),
                    )
            })
    }
}

fn dot(color: u32) -> Div {
    div().flex_none().text_color(rgb(color)).child("·")
}

fn meta_item(
    p: ch_ui::Palette,
    glyph: Icon,
    text: impl Into<SharedString>,
    font: Option<&'static str>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .min_w_0()
        .child(icon(glyph, px(META_ICON), p.text_faint))
        .child(
            div()
                .truncate()
                .when_some(font, Styled::font_family)
                .child(text.into()),
        )
}
