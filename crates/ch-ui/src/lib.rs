//! Color tokens as `0xRRGGBB`, and the WCAG contrast math that checks them.
//!
//! Neutrals are zinc, so the only hue on screen is a status or a tool. Each theme carries its own
//! status and tool shades: the light theme uses darker ones, because the bright shades that read
//! well on near-black fall under 3:1 on white.
//!
//! The tests hold these lines on every surface, in both themes:
//!
//! - body and muted text, and every status color used as label text, reach 4.5:1;
//! - faint text reaches 3:1, so it only carries hints a reader can do without;
//! - a status or tool icon on its own [`TINT`] chip reaches 3:1, and text never sits on a chip;
//! - the label on a primary button reaches 4.5:1 on its fill;
//! - the brand mark's lanes step down in contrast from the outer arc in.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// The three arcs and the core of the brand mark, outermost first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub outer: u32,
    pub middle: u32,
    pub inner: u32,
    pub core: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub theme: Theme,
    /// The thread and the main column.
    pub bg: u32,
    /// The sidebar and side panels.
    pub surface: u32,
    /// Cards, inputs and menus.
    pub elevated: u32,
    /// A hovered or selected row.
    pub hover: u32,
    pub border: u32,
    /// A focused or hovered input's border.
    pub border_strong: u32,
    pub text: u32,
    pub text_muted: u32,
    pub text_faint: u32,
    /// The fill of the one primary button on a screen.
    pub primary: u32,
    pub on_primary: u32,
    /// Focus rings and text selection, nothing else.
    pub accent: u32,
    pub mark: Mark,
    status: [u32; Status::ALL.len()],
    tools: [u32; ToolGroup::ALL.len()],
}

/// How strongly a status or tool color tints the chip behind its icon.
pub const TINT: f32 = 0.14;

pub const DARK: Palette = Palette {
    theme: Theme::Dark,
    bg: 0x09090B,
    surface: 0x111113,
    elevated: 0x18181B,
    hover: 0x1F1F23,
    border: 0x27272A,
    border_strong: 0x3F3F46,
    text: 0xFAFAFA,
    text_muted: 0xA1A1AA,
    text_faint: 0x71717A,
    primary: 0xFAFAFA,
    on_primary: 0x09090B,
    accent: 0x60A5FA,
    mark: Mark {
        outer: 0xF4F4F5,
        middle: 0xA1A1AA,
        inner: 0x71717A,
        core: 0xFFFFFF,
    },
    // Same order as `Status::ALL`.
    status: [
        0xF59E0B, 0x60A5FA, 0x22D3EE, 0x4ADE80, 0xF87171, 0xA1A1AA, 0xFB923C,
    ],
    // Same order as `ToolGroup::ALL`.
    tools: [
        0x38BDF8, 0xA78BFA, 0xA3E635, 0x818CF8, 0xF472B6, 0x2DD4BF, 0xA1A1AA,
    ],
};

pub const LIGHT: Palette = Palette {
    theme: Theme::Light,
    bg: 0xFFFFFF,
    surface: 0xFAFAFA,
    elevated: 0xFFFFFF,
    hover: 0xF4F4F5,
    border: 0xE4E4E7,
    border_strong: 0xD4D4D8,
    text: 0x09090B,
    text_muted: 0x52525B,
    text_faint: 0x71717A,
    primary: 0x18181B,
    on_primary: 0xFAFAFA,
    accent: 0x2563EB,
    mark: Mark {
        outer: 0x09090B,
        middle: 0x3F3F46,
        inner: 0x71717A,
        core: 0x09090B,
    },
    status: [
        0xB45309, 0x2563EB, 0x0E7490, 0x15803D, 0xB91C1C, 0x52525B, 0xC2410C,
    ],
    tools: [
        0x0369A1, 0x6D28D9, 0x4D7C0F, 0x4338CA, 0xBE185D, 0x0F766E, 0x52525B,
    ],
};

impl Palette {
    pub const fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => DARK,
            Theme::Light => LIGHT,
        }
    }

    pub const fn is_dark(&self) -> bool {
        matches!(self.theme, Theme::Dark)
    }

    pub const fn status(&self, status: Status) -> u32 {
        self.status[status as usize]
    }

    pub const fn tool(&self, group: ToolGroup) -> u32 {
        self.tools[group as usize]
    }

    /// The chip behind a status or tool icon, as painted on `under`.
    pub fn chip(&self, color: u32, under: u32) -> u32 {
        blend(color, under, TINT)
    }
}

