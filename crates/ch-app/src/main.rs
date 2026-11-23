// Release builds on Windows would otherwise open a console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod approval_card;
mod assets;
mod brand;
mod driver;
mod first_run;
mod launch;
mod ledger;
mod link;
mod outbound_view;
mod panels;
mod preflight;
mod session_view;
mod settings;
mod settings_view;
mod view_model;
mod widgets;

use ch_ui::{DARK, LIGHT, Palette, Status};
use gpui_kit::base::Button;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, Stateful, StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, Window,
    WindowAppearance, WindowBounds, WindowOptions, application, div, px, rgb, size,
};

use crate::assets::{Icon, icon, status_glyph};
use crate::first_run::{FirstRunView, SetupDone, UseVerify};
use crate::session_view::{OpenSettings, SessionView};
use crate::settings::{Settings, ThemePref};
use crate::settings_view::{ReopenSetup, SettingsSaved, SettingsView};
use crate::view_model::{inbox_counts, inbox_slot};
use crate::widgets::{Tone, eyebrow, icon_text_button, sync_theme, tip};

const SIDEBAR_WIDTH: f32 = 232.0;
const ROW_ICON: f32 = 14.0;

/// Inbox groups, most urgent first. Sessions are sorted by what they need from the user,
/// not by repository or start time.
const INBOX: [(&str, Status); 4] = [
    ("Needs you", Status::NeedsYou),
    ("Running", Status::Running),
    ("Needs review", Status::Review),
    ("Landed", Status::Done),
];

const THEMES: [(ThemePref, Icon, &str); 3] = [
    (ThemePref::System, Icon::Monitor, "Match system"),
    (ThemePref::Light, Icon::Sun, "Light"),
    (ThemePref::Dark, Icon::Moon, "Dark"),
];

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
        gpui_kit::init(cx);
        // The name notifications are shown under. On Windows the first notification of a run
        // also registers it for the current user, under HKCU\Software\Classes\AppUserModelId.
        cx.set_app_identity("c-harness", "C-Harness");
        cx.on_system_notification_response(|_, cx| {
            cx.activate(true);
            for window in cx.windows() {
                // A window that closed meanwhile has nothing to bring forward.
                let _ = window.update(cx, |_, window, _| window.activate_window());
            }
        });
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1280.0), px(800.0)), cx)),
            window_min_size: Some(size(px(960.0), px(600.0))),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    FirstRun,
    Session,
    Settings,
}

/// Where the window opens. A settings file that could not be read is fixed in Settings
/// first; otherwise missing setup opens the first-run steps.
fn first_screen(setup_missing: bool, load_failed: bool) -> Screen {
    if load_failed {
        Screen::Settings
    } else if setup_missing {
        Screen::FirstRun
    } else {
        Screen::Session
    }
}

/// Where leaving setup goes. Skipping with fields still missing opens Settings, where they are
/// filled in, instead of a session that cannot start.
fn setup_exit(problems_remain: bool) -> Screen {
    if problems_remain {
        Screen::Settings
    } else {
        Screen::Session
    }
}

struct Shell {
    palette: Palette,
    screen: Screen,
    session: Entity<SessionView>,
    settings: Entity<SettingsView>,
    /// Made the first time setup is shown, so a finished setup never runs the checks.
    first_run: Option<(Entity<FirstRunView>, Vec<Subscription>)>,
    /// Settings was opened from a setup step, so saving goes back to that step.
    back_to_setup: bool,
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

/// A theme the user picked wins; `System` follows the OS as it changes.
fn theme_is_dark(pref: ThemePref, appearance: WindowAppearance) -> bool {
    match pref {
        ThemePref::System => is_dark(appearance),
        ThemePref::Light => false,
        ThemePref::Dark => true,
    }
}

fn palette_for(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

impl Shell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (saved, load_error) = load_settings();
        let dark = theme_is_dark(saved.theme, window.appearance());
        let palette = palette_for(dark);
        sync_theme(palette, dark, cx);

        let screen = first_screen(!saved.problems().is_empty(), load_error.is_some());
        let first_run = (screen == Screen::FirstRun)
            .then(|| Self::make_first_run(saved.clone(), palette, window, cx));
        let session = cx.new(|cx| SessionView::new(saved.clone(), palette, window, cx));
        let settings = cx.new(|cx| SettingsView::new(saved, load_error, palette, window, cx));

        let subscriptions = vec![
            cx.subscribe(&settings, |shell, _, event: &SettingsSaved, cx| {
                shell.settings_saved(&event.0, cx);
            }),
            cx.subscribe_in(
                &settings,
                window,
                |shell, _, _: &ReopenSetup, window, cx| {
                    shell.open_setup(window, cx);
                },
            ),
            cx.subscribe(&session, |shell, _, _: &OpenSettings, cx| {
                shell.show(Screen::Settings, cx);
            }),
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.observe(&settings, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, Shell::apply_theme),
        ];
        Self {
            palette,
            screen,
            session,
            settings,
            first_run,
            back_to_setup: false,
            _subscriptions: subscriptions,
        }
    }

