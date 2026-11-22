//! Whether the transcript keeps up with new output or stays where the person is reading.

#[cfg(test)]
mod tests {
    //! A replay of the approval fixture against a model of the transcript's scroll area. Heights
    //! are rough; what matters is where new output lands relative to the view.

    use ch_core::{ClientAction, Session};
    use ch_protocol::runner::route_runner_line;

    const FIXTURE: &str = include_str!("../../../../testdata/omp/approval-session.jsonl");
    const VIEWPORT: f32 = 100.0;
    const EARLIER: f32 = 400.0;
    const LINE: f32 = 20.0;
    const CHARS_PER_LINE: usize = 8;
    const TOOL_CARD: f32 = 160.0;
    const TOOL_DONE: usize = 7;

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

    /// The scroll area as `session_view` sets it up today: GPUI keeps the offset from the top,
    /// clamps it at the next layout, and nothing else moves it.
    struct View {
        top: f32,
    }

    impl View {
        fn at(top: f32) -> Self {
            View { top }
        }

        fn frame(&mut self, layout: Layout) {
            self.top = self.top.clamp(0.0, layout.max_top());
        }

        fn sees_end(&self, layout: Layout) -> bool {
            self.top + VIEWPORT >= layout.total()
        }
    }

    #[test]
    fn today_new_output_lands_below_the_fold() {
        let mut view = View::at(start().max_top());
        assert!(view.sees_end(start()));
        let layouts = replay();
        for layout in &layouts {
            view.frame(*layout);
        }
        let last = *layouts.last().expect("a fixture");
        assert!(!view.sees_end(last));
        assert_eq!(view.top, start().max_top());
    }

    #[test]
    fn today_reading_earlier_turns_stays_put() {
        let mut view = View::at(EARLIER / 2.0);
        for layout in replay() {
            view.frame(layout);
            assert_eq!(view.top, EARLIER / 2.0);
        }
    }

    #[test]
    fn today_a_restored_session_opens_at_the_top() {
        let mut view = View::at(0.0);
        let layouts = replay();
        for layout in &layouts {
            view.frame(*layout);
        }
        assert_eq!(view.top, 0.0);
        assert!(!view.sees_end(*layouts.last().expect("a fixture")));
    }

    #[test]
    fn today_reply_growth_moves_the_tool_card_being_read() {
        let layouts = replay();
        let before = layouts[TOOL_DONE];
        let mut view = View::at(before.tools_at() + LINE);
        view.frame(before);
        let card_before = before.tools_at() - view.top;
        let after = layouts[TOOL_DONE + 1];
        view.frame(after);
        let card_after = after.tools_at() - view.top;
        assert!(after.reply > before.reply);
        assert_eq!(card_after - card_before, after.reply - before.reply);
    }
}