/// Session states. Amber marks "needs you" everywhere, so the one thing that wants the user is
/// the one thing in that color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    NeedsYou,
    Running,
    Review,
    Done,
    Error,
    Idle,
    NearCap,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::NeedsYou,
        Status::Running,
        Status::Review,
        Status::Done,
        Status::Error,
        Status::Idle,
        Status::NearCap,
    ];
}

/// What kind of work a tool call does, read from the tool's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    Read,
    Edit,
    Shell,
    Web,
    Mcp,
    Agent,
    Other,
}

const READ_TOOLS: &[&str] = &["read", "grep", "find", "ls", "glob", "search"];
const EDIT_TOOLS: &[&str] = &["edit", "write", "apply_patch", "ast_edit", "notebook_edit"];
const SHELL_TOOLS: &[&str] = &["bash", "eval", "shell", "exec"];
const WEB_TOOLS: &[&str] = &["web_search", "web_fetch", "fetch", "browser"];
const AGENT_TOOLS: &[&str] = &["task", "agent", "subagent"];
const MCP_PREFIX: &str = "mcp__";

impl ToolGroup {
    pub const ALL: [ToolGroup; 7] = [
        ToolGroup::Read,
        ToolGroup::Edit,
        ToolGroup::Shell,
        ToolGroup::Web,
        ToolGroup::Mcp,
        ToolGroup::Agent,
        ToolGroup::Other,
    ];

    pub fn of(tool: &str) -> Self {
        let name = tool.trim().to_ascii_lowercase();
        let name = name.as_str();
        if name.starts_with(MCP_PREFIX) {
            ToolGroup::Mcp
        } else if READ_TOOLS.contains(&name) {
            ToolGroup::Read
        } else if EDIT_TOOLS.contains(&name) {
            ToolGroup::Edit
        } else if SHELL_TOOLS.contains(&name) {
            ToolGroup::Shell
        } else if WEB_TOOLS.contains(&name) {
            ToolGroup::Web
        } else if AGENT_TOOLS.contains(&name) {
            ToolGroup::Agent
        } else {
            ToolGroup::Other
        }
    }
}

/// `fg` laid over `bg` at `alpha`, the way a translucent fill is painted.
pub fn blend(fg: u32, bg: u32, alpha: f32) -> u32 {
    let alpha = alpha.clamp(0.0, 1.0);
    [16, 8, 0].into_iter().fold(0, |out, shift| {
        let f = f32::from(u8::try_from((fg >> shift) & 0xFF).unwrap_or(u8::MAX));
        let b = f32::from(u8::try_from((bg >> shift) & 0xFF).unwrap_or(u8::MAX));
        let mixed = (f * alpha + b * (1.0 - alpha)).round().clamp(0.0, 255.0);
        // In range after the clamp, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let channel = mixed as u32;
        out | (channel << shift)
    })
}

