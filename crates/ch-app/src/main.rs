// Release builds on Windows would otherwise open a console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ch_ui::{DARK, LIGHT, Palette, Status};
use gpui_kit::{
    AppContext, Context, FontWeight, IntoElement, ParentElement, Render, Styled, Subscription,
    TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions, application, div, px,
    rgb, size,
};

/// Inbox groups, most urgent first. Sessions are sorted by what they need from the user,
/// not by repository or start time.
const INBOX: [(&str, Status); 4] = [
    ("Needs you", Status::NeedsYou),
    ("Running", Status::Running),
    ("Needs review", Status::Review),
    ("Landed", Status::Done),
];

fn main() {
    application().run(|cx| {
        gpui_kit::init(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1100.0), px(720.0)), cx)),
            window_min_size: Some(size(px(720.0), px(480.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("C-Harness".into()),
                ..Default::default()
            }),
            app_id: Some("c-harness".into()),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Shell::new(window, cx))
        });
        if let Err(err) = opened {
            eprintln!("c-harness: could not open a window: {err:#}");
            cx.quit();
        }
    });
}

struct Shell {
    palette: Palette,
    _appearance: Subscription,
}

impl Shell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |shell, window, cx| {
            shell.palette = palette_for(window.appearance());
            cx.notify();
        });
        Self {
            palette: palette_for(window.appearance()),
            _appearance: appearance,
        }
    }
}

fn palette_for(appearance: WindowAppearance) -> Palette {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => DARK,
        WindowAppearance::Light | WindowAppearance::VibrantLight => LIGHT,
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        div()
            .flex()
            .size_full()
            .bg(rgb(p.bg))
            .text_color(rgb(p.text))
            .child(sidebar(p))
            .child(empty_state(p))
    }
}

fn sidebar(p: Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px(240.0))
        .h_full()
        .p_3()
        .gap_1()
        .bg(rgb(p.surface))
        .border_r_1()
        .border_color(rgb(p.border))
        .child(
            div()
                .px_2()
                .pb_3()
                .font_weight(FontWeight::SEMIBOLD)
                .child("C-Harness"),
        )
        .children(INBOX.map(|(label, status)| inbox_row(p, label, status)))
}

// The label carries the state. Some light-theme status colors fall under 3:1 on `surface`,
// so the dot only decorates.
fn inbox_row(p: Palette, label: &'static str, status: Status) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .text_sm()
        .child(div().size_2().rounded_full().bg(rgb(status.color())))
        .child(div().flex_1().child(label))
        .child(div().text_color(rgb(p.text_muted)).child("0"))
}

fn empty_state(p: Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_1()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("No sessions"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(p.text_muted))
                .child("This build cannot connect to a sandbox yet."),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_dark_appearances_pick_the_dark_palette() {
        assert_eq!(palette_for(WindowAppearance::Dark), DARK);
        assert_eq!(palette_for(WindowAppearance::VibrantDark), DARK);
        assert_eq!(palette_for(WindowAppearance::Light), LIGHT);
        assert_eq!(palette_for(WindowAppearance::VibrantLight), LIGHT);
    }
}
