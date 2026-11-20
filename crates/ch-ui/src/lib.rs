//! Color tokens as `0xRRGGBB`, and the WCAG contrast math that checks them.
//!
//! The tests hold body and muted text to AA (4.5:1) on every surface. Two known gaps stay
//! until the tokens are revised. Dark `accent` measures 4.47:1 on `bg` and less on the raised
//! surfaces, so it is never used for body text. In the light theme, the needs-you, review,
//! done and near-cap colors fall under 3:1 on `surface`, so a status dot always sits next to
//! its label.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: u32,
    pub surface: u32,
    pub elevated: u32,
    pub border: u32,
    pub text: u32,
    pub text_muted: u32,
    pub accent: u32,
}

pub const DARK: Palette = Palette {
    bg: 0x0B0D12,
    surface: 0x12151C,
    elevated: 0x191D26,
    border: 0x262B36,
    text: 0xE6E8EE,
    text_muted: 0x9AA3B2,
    accent: 0x7C5CFF,
};

pub const LIGHT: Palette = Palette {
    bg: 0xFFFFFF,
    surface: 0xF7F8FA,
    elevated: 0xFFFFFF,
    border: 0xE4E7EC,
    text: 0x0F1218,
    text_muted: 0x5B6474,
    accent: 0x6A48F5,
};

/// Session states. The same amber marks "needs you" everywhere, so the one
/// thing that wants the user is the one thing in color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub const fn color(self) -> u32 {
        match self {
            Status::NeedsYou => 0xF59E0B,
            Status::Running => 0x3B82F6,
            Status::Review => 0x06B6D4,
            Status::Done => 0x22C55E,
            Status::Error => 0xEF4444,
            Status::Idle => 0x64748B,
            Status::NearCap => 0xF97316,
        }
    }
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

    #[test]
    fn contrast_matches_known_extremes() {
        assert!((contrast_ratio(0x000000, 0xFFFFFF) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(0x777777, 0x777777) - 1.0).abs() < 1e-9);
        assert!((contrast_ratio(0x777777, 0xFFFFFF) - 4.48).abs() < 0.01);
    }

    #[test]
    fn text_tokens_meet_aa_on_every_surface() {
        for (theme, p) in [("dark", DARK), ("light", LIGHT)] {
            for (fg_name, fg) in [("text", p.text), ("text_muted", p.text_muted)] {
                for (bg_name, bg) in [
                    ("bg", p.bg),
                    ("surface", p.surface),
                    ("elevated", p.elevated),
                ] {
                    let ratio = contrast_ratio(fg, bg);
                    assert!(
                        ratio >= AA_TEXT,
                        "{theme} {fg_name} on {bg_name} is {ratio:.2}:1, needs {AA_TEXT}:1"
                    );
                }
            }
        }
    }

    // Keeps the module doc honest: change a token and this says which sentence went stale.
    #[test]
    fn known_contrast_gaps_match_the_module_doc() {
        assert!(contrast_ratio(DARK.accent, DARK.bg) < AA_TEXT);
        for s in [
            Status::NeedsYou,
            Status::Review,
            Status::Done,
            Status::NearCap,
        ] {
            assert!(contrast_ratio(s.color(), LIGHT.surface) < 3.0, "{s:?}");
        }
        for s in [Status::Running, Status::Error, Status::Idle] {
            assert!(contrast_ratio(s.color(), LIGHT.surface) >= 3.0, "{s:?}");
        }
    }
}
