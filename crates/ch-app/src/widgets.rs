//! Small styled pieces shared by the views, so every screen uses the same palette rules.

use ch_ui::{Palette, Status};
use gpui_kit::base::{Button, ColorTokens, Theme, ThemeAppearance};
use gpui_kit::{
    App, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, Stateful, StatefulInteractiveElement, Styled, div, px, rgb,
};

#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas";
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const MONO: &str = "DejaVu Sans Mono";

const SELECTION_ALPHA: f32 = 0.35;

fn hsla(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Points gpui-base's own components (text inputs, selection) at the app palette.
pub fn sync_theme(p: Palette, dark: bool, cx: &mut App) {
    let mut colors = if dark {
        ColorTokens::dark()
    } else {
        ColorTokens::light()
    };
    colors.background = hsla(p.bg);
    colors.foreground = hsla(p.text);
    colors.surface = hsla(p.elevated);
    colors.surface_foreground = hsla(p.text);
    colors.muted_foreground = hsla(p.text_muted);
    colors.border = hsla(p.border);
    colors.input = hsla(p.border);
    colors.accent = hsla(p.accent);
    colors.ring = hsla(p.accent);
    colors.primary = hsla(p.accent);
    colors.selection = Hsla {
        a: SELECTION_ALPHA,
        ..hsla(p.accent)
    };
    let theme = Theme::global_mut(cx);
    theme.appearance = if dark {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    };
    theme.tokens.colors = colors;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Plain,
    Danger,
}

/// A button in the app style. The caller adds `on_click`; a disabled button drops it.
pub fn button(
    p: Palette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    enabled: bool,
) -> Button {
    let (bg, fg, border) = match tone {
        Tone::Primary => (p.accent, 0xFF_FF_FF, p.accent),
        Tone::Plain => (p.elevated, p.text, p.border),
        Tone::Danger => (p.elevated, Status::Error.color(), Status::Error.color()),
    };
    let base = Button::new(id)
        .disabled(!enabled)
        .px_3()
        .py(px(6.0))
        .rounded_md()
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(label.into());
    if enabled {
        base.cursor_pointer().hover(|s| s.opacity(0.85))
    } else {
        base.opacity(0.45)
    }
}

/// A labelled input. The input draws its own text; this draws the box around it.
pub fn field(
    p: Palette,
    label: &'static str,
    hint: Option<&'static str>,
    input: impl IntoElement,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
        .child(
            div()
                .w_full()
                .px_2()
                .py(px(6.0))
                .rounded_md()
                .border_1()
                .border_color(rgb(p.border))
                .bg(rgb(p.elevated))
                .text_sm()
                .child(input),
        )
        .children(hint.map(|hint| muted(p, hint)))
}

pub fn section(title: impl Into<SharedString>) -> Div {
    div()
        .pt_4()
        .text_base()
        .font_weight(FontWeight::SEMIBOLD)
        .child(title.into())
}

pub fn muted(p: Palette, text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(rgb(p.text_muted))
        .child(text.into())
}

/// A bordered block with a colored edge, for failures, warnings and notices.
pub fn banner(
    p: Palette,
    color: u32,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_l_4()
        .border_color(rgb(color))
        .bg(rgb(p.surface))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(div().text_sm().child(body.into()))
}

/// A card that groups one part of a session: checks, the diff, the landing.
pub fn card(p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.surface))
}

pub fn mono_block(
    p: Palette,
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
) -> Stateful<Div> {
    div()
        .id(id)
        .max_h(px(320.0))
        .overflow_y_scroll()
        .p_2()
        .rounded_md()
        .bg(rgb(p.bg))
        .font_family(MONO)
        .text_xs()
        .child(gpui_kit::base::SelectableText::new("mono", text.into()))
}
