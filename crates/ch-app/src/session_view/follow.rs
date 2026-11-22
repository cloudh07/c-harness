//! Whether the transcript keeps up with new output or stays where the person is reading.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt, App, BoxShadow, ClickEvent, Context, Div, FocusHandle, Hsla,
    InteractiveElement, KeyDownEvent, ParentElement, ScrollEvent, ScrollHandle,
    StatefulInteractiveElement, Styled, div, ease_out_quint, point, px, rgb,
};

use super::{CONTENT_WIDTH, Phase, SessionView};
use crate::assets::Icon;
use crate::widgets::{Tone, icon_text_button};

/// How far from the end, in pixels, still counts as back at the end when scrolling down.
const AT_END: f32 = 24.0;
const JUMP_FADE: Duration = Duration::from_millis(180);
const JUMP_LABEL: &str = "Jump to latest";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Following,
    /// The person scrolled away from the end. `unseen` is set once more output arrives.
    Reading {
        unseen: bool,
    },
}

/// Distances are from the end of the transcript, after the scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Event {
    ScrolledUp(f32),
    ScrolledDown(f32),
    Output,
    Latest,
}

impl Mode {
    fn next(self, event: Event) -> Mode {
        match (self, event) {
            (_, Event::Latest) => Mode::Following,
            (Mode::Following, Event::ScrolledUp(from_end)) if from_end > 0.0 => {
                Mode::Reading { unseen: false }
            }
            (Mode::Reading { .. }, Event::ScrolledDown(from_end)) if from_end <= AT_END => {
                Mode::Following
            }
            (Mode::Reading { .. }, Event::Output) => Mode::Reading { unseen: true },
            (mode, _) => mode,
        }
    }
}

/// GPUI has already moved the offset when a scroll listener runs, but clamps it only at the
/// next layout, so the clamp happens here.
fn from_end(offset_y: f32, max_y: f32) -> f32 {
    max_y + offset_y.clamp(-max_y, 0.0)
}

/// A positive `dy` scrolls toward earlier output.
fn scrolled(dy: f32, offset_y: f32, max_y: f32) -> Option<Event> {
    let from_end = from_end(offset_y, max_y);
    if dy > 0.0 {
        Some(Event::ScrolledUp(from_end))
    } else if dy < 0.0 {
        Some(Event::ScrolledDown(from_end))
    } else {
        None
    }
}

