//! The bar above the thread: what the session is, where it works, and its own actions.

use ch_ui::Status;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    ClickEvent, Context, Div, FontWeight, ParentElement, SharedString, Styled, div, px, rgb,
};

use super::{Phase, SessionView, status_for};
use crate::assets::{Icon, icon, status_icon};
use crate::view_model::{branch_for, repo_label, session_title, state_label};
use crate::widgets::{MONO, Tone, icon_button};

const STATUS_ICON: f32 = 16.0;
const META_ICON: f32 = 12.0;

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
            Phase::LaunchFailed { prompt, .. } => (session_title(prompt), None),
            Phase::Live(live) => (
                session_title(&live.first_prompt),
                Some(branch_for(&live.session)),
            ),
        };
        let repo = repo_label(&self.settings);
        let live = matches!(self.phase, Phase::Live(_));

        let meta = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_xs()
            .text_color(rgb(p.text_muted))
            .child(div().flex_none().text_color(rgb(color)).child(label))
            .when(!repo.is_empty(), |row| {
                row.child(dot(p.text_faint))
                    .child(meta_item(p, Icon::FolderGit, repo, None))
            })
            .children(branch.map(|branch| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(dot(p.text_faint))
                    .child(meta_item(p, Icon::GitBranch, branch, Some(MONO)))
            }));

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
                    .child(meta),
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
