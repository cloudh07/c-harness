//! Settings, Notifications: whether a run that finishes in the background is announced, and
//! whether a sound comes with it.

use std::time::Duration;

use ch_ui::Palette;
use gpui_kit::base::{Spring, Switch, spring};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px, rgb, transparent_black,
};

use super::{SettingsSaved, SettingsView};
use crate::outbound_view::OutboundSection;
use crate::widgets::{card, muted, section};

const SLIDE: Spring = Spring::new(Duration::from_millis(220));
const TRACK_WIDTH: f32 = 32.0;
const TRACK_HEIGHT: f32 = 18.0;
const INSET: f32 = 2.0;
const THUMB: f32 = TRACK_HEIGHT - 2.0 * INSET;
const TRAVEL: f32 = TRACK_WIDTH - THUMB - 2.0 * INSET;

type OnChange = Box<dyn Fn(&bool, &mut Window, &mut App)>;

impl SettingsView {
    /// A switch takes effect when it is flipped, as the OS's own do, so these save on their own
    /// like the theme and reach the session view at once.
    fn set_notices(&mut self, notify: bool, sound: bool, cx: &mut Context<Self>) {
        self.saved.notify_finished = notify;
        self.saved.finished_sound = sound;
        self.save_alone("the notification choice", cx);
        cx.emit(SettingsSaved(self.saved.clone()));
        cx.notify();
    }

    /// What the app does on its own: the notices it gives the OS, then what leaves this machine.
    pub(super) fn outward(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.notice_toggles(cx))
            .child(OutboundSection::new(self.palette))
    }

    fn notice_toggles(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let (notify, sound) = (self.saved.notify_finished, self.saved.finished_sound);
        let notify_row = SwitchRow {
            palette: p,
            id: "notify-finished",
            label: "Tell me when a run finishes",
            hint: "Only while C-Harness is in the background, and not for a run you stopped. It \
                   says a session is ready for review, nothing about the work.",
            on: notify,
            enabled: true,
            on_change: Box::new(cx.listener(|this, on: &bool, _, cx| {
                this.set_notices(*on, this.saved.finished_sound, cx);
            })),
        };
        let sound_row = SwitchRow {
            palette: p,
            id: "finished-sound",
            label: "Play a sound with it",
            hint: "Your system's alert sound.",
            on: sound,
            enabled: notify,
            on_change: Box::new(cx.listener(|this, on: &bool, _, cx| {
                this.set_notices(this.saved.notify_finished, *on, cx);
            })),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section("Notifications"))
            .child(card(p).p_1().gap_0p5().child(notify_row).child(sound_row))
    }
}

/// The whole row is the switch, so the label is part of what you click.
#[derive(IntoElement)]
struct SwitchRow {
    palette: Palette,
    id: &'static str,
    label: &'static str,
    hint: &'static str,
    on: bool,
    enabled: bool,
    on_change: OnChange,
}

impl RenderOnce for SwitchRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = self.palette;
        let target = if self.on { 1.0_f32 } else { 0.0 };
        let progress: f32 = spring(("notice-switch", self.id), target, SLIDE, window, cx);
        let on_change = self.on_change;
        Switch::new(self.id)
            .checked(self.on)
            .disabled(!self.enabled)
            .accessibility_label(self.label)
            .on_change(move |on, _, window, cx| on_change(&on, window, cx))
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(transparent_black())
            .focus_visible(move |s| s.border_color(rgb(p.accent)))
            .when(self.enabled, |row| {
                row.cursor_pointer()
                    .hover(move |s| s.bg(rgb(p.hover)))
                    .active(move |s| s.bg(rgb(p.border)))
            })
            .when(!self.enabled, |row| row.opacity(0.5))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(p.text))
                            .child(self.label),
                    )
                    .child(muted(p, self.hint)),
            )
            .child(track(p, progress))
    }
}

/// Where the thumb sits, with a spring that overshoots kept inside the track.
fn thumb_left(progress: f32) -> f32 {
    INSET + TRAVEL * progress.clamp(0.0, 1.0)
}

/// Neutral on purpose: the palette keeps its colors for the primary button and focus. The "on"
/// fill fades in under the thumb as it slides.
fn track(p: Palette, progress: f32) -> impl IntoElement {
    let progress = progress.clamp(0.0, 1.0);
    div()
        .relative()
        .flex_none()
        .w(px(TRACK_WIDTH))
        .h(px(TRACK_HEIGHT))
        .rounded_full()
        .bg(rgb(p.border_strong))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .bg(rgb(p.text))
                .opacity(progress),
        )
        .child(
            div()
                .absolute()
                .top(px(INSET))
                .left(px(thumb_left(progress)))
                .size(px(THUMB))
                .rounded_full()
                .bg(rgb(p.bg)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_stays_inside_the_track() {
        assert!((thumb_left(0.0) - INSET).abs() < f32::EPSILON);
        assert!((thumb_left(1.0) + THUMB + INSET - TRACK_WIDTH).abs() < f32::EPSILON);
        for overshoot in [-0.2, 1.2] {
            let left = thumb_left(overshoot);
            assert!(
                left >= INSET && left + THUMB + INSET <= TRACK_WIDTH,
                "{overshoot}"
            );
        }
    }
}