pub(super) struct Follow {
    mode: Mode,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl Follow {
    pub(super) fn new(cx: &App) -> Self {
        Follow {
            mode: Mode::Following,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    pub(super) fn output(&mut self) {
        self.apply(Event::Output);
    }

    /// A session opened: start at its end.
    pub(super) fn reset(&mut self) {
        self.apply(Event::Latest);
    }

    fn apply(&mut self, event: Event) -> bool {
        let next = self.mode.next(event);
        let changed = next != self.mode;
        self.mode = next;
        changed
    }
}

impl SessionView {
    /// The scrolling thread. It stays at the end while following and offers a way back once
    /// the person has scrolled away and more arrived.
    pub(super) fn transcript(&self, cx: &mut Context<Self>) -> Div {
        let live = matches!(self.phase, Phase::Live(_));
        if live && self.follow.mode == Mode::Following {
            self.follow.scroll.scroll_to_bottom();
        }
        let behind = live && self.follow.mode == (Mode::Reading { unseen: true });
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .track_focus(&self.follow.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let keys = &event.keystroke;
                if keys.key == "end"
                    && !keys.modifiers.modified()
                    && this.follow.apply(Event::Latest)
                {
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id("session-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.follow.scroll)
                    .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                        let dy = f32::from(event.delta.pixel_delta(window.line_height()).y);
                        let scroll = &this.follow.scroll;
                        let offset = f32::from(scroll.offset().y);
                        let max = f32::from(scroll.max_offset().y);
                        if let Some(event) = scrolled(dy, offset, max)
                            && this.follow.apply(event)
                        {
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .mx_auto()
                            .w_full()
                            .max_w(px(CONTENT_WIDTH))
                            .px_6()
                            .py_6()
                            .child(self.body(cx)),
                    ),
            )
            .children(behind.then(|| self.jump_to_latest(cx)))
    }

    fn jump_to_latest(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let ring: Hsla = rgb(p.accent).into();
        let button = icon_text_button(
            p,
            "jump-latest",
            Icon::ChevronDown,
            JUMP_LABEL,
            Tone::Plain,
            true,
        )
        .accessibility_label(JUMP_LABEL)
        .focus_visible(move |s| {
            s.border_color(ring).shadow(vec![BoxShadow {
                color: ring,
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(0.0),
                spread_radius: px(2.0),
                inset: false,
            }])
        })
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.follow.reset();
            this.follow.focus.focus(window, cx);
            cx.notify();
        }));
        div()
            .absolute()
            .bottom_3()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(button.with_animation(
                "jump-latest-in",
                Animation::new(JUMP_FADE).with_easing(ease_out_quint()),
                Styled::opacity,
            ))
    }
}

#[cfg(test)]
mod tests {
    //! The rule as a table, then a replay of the approval fixture against a model of the
    //! scroll area. Heights in the replay are rough; what matters is where new output lands
    //! relative to the view.

    use super::{AT_END, Event, Mode, from_end, scrolled};
    use ch_core::{ClientAction, Session};
    use ch_protocol::runner::route_runner_line;

    const FIXTURE: &str = include_str!("../../../../testdata/omp/approval-session.jsonl");
    const VIEWPORT: f32 = 100.0;
    const EARLIER: f32 = 400.0;
    const LINE: f32 = 20.0;
    const CHARS_PER_LINE: usize = 8;
    const TOOL_CARD: f32 = 160.0;
    const TOOL_DONE: usize = 7;

    const READING: Mode = Mode::Reading { unseen: false };
    const BEHIND: Mode = Mode::Reading { unseen: true };

    #[test]
    fn the_follow_rule() {
        let rows = [
            (Mode::Following, Event::Output, Mode::Following),
            (Mode::Following, Event::ScrolledUp(1.0), READING),
            (Mode::Following, Event::ScrolledUp(0.0), Mode::Following),
            (Mode::Following, Event::ScrolledDown(0.0), Mode::Following),
            (Mode::Following, Event::Latest, Mode::Following),
            (READING, Event::Output, BEHIND),
            (BEHIND, Event::Output, BEHIND),
            (READING, Event::ScrolledUp(500.0), READING),
            (BEHIND, Event::ScrolledUp(AT_END), BEHIND),
            (READING, Event::ScrolledDown(AT_END + 1.0), READING),
            (BEHIND, Event::ScrolledDown(AT_END + 1.0), BEHIND),
            (READING, Event::ScrolledDown(AT_END), Mode::Following),
            (BEHIND, Event::ScrolledDown(0.0), Mode::Following),
            (READING, Event::Latest, Mode::Following),
            (BEHIND, Event::Latest, Mode::Following),
        ];
        for (from, event, to) in rows {
            assert_eq!(from.next(event), to, "{from:?} on {event:?}");
        }
    }

    #[test]
    fn distance_from_the_end() {
        let rows = [
            (0.0, 0.0, 0.0),
            (-300.0, 300.0, 0.0),
            (-320.0, 300.0, 0.0),
            (-200.0, 300.0, 100.0),
            (0.0, 300.0, 300.0),
            (20.0, 300.0, 300.0),
        ];
        for (offset, max, want) in rows {
            assert_eq!(from_end(offset, max), want, "offset {offset}, max {max}");
        }
        assert_eq!(scrolled(0.0, -100.0, 300.0), None);
        assert_eq!(scrolled(5.0, -100.0, 300.0), Some(Event::ScrolledUp(200.0)));
        assert_eq!(
            scrolled(-5.0, -100.0, 300.0),
            Some(Event::ScrolledDown(200.0))
        );
    }

    /// Heights in the order `live_thread` stacks them: earlier turns, the reply, the tool calls.
    #[derive(Clone, Copy, Debug)]
    struct Layout {
        reply: f32,
        tools: f32,
    }

    impl Layout {
        fn of(session: &Session) -> Self {
            let lines = session.reply().chars().count().div_ceil(CHARS_PER_LINE);
            Layout {
                reply: px(lines) * LINE,
                tools: px(session.tool_calls().len()) * TOOL_CARD,
            }
        }

        fn total(self) -> f32 {
            EARLIER + self.reply + self.tools
        }

        fn max_top(self) -> f32 {
            (self.total() - VIEWPORT).max(0.0)
        }

        fn tools_at(self) -> f32 {
            EARLIER + self.reply
        }
    }

    fn px(count: usize) -> f32 {
        f32::from(u16::try_from(count).expect("a small count"))
    }

    /// The layout after each fixture line, with the prompt and the approval sent where the app
    /// sends them.
    fn replay() -> Vec<Layout> {
        let mut session = Session::new();
        session.apply_action(&ClientAction::StartSent);
        FIXTURE
            .lines()
            .enumerate()
            .map(|(n, line)| {
                match n {
                    1 => session.apply_action(&ClientAction::PromptSent),
                    5 => session.apply_action(&ClientAction::ApprovalAnswered {
                        id: "ui_1".to_owned(),
                    }),
                    _ => {}
                }
                session.apply_line(&route_runner_line(line).expect("a routable line"));
                Layout::of(&session)
            })
            .collect()
    }

    fn start() -> Layout {
        Layout::of(&Session::new())
    }

    /// The scroll area as `transcript` drives it: GPUI moves the offset on a scroll and clamps
    /// it at the next layout, and a following view is put back at the end on every frame.
    struct View {
        top: f32,
        mode: Mode,
    }

    impl View {
        fn at(top: f32, mode: Mode) -> Self {
            View { top, mode }
        }

        fn frame(&mut self, layout: Layout) {
            self.top = if self.mode == Mode::Following {
                layout.max_top()
            } else {
                self.top.clamp(0.0, layout.max_top())
            };
        }

        fn output(&mut self, layout: Layout) {
            self.mode = self.mode.next(Event::Output);
            self.frame(layout);
        }

        fn scroll(&mut self, dy: f32, layout: Layout) {
            self.top -= dy;
            if let Some(event) = scrolled(dy, -self.top, layout.max_top()) {
                self.mode = self.mode.next(event);
            }
            self.frame(layout);
        }

        fn sees_end(&self, layout: Layout) -> bool {
            self.top + VIEWPORT >= layout.total()
        }
    }

    #[test]
    fn following_keeps_new_output_in_view() {
        let mut view = View::at(start().max_top(), Mode::Following);
        for layout in replay() {
            view.output(layout);
            assert!(view.sees_end(layout));
        }
    }

    #[test]
    fn scrolling_up_keeps_the_view_still_and_offers_the_way_back() {
        let layouts = replay();
        let mut view = View::at(start().max_top(), Mode::Following);
        view.output(layouts[0]);
        view.scroll(3.0 * LINE, layouts[0]);
        let reading_at = view.top;
        assert_eq!(view.mode, READING);
        for layout in &layouts[1..] {
            view.output(*layout);
            assert_eq!(view.top, reading_at);
        }
        assert_eq!(view.mode, BEHIND);
        let last = *layouts.last().expect("a fixture");
        view.mode = view.mode.next(Event::Latest);
        view.frame(last);
        assert!(view.sees_end(last));
        assert_eq!(view.mode, Mode::Following);
    }

    #[test]
    fn scrolling_back_to_the_end_follows_again() {
        let layouts = replay();
        let mut view = View::at(start().max_top(), Mode::Following);
        view.output(layouts[0]);
        view.scroll(3.0 * LINE, layouts[0]);
        view.output(layouts[1]);
        let near_end = layouts[1].max_top() - view.top - AT_END / 2.0;
        view.scroll(-near_end, layouts[1]);
        assert_eq!(view.mode, Mode::Following);
        for layout in &layouts[2..] {
            view.output(*layout);
            assert!(view.sees_end(*layout));
        }
    }

    #[test]
    fn a_resumed_session_opens_at_its_end() {
        let mut view = View::at(0.0, BEHIND);
        view.mode = view.mode.next(Event::Latest);
        let layouts = replay();
        for layout in &layouts {
            view.output(*layout);
        }
        assert!(view.sees_end(*layouts.last().expect("a fixture")));
    }

    /// Known limit: GPUI has no scroll anchoring, so a reply that grows above the reading point
    /// still pushes the tool cards below it down by the same amount.
    #[test]
    fn reply_growth_above_the_reader_still_moves_them() {
        let layouts = replay();
        let before = layouts[TOOL_DONE];
        let mut view = View::at(before.tools_at() + LINE, READING);
        view.frame(before);
        let card_before = before.tools_at() - view.top;
        let after = layouts[TOOL_DONE + 1];
        view.output(after);
        let card_after = after.tools_at() - view.top;
        assert!(after.reply > before.reply);
        assert_eq!(card_after - card_before, after.reply - before.reply);
    }
}
