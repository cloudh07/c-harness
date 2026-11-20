// Release builds on Windows would otherwise open a console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod approval_card;
mod driver;
mod launch;
mod link;
mod panels;
mod session_view;
mod settings;
mod settings_view;
mod view_model;
mod widgets;

use ch_ui::{DARK, LIGHT, Palette, Status};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AppContext, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, Stateful, StatefulInteractiveElement, Styled, Subscription,
    TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions, application, div, px,
    rgb, size,
};

use crate::session_view::{OpenSettings, SessionView};
use crate::settings::Settings;
use crate::settings_view::{SettingsSaved, SettingsView};
use crate::view_model::inbox_counts;
use crate::widgets::sync_theme;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Session,
    Settings,
}

struct Shell {
    palette: Palette,
    screen: Screen,
    session: Entity<SessionView>,
    settings: Entity<SettingsView>,
    _subscriptions: Vec<Subscription>,
}

/// The saved settings, and why they could not be read when that happened. A broken file
/// falls back to defaults so the window still opens and the user can fix it from Settings.
fn load_settings() -> (Settings, Option<String>) {
    let Some(path) = settings::default_path() else {
        return (
            Settings::default(),
            Some("This system has no config folder, so settings cannot be saved.".to_string()),
        );
    };
    match settings::load(&path) {
        Ok(saved) => (saved, None),
        Err(e) => (Settings::default(), Some(e.to_string())),
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => true,
        WindowAppearance::Light | WindowAppearance::VibrantLight => false,
    }
}

fn palette_for(appearance: WindowAppearance) -> Palette {
    if is_dark(appearance) { DARK } else { LIGHT }
}

impl Shell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = window.appearance();
        let palette = palette_for(appearance);
        sync_theme(palette, is_dark(appearance), cx);

        let (saved, load_error) = load_settings();
        let screen = if saved.problems().is_empty() && load_error.is_none() {
            Screen::Session
        } else {
            Screen::Settings
        };
        let session = cx.new(|cx| SessionView::new(saved.clone(), palette, window, cx));
        let settings = cx.new(|cx| SettingsView::new(saved, load_error, palette, window, cx));

        let subscriptions = vec![
            cx.subscribe(&settings, |shell, _, event: &SettingsSaved, cx| {
                let saved = event.0.clone();
                shell
                    .session
                    .update(cx, |view, cx| view.set_settings(saved, cx));
            }),
            cx.subscribe(&session, |shell, _, _: &OpenSettings, cx| {
                shell.screen = Screen::Settings;
                cx.notify();
            }),
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |shell, window, cx| {
                let appearance = window.appearance();
                shell.palette = palette_for(appearance);
                sync_theme(shell.palette, is_dark(appearance), cx);
                let palette = shell.palette;
                shell
                    .session
                    .update(cx, |view, cx| view.set_palette(palette, cx));
                shell
                    .settings
                    .update(cx, |view, cx| view.set_palette(palette, cx));
                window.refresh();
            }),
        ];
        Self {
            palette,
            screen,
            session,
            settings,
            _subscriptions: subscriptions,
        }
    }

    fn show(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        cx.notify();
    }

    fn nav_row(&self, screen: Screen, label: &'static str, cx: &Context<Self>) -> Stateful<Div> {
        let p = self.palette;
        let active = self.screen == screen;
        div()
            .id(label)
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(active, |row| row.bg(rgb(p.elevated)))
            .hover(|s| s.bg(rgb(p.elevated)))
            .on_click(cx.listener(move |shell, _: &ClickEvent, _, cx| shell.show(screen, cx)))
            .child(div().flex_1().child(label))
    }

    fn sidebar(&self, cx: &Context<Self>) -> Div {
        let p = self.palette;
        let counts = inbox_counts(self.session.read(cx).state());
        let inbox = INBOX.iter().zip(counts).map(|((label, status), count)| {
            div()
                .id(*label)
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(|s| s.bg(rgb(p.elevated)))
                .on_click(
                    cx.listener(|shell, _: &ClickEvent, _, cx| shell.show(Screen::Session, cx)),
                )
                .child(div().size_2().rounded_full().bg(rgb(status.color())))
                .child(div().flex_1().child(*label))
                .child(div().text_color(rgb(p.text_muted)).child(count.to_string()))
        });
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
            .child(self.nav_row(Screen::Session, "Session", cx))
            .child(self.nav_row(Screen::Settings, "Settings", cx))
            .child(
                div()
                    .px_2()
                    .pt_4()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(p.text_muted))
                    .child("Inbox"),
            )
            .children(inbox)
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let content = match self.screen {
            Screen::Session => div().flex().flex_1().min_w_0().child(self.session.clone()),
            Screen::Settings => div().flex().flex_1().min_w_0().child(self.settings.clone()),
        };
        div()
            .flex()
            .size_full()
            .bg(rgb(p.bg))
            .text_color(rgb(p.text))
            .child(self.sidebar(cx))
            .child(content)
    }
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