/// WCAG 2.2 contrast ratio between two `0xRRGGBB` colors.
pub fn contrast_ratio(a: u32, b: u32) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn relative_luminance(rgb: u32) -> f64 {
    let channel = |shift: u32| {
        let c = f64::from((rgb >> shift) & 0xFF) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AA_TEXT: f64 = 4.5;
    const NON_TEXT: f64 = 3.0;
    const THEMES: [(&str, Palette); 2] = [("dark", DARK), ("light", LIGHT)];

    fn surfaces(p: &Palette) -> [(&'static str, u32); 4] {
        [
            ("bg", p.bg),
            ("surface", p.surface),
            ("elevated", p.elevated),
            ("hover", p.hover),
        ]
    }

    fn assert_on_every_surface(p: &Palette, theme: &str, name: &str, fg: u32, min: f64) {
        for (bg_name, bg) in surfaces(p) {
            let ratio = contrast_ratio(fg, bg);
            assert!(
                ratio >= min,
                "{theme} {name} on {bg_name} is {ratio:.2}:1, needs {min}:1"
            );
        }
    }

    #[test]
    fn contrast_matches_known_extremes() {
        assert!((contrast_ratio(0x000000, 0xFFFFFF) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(0x777777, 0x777777) - 1.0).abs() < 1e-9);
        assert!((contrast_ratio(0x777777, 0xFFFFFF) - 4.48).abs() < 0.01);
    }

    #[test]
    fn blend_paints_like_a_translucent_fill() {
        assert_eq!(blend(0xFFFFFF, 0x000000, 0.0), 0x000000);
        assert_eq!(blend(0xFFFFFF, 0x000000, 1.0), 0xFFFFFF);
        assert_eq!(blend(0xFFFFFF, 0x000000, 0.5), 0x808080);
        assert_eq!(blend(0xFF0000, 0x0000FF, 0.25), 0x4000BF);
    }

    #[test]
    fn text_tokens_meet_their_floor_on_every_surface() {
        for (theme, p) in THEMES {
            assert_on_every_surface(&p, theme, "text", p.text, AA_TEXT);
            assert_on_every_surface(&p, theme, "text_muted", p.text_muted, AA_TEXT);
            assert_on_every_surface(&p, theme, "text_faint", p.text_faint, NON_TEXT);
            assert_on_every_surface(&p, theme, "accent", p.accent, NON_TEXT);
        }
    }

    #[test]
    fn every_status_reads_as_label_text_on_every_surface() {
        for (theme, p) in THEMES {
            for s in Status::ALL {
                assert_on_every_surface(&p, theme, &format!("{s:?}"), p.status(s), AA_TEXT);
            }
        }
    }

    #[test]
    fn status_and_tool_icons_stand_out_on_their_chips() {
        for (theme, p) in THEMES {
            let colors = Status::ALL
                .iter()
                .map(|s| (format!("{s:?}"), p.status(*s)))
                .chain(
                    ToolGroup::ALL
                        .iter()
                        .map(|g| (format!("{g:?}"), p.tool(*g))),
                );
            for (name, color) in colors {
                for (bg_name, bg) in surfaces(&p) {
                    let ratio = contrast_ratio(color, p.chip(color, bg));
                    assert!(
                        ratio >= NON_TEXT,
                        "{theme} {name} icon on its chip over {bg_name} is {ratio:.2}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn primary_label_reads_on_its_fill_and_the_fill_stands_out() {
        for (theme, p) in THEMES {
            let label = contrast_ratio(p.on_primary, p.primary);
            assert!(label >= AA_TEXT, "{theme} primary label is {label:.2}:1");
            assert_on_every_surface(&p, theme, "primary fill", p.primary, NON_TEXT);
        }
    }

    #[test]
    fn mark_lanes_step_down_from_the_outer_arc() {
        for (theme, p) in THEMES {
            for (bg_name, bg) in [("bg", p.bg), ("surface", p.surface)] {
                let [outer, middle, inner] =
                    [p.mark.outer, p.mark.middle, p.mark.inner].map(|c| contrast_ratio(c, bg));
                assert!(
                    outer > middle && middle > inner && inner >= NON_TEXT,
                    "{theme} mark on {bg_name}: {outer:.2} {middle:.2} {inner:.2}"
                );
                assert!(contrast_ratio(p.mark.core, bg) >= outer, "{theme} core");
            }
        }
    }

    #[test]
    fn themes_are_what_they_say() {
        assert!(DARK.is_dark() && !LIGHT.is_dark());
        assert_eq!(Palette::for_theme(Theme::Light), LIGHT);
        assert!(relative_luminance(DARK.bg) < relative_luminance(DARK.text));
        assert!(relative_luminance(LIGHT.bg) > relative_luminance(LIGHT.text));
    }

    #[test]
    fn tools_are_grouped_by_name() {
        let cases: &[(&str, ToolGroup)] = &[
            ("read", ToolGroup::Read),
            ("grep", ToolGroup::Read),
            ("Edit", ToolGroup::Edit),
            ("apply_patch", ToolGroup::Edit),
            ("bash", ToolGroup::Shell),
            ("eval", ToolGroup::Shell),
            ("web_fetch", ToolGroup::Web),
            ("mcp__tracker__create", ToolGroup::Mcp),
            ("task", ToolGroup::Agent),
            ("brand_new_tool", ToolGroup::Other),
            ("", ToolGroup::Other),
        ];
        for (tool, group) in cases {
            assert_eq!(ToolGroup::of(tool), *group, "{tool:?}");
        }
    }

    #[test]
    fn the_enum_lists_match_the_color_tables() {
        for (i, s) in Status::ALL.iter().enumerate() {
            assert_eq!(*s as usize, i, "{s:?}");
        }
        for (i, g) in ToolGroup::ALL.iter().enumerate() {
            assert_eq!(*g as usize, i, "{g:?}");
        }
    }
}
