//! Settings, What leaves this machine. Shows `ch_core::OUTBOUND` grouped by who decides whether a
//! row happens; each row opens to show what, when, why and how we know.

use std::collections::HashSet;
use std::f32::consts::FRAC_PI_2;
use std::time::Duration;

use ch_core::{Control, Evidence, OMP_VERSION_SEEN, OUTBOUND, OUTBOUND_SUMMARY, Outbound};
use ch_ui::Palette;
use gpui_kit::base::{Button, MotionReveal, Spring, spring};
use gpui_kit::{
    App, Div, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Transformation, Window, div, px, radians,
    relative, rgb, transparent_black,
};

use crate::assets::{Icon, icon};
use crate::widgets::{card, eyebrow, muted, section};

/// No overshoot: a height that bounces past its end looks broken.
const REVEAL: Spring = Spring::new(Duration::from_millis(320));
const FACT_LABEL_WIDTH: f32 = 112.0;
const CHEVRON: f32 = 14.0;

fn heading(control: Control) -> &'static str {
    match control {
        Control::NoSwitch => "Whenever a session runs",
        Control::Setting => "On until you turn it off",
        Control::YourChoice => "Only when you choose",
        Control::Off => "Turned off",
    }
}

/// What a screen reader says for a row, since the group heading is not read when tabbing.
fn accessible_name(row: &Outbound) -> String {
    let doubt = match row.evidence {
        Evidence::NotVerified => " Not verified.",
        Evidence::InCode | Evidence::Observed => "",
    };
    format!(
        "{}. Goes to: {}. {}.{doubt}",
        row.title,
        row.destination,
        row.control.label()
    )
}

/// Rows the reader opened, by id. Lives while the page is on screen.
#[derive(Default)]
struct OpenRows(HashSet<&'static str>);

impl OpenRows {
    fn toggle(&mut self, id: &'static str) {
        if !self.0.remove(id) {
            self.0.insert(id);
        }
    }

    fn is_open(&self, id: &str) -> bool {
        self.0.contains(id)
    }
}

/// The section at the foot of Settings.
#[derive(IntoElement)]
pub struct OutboundSection {
    palette: Palette,
}

impl OutboundSection {
    pub fn new(palette: Palette) -> Self {
        Self { palette }
    }
}

impl RenderOnce for OutboundSection {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = self.palette;
        let open = window.use_keyed_state("outbound-open", cx, |_, _| OpenRows::default());
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section("What leaves this machine"))
            .child(muted(p, OUTBOUND_SUMMARY))
            .children(Control::ORDER.into_iter().filter_map(|control| {
                let rows: Vec<_> = OUTBOUND
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.control == control)
                    .map(|(index, row)| row_view(p, index, row, &open, window, cx))
                    .collect();
                (!rows.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(eyebrow(p, heading(control)))
                        .child(card(p).p_1().gap_0p5().children(rows))
                })
            }))
            .child(muted(
                p,
                format!(
                    "Rows seen in a test run were checked against omp {OMP_VERSION_SEEN}. \
                     PRIVACY.md keeps the same list."
                ),
            ))
    }
}

fn row_view(
    p: Palette,
    index: usize,
    row: &'static Outbound,
    open: &Entity<OpenRows>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let is_open = open.read(cx).is_open(row.id);
    let target = if is_open { 1.0_f32 } else { 0.0 };
    let progress: f32 = spring(("outbound-reveal", row.id), target, REVEAL, window, cx);
    let open = open.clone();
    let trigger = Button::new(("outbound-row", index))
        .accessibility_label(accessible_name(row))
        .aria_expanded(is_open)
        .on_click(move |_, _, cx| {
            open.update(cx, |rows, cx| {
                rows.toggle(row.id);
                cx.notify();
            });
        })
        .w_full()
        .justify_start()
        .items_start()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(transparent_black())
        .line_height(relative(1.4))
        .text_color(rgb(p.text))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .active(move |s| s.bg(rgb(p.border)))
        .focus_visible(move |s| s.border_color(rgb(p.accent)))
        .child(
            icon(Icon::ChevronDown, px(CHEVRON), p.text_muted)
                .mt(px(3.0))
                .with_transformation(Transformation::rotate(radians(
                    -FRAC_PI_2 * (1.0 - progress),
                ))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(row.title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.text_muted))
                        .child(format!("Goes to: {}", row.destination)),
                ),
        )
        .children((row.evidence == Evidence::NotVerified).then(|| not_verified(p)));
    div()
        .flex()
        .flex_col()
        .child(trigger)
        .children((is_open || progress > 0.0).then(|| {
            MotionReveal::new(
                ("outbound-panel", index),
                progress,
                details(p, row).opacity(progress).into_any_element(),
            )
        }))
}

/// Neutral on purpose: amber is kept for the one thing that needs the user.
fn not_verified(p: Palette) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(rgb(p.text_muted))
        .child(icon(Icon::CircleDashed, px(12.0), p.text_muted))
        .child("Not verified")
}

fn details(p: Palette, row: &Outbound) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .pl(px(CHEVRON + 24.0))
        .pr_3()
        .pt_1()
        .pb_3()
        .child(fact(p, "What", row.what))
        .child(fact(p, "When", row.when))
        .child(fact(p, "Why", row.why))
        .child(fact(p, "Turning it off", row.turn_off))
        .child(fact(
            p,
            "How we know",
            format!("{}: {}", row.evidence.label(), row.source),
        ))
}

fn fact(p: Palette, label: &'static str, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .gap_3()
        .text_sm()
        .line_height(relative(1.45))
        .child(
            div()
                .w(px(FACT_LABEL_WIDTH))
                .flex_none()
                .text_color(rgb(p.text_muted))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(rgb(p.text))
                .child(text.into()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_opens_then_closes() {
        let mut rows = OpenRows::default();
        rows.toggle("github");
        assert!(rows.is_open("github"));
        assert!(!rows.is_open("ssh-host"));
        rows.toggle("github");
        assert!(!rows.is_open("github"));
    }

    #[test]
    fn every_group_has_its_own_heading() {
        let headings: HashSet<_> = Control::ORDER.into_iter().map(heading).collect();
        assert_eq!(headings.len(), Control::ORDER.len());
    }

    #[test]
    fn the_spoken_name_carries_destination_and_doubt() {
        for row in OUTBOUND {
            let name = accessible_name(row);
            assert!(name.starts_with(row.title), "{}", row.id);
            assert!(name.contains(row.destination), "{}", row.id);
            assert_eq!(
                name.contains("Not verified"),
                row.evidence == Evidence::NotVerified,
                "{}",
                row.id
            );
        }
    }
}
