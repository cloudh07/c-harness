//! The land checklist: one row per item with its word and reason, then the pull request text the
//! land would send.

use ch_ui::{Palette, Status};
use gpui_kit::{
    Animation, AnimationElement, AnimationExt, Div, FontWeight, InteractiveElement, ParentElement,
    Role, SharedString, Stateful, StatefulInteractiveElement, Styled, div, ease_out_quint, px, rgb,
};

use super::{FADE, FADE_FLOOR, ROW_ICON};
use crate::assets::{Icon, icon};
use crate::view_model::{Checklist, LandItem, Mark, first_chars};
use crate::widgets::{MONO, muted};

/// Enough of the description to see what it says; the land sends all of it.
const SHOWN_BODY: usize = 360;

pub(super) fn checklist_rows(p: Palette, list: &Checklist) -> Stateful<Div> {
    div()
        .id("land-checklist")
        .role(Role::List)
        .aria_label("Before you land")
        .flex()
        .flex_col()
        .gap_2()
        .children(list.items.iter().map(|item| row(p, item)))
}

/// The icon and its colour repeat the word, which is always shown. A failure that leaves a way
/// on, such as Land anyway, reads as needing you rather than as an error.
fn row(p: Palette, item: &LandItem) -> AnimationElement<Stateful<Div>> {
    let (glyph, colour) = match item.mark {
        Mark::Pass => (Icon::CircleCheck, p.status(Status::Done)),
        Mark::Fail if item.blocks => (Icon::CircleX, p.status(Status::Error)),
        Mark::Fail => (Icon::CircleAlert, p.status(Status::NeedsYou)),
        Mark::Unknown => (Icon::CircleDashed, p.text_muted),
    };
    let word = item.mark.word();
    div()
        .id(SharedString::from(format!("land-item-{}", item.name)))
        .role(Role::ListItem)
        .aria_label(item.spoken())
        .flex()
        .items_start()
        .gap_2()
        .child(icon(glyph, px(ROW_ICON), colour))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .child(div().font_weight(FontWeight::MEDIUM).child(item.name))
                        .child(div().flex_none().text_color(rgb(p.text_muted)).child(word)),
                )
                .child(muted(p, item.reason.clone())),
        )
        // Keyed on the word, so a row fades in again only when its state changes.
        .with_animation(
            SharedString::from(format!("land-item-{}-{word}", item.name)),
            Animation::new(FADE).with_easing(ease_out_quint()),
            |row, delta| row.opacity(FADE_FLOOR + (1.0 - FADE_FLOOR) * delta),
        )
}

/// The title and description exactly as the land sends them. They come from the session, so
/// they are shown rather than edited here; GitHub edits them once the pull request is open.
pub(super) fn pr_preview(p: Palette, list: &Checklist) -> Div {
    let (body, cut) = first_chars(&list.body, SHOWN_BODY);
    let body = if cut {
        format!("{}...", body.trim_end())
    } else {
        body.trim_end().to_string()
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(muted(
            p,
            "The pull request opens with this text. Edit it on GitHub once it is open.",
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(p.border))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(list.title.clone()),
                )
                .child(muted(p, body).font_family(MONO)),
        )
}
