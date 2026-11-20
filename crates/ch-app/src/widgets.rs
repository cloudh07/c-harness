//! Small styled pieces shared by the views, so every screen uses the same palette rules.

use ch_ui::{Palette, Status};
use gpui_kit::base::{Button, ColorTokens, Theme, ThemeAppearance};
use gpui_kit::{
    AnyView, App, AppContext, Context, Div, ElementId, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Window, div, px, rgb,
};

use crate::assets::{Icon, icon};

#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas";
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const MONO: &str = "DejaVu Sans Mono";

const SELECTION_ALPHA: f32 = 0.35;
const DISABLED_OPACITY: f32 = 0.45;
const ICON_BUTTON: f32 = 28.0;
const ICON_SIZE: f32 = 16.0;

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
    colors.muted = hsla(p.hover);
    colors.muted_foreground = hsla(p.text_muted);
    colors.destructive = hsla(p.status(Status::Error));
    colors.border = hsla(p.border);
    colors.input = hsla(p.border);
    colors.accent = hsla(p.accent);
    colors.ring = hsla(p.accent);
    colors.primary = hsla(p.primary);
    colors.primary_foreground = hsla(p.on_primary);
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

/// One short line that names what a control does.
pub struct Tip {
    text: SharedString,
    palette: Palette,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(p.border_strong))
            .bg(rgb(p.elevated))
            .text_xs()
            .text_color(rgb(p.text))
            .child(self.text.clone())
    }
}

/// A builder for `.tooltip()`.
pub fn tip(
    p: Palette,
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tip { text, palette: p }).into()
    }
}

/// How loud a button is. Each screen has at most one `Primary`, picked by what the session
/// needs next.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Plain,
    Danger,
    /// No fill or border until hovered. For icon buttons in toolbars.
    Ghost,
}

impl Tone {
    fn foreground(self, p: Palette) -> u32 {
        match self {
            Tone::Primary => p.on_primary,
            Tone::Plain => p.text,
            Tone::Danger => p.status(Status::Error),
            Tone::Ghost => p.text_muted,
        }
    }
}

fn paint(p: Palette, base: Button, tone: Tone, enabled: bool) -> Button {
    let fg = rgb(tone.foreground(p));
    let base = match tone {
        Tone::Primary => base
            .border_1()
            .border_color(rgb(p.primary))
            .bg(rgb(p.primary)),
        Tone::Plain => base
            .border_1()
            .border_color(rgb(p.border))
            .bg(rgb(p.elevated)),
        Tone::Danger => base
            .border_1()
            .border_color(rgb(p.status(Status::Error)))
            .bg(rgb(p.elevated)),
        Tone::Ghost => base,
    }
    .text_color(fg);
    if !enabled {
        return base.opacity(DISABLED_OPACITY);
    }
    let base = base.cursor_pointer();
    match tone {
        Tone::Primary => base.hover(|s| s.opacity(0.9)),
        Tone::Plain | Tone::Danger | Tone::Ghost => base.hover(move |s| s.bg(rgb(p.hover))),
    }
}

/// A text button in the app style. The caller adds `on_click`; a disabled button drops it.
pub fn button(
    p: Palette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    enabled: bool,
) -> Button {
    let base = Button::new(id)
        .disabled(!enabled)
        .gap(px(6.0))
        .px(px(14.0))
        .py(px(6.0))
        .rounded_full()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(label.into());
    paint(p, base, tone, enabled)
}

/// A text button with a leading icon.
pub fn icon_text_button(
    p: Palette,
    id: impl Into<ElementId>,
    glyph: Icon,
    label: impl Into<SharedString>,
    tone: Tone,
    enabled: bool,
) -> Button {
    let base = Button::new(id)
        .disabled(!enabled)
        .gap(px(6.0))
        .px(px(12.0))
        .py(px(6.0))
        .rounded_full()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(icon(glyph, px(ICON_SIZE - 2.0), tone.foreground(p)))
        .child(label.into());
    paint(p, base, tone, enabled)
}

/// A square button that shows only an icon. `label` is read by screen readers and shown as
/// the tooltip, so the icon never has to explain itself.
pub fn icon_button(
    p: Palette,
    id: impl Into<ElementId>,
    glyph: Icon,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> Button {
    let shape = Button::new(id)
        .disabled(!enabled)
        .flex_none()
        .size(px(ICON_BUTTON))
        .accessibility_label(label)
        .tooltip(tip(p, label))
        .child(icon(glyph, px(ICON_SIZE), tone.foreground(p)));
    let shape = if tone == Tone::Ghost {
        shape.rounded_md()
    } else {
        shape.rounded_full()
    };
    paint(p, shape, tone, enabled)
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

/// A small label above a group of rows, as in a sidebar or side panel.
pub fn eyebrow(p: Palette, text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(p.text_muted))
        .child(text.into())
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
        .rounded_lg()
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
        .rounded_lg()
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