    fn make_first_run(
        saved: Settings,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<FirstRunView>, Vec<Subscription>) {
        let view = cx.new(|cx| FirstRunView::new(saved, palette, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &view,
                window,
                |shell, view, event: &UseVerify, window, cx| {
                    let saved = shell
                        .settings
                        .update(cx, |settings, cx| settings.set_verify(&event.0, window, cx));
                    if let Err(why) = saved {
                        view.update(cx, |view, cx| view.verify_failed(why, cx));
                    }
                },
            ),
            cx.subscribe(&view, |shell, _, _: &OpenSettings, cx| {
                shell.show(Screen::Settings, cx);
                shell.back_to_setup = true;
            }),
            cx.subscribe(&view, |shell, _, _: &SetupDone, cx| {
                let problems_remain = !shell.settings.read(cx).saved().problems().is_empty();
                shell.show(setup_exit(problems_remain), cx);
            }),
        ];
        (view, subscriptions)
    }

    /// Shows the first-run steps from the start, with the free checks run again.
    fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.first_run {
            view.update(cx, FirstRunView::restart);
        } else {
            let saved = self.settings.read(cx).saved().clone();
            self.first_run = Some(Self::make_first_run(saved, self.palette, window, cx));
        }
        self.show(Screen::FirstRun, cx);
    }

    fn settings_saved(&mut self, saved: &Settings, cx: &mut Context<Self>) {
        self.session
            .update(cx, |view, cx| view.set_settings(saved.clone(), cx));
        if let Some((view, _)) = &self.first_run {
            view.update(cx, |view, cx| view.set_settings(saved.clone(), cx));
        }
        if self.back_to_setup {
            self.show(Screen::FirstRun, cx);
        }
    }

    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pref = self.settings.read(cx).theme();
        let dark = theme_is_dark(pref, window.appearance());
        let palette = palette_for(dark);
        self.palette = palette;
        sync_theme(palette, dark, cx);
        self.session
            .update(cx, |view, cx| view.set_palette(palette, cx));
        self.settings
            .update(cx, |view, cx| view.set_palette(palette, cx));
        if let Some((view, _)) = &self.first_run {
            view.update(cx, |view, cx| view.set_palette(palette, cx));
        }
        window.refresh();
    }

    fn set_theme(&mut self, theme: ThemePref, window: &mut Window, cx: &mut Context<Self>) {
        self.settings
            .update(cx, |view, cx| view.set_theme(theme, cx));
        self.apply_theme(window, cx);
    }

    /// Any move clears the way back to setup; opening Settings from a step sets it after.
    fn show(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        self.back_to_setup = false;
        cx.notify();
    }

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session
            .update(cx, |view, cx| view.new_session(window, cx));
        self.show(Screen::Session, cx);
    }

    fn nav_row(&self, id: &'static str, active: bool) -> Stateful<Div> {
        let p = self.palette;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py(px(5.0))
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(active, |row| row.bg(rgb(p.hover)))
            .hover(|s| s.bg(rgb(p.hover)))
    }

    fn inbox(&self, cx: &Context<Self>) -> Vec<Stateful<Div>> {
        let p = self.palette;
        let session = self.session.read(cx);
        let (state, waiting) = (session.state(), session.waiting_leftovers());
        let current = state.map(inbox_slot);
        let on_session = self.screen == Screen::Session;
        INBOX
            .iter()
            .zip(inbox_counts(state, waiting))
            .enumerate()
            .map(|(slot, ((label, status), count))| {
                let (text, number) = if count > 0 {
                    (p.text, p.text_muted)
                } else {
                    (p.text_muted, p.text_faint)
                };
                self.nav_row(label, on_session && current == Some(slot))
                    .on_click(
                        cx.listener(|shell, _: &ClickEvent, _, cx| shell.show(Screen::Session, cx)),
                    )
                    .child(status_glyph(p, *status, px(ROW_ICON)))
                    .child(div().flex_1().text_color(rgb(text)).child(*label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(number))
                            .child(count.to_string()),
                    )
            })
            .collect()
    }

    fn settings_row(&self, cx: &Context<Self>) -> Stateful<Div> {
        let p = self.palette;
        let needs_setup = self.session.read(cx).needs_setup();
        self.nav_row("nav-settings", self.screen == Screen::Settings)
            .on_click(cx.listener(|shell, _: &ClickEvent, _, cx| shell.show(Screen::Settings, cx)))
            .child(icon(Icon::Settings, px(ROW_ICON), p.text_muted))
            .child(div().flex_1().child("Settings"))
            .when(needs_setup, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.status(Status::NeedsYou)))
                        .child("Finish setup"),
                )
            })
    }

    fn theme_switch(&self, cx: &Context<Self>) -> Div {
        let p = self.palette;
        let current = self.settings.read(cx).theme();
        let segments = THEMES.iter().map(|&(pref, glyph, label)| {
            let on = pref == current;
            Button::new(label)
                .flex_1()
                .h(px(26.0))
                .rounded_md()
                .selected(on)
                .accessibility_label(format!("Theme: {label}"))
                .tooltip(tip(p, label))
                .cursor_pointer()
                .when(on, |b| b.bg(rgb(p.hover)))
                .hover(|s| s.bg(rgb(p.hover)))
                .child(icon(
                    glyph,
                    px(ROW_ICON),
                    if on { p.text } else { p.text_muted },
                ))
                .on_click(cx.listener(move |shell, _: &ClickEvent, window, cx| {
                    shell.set_theme(pref, window, cx);
                }))
        });
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded_lg()
            .border_1()
            .border_color(rgb(p.border))
            .children(segments)
    }

    fn sidebar(&self, cx: &Context<Self>) -> Div {
        let p = self.palette;
        let (can_start, live) = {
            let session = self.session.read(cx);
            (session.can_start_new(), session.state().is_some())
        };
        let new_tip = match (live, can_start) {
            (false, _) => "Start a session",
            (true, true) => {
                "End this session and start another. Work that has not landed stays in its worktree."
            }
            (true, false) => "Stop the agent before starting another session.",
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .p_3()
            .gap_1()
            .bg(rgb(p.surface))
            .border_r_1()
            .border_color(rgb(p.border))
            .child(div().px_2().pt_1().pb_3().child(brand::lockup(p)))
            .child(
                icon_text_button(
                    p,
                    "new-session",
                    Icon::Plus,
                    "New session",
                    Tone::Plain,
                    can_start,
                )
                .w_full()
                .tooltip(tip(p, new_tip))
                .on_click(cx.listener(|shell, _: &ClickEvent, window, cx| {
                    shell.new_session(window, cx);
                })),
            )
            .child(div().px_2().pt_4().pb_1().child(eyebrow(p, "Inbox")))
            .children(self.inbox(cx))
            .child(div().flex_1())
            .child(self.settings_row(cx))
            .child(div().pt_2().child(self.theme_switch(cx)))
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let content = div().flex().flex_1().min_w_0();
        let content = match (self.screen, &self.first_run) {
            (Screen::FirstRun, Some((view, _))) => content.child(view.clone()),
            (Screen::Settings, _) => content.child(self.settings.clone()),
            // Setup is only shown once its view exists.
            (Screen::Session | Screen::FirstRun, _) => content.child(self.session.clone()),
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

    const APPEARANCES: [WindowAppearance; 4] = [
        WindowAppearance::Dark,
        WindowAppearance::VibrantDark,
        WindowAppearance::Light,
        WindowAppearance::VibrantLight,
    ];

    #[test]
    fn both_dark_appearances_pick_the_dark_palette() {
        assert!(is_dark(WindowAppearance::Dark));
        assert!(is_dark(WindowAppearance::VibrantDark));
        assert!(!is_dark(WindowAppearance::Light));
        assert!(!is_dark(WindowAppearance::VibrantLight));
        assert_eq!(palette_for(true), DARK);
        assert_eq!(palette_for(false), LIGHT);
    }

    #[test]
    fn the_window_opens_on_setup_only_while_setup_is_missing() {
        assert_eq!(first_screen(false, false), Screen::Session);
        assert_eq!(first_screen(true, false), Screen::FirstRun);
        // A file that could not be read is fixed in Settings before anything else.
        assert_eq!(first_screen(true, true), Screen::Settings);
        assert_eq!(first_screen(false, true), Screen::Settings);
    }

    #[test]
    fn leaving_setup_with_fields_missing_opens_settings() {
        assert_eq!(setup_exit(true), Screen::Settings);
        assert_eq!(setup_exit(false), Screen::Session);
    }

    #[test]
    fn a_picked_theme_ignores_the_system_setting() {
        for appearance in APPEARANCES {
            assert!(!theme_is_dark(ThemePref::Light, appearance));
            assert!(theme_is_dark(ThemePref::Dark, appearance));
            assert_eq!(
                theme_is_dark(ThemePref::System, appearance),
                is_dark(appearance)
            );
        }
    }
}
