//! Every SVG the window draws: Lucide icons, provider logos and the brand mark. They are
//! compiled into the binary, so nothing is read from disk at run time.
//!
//! `svg()` paints a file as a mask in the element's text color, so each file is one color and
//! the brand mark is drawn as stacked lanes.

use std::borrow::Cow;

use ch_ui::{Palette, Status, ToolGroup};
use gpui_kit::{AssetSource, Pixels, Result, SharedString, Styled, Svg, rgb, svg};

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../assets/", $path)) as &[u8])),*]
    };
}

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// A Lucide icon. `assets/icons/LICENSE` carries Lucide's ISC license.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon {
            $($variant),*
        }

        impl Icon {
            pub const fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => concat!("icons/", $file, ".svg")),*
                }
            }
        }

        const ICON_FILES: &[(&str, &[u8])] = &[$((
            concat!("icons/", $file, ".svg"),
            include_bytes!(concat!("../assets/icons/", $file, ".svg")) as &[u8],
        )),*];
    };
}

icons! {
    ArrowUp => "arrow-up",
    Bot => "bot",
    ChevronDown => "chevron-down",
    CircleAlert => "circle-alert",
    CircleCheck => "circle-check",
    CircleDashed => "circle-dashed",
    CircleDot => "circle-dot",
    CircleX => "circle-x",
    Download => "download",
    ExternalLink => "external-link",
    Eye => "eye",
    FileDiff => "file-diff",
    FilePen => "file-pen",
    FolderGit => "folder-git-2",
    Gauge => "gauge",
    GitBranch => "git-branch",
    GitCommit => "git-commit-horizontal",
    GitPullRequest => "git-pull-request",
    Globe => "globe",
    ListChecks => "list-checks",
    LoaderCircle => "loader-circle",
    Monitor => "monitor",
    Moon => "moon",
    PanelRight => "panel-right",
    Plug => "plug",
    Plus => "plus",
    Settings => "settings",
    ShieldAlert => "shield-alert",
    ShieldCheck => "shield-check",
    Sparkles => "sparkles",
    Square => "square",
    SquareTerminal => "square-terminal",
    Sun => "sun",
    X => "x",
}

/// Logos for the providers omp knows a key variable for. `assets/providers/LICENSE` says where
/// they come from and whose trademarks they are.
const PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "providers/anthropic.svg"),
    ("cerebras", "providers/cerebras.svg"),
    ("google", "providers/google.svg"),
    ("groq", "providers/groq.svg"),
    ("mistral", "providers/mistral.svg"),
    ("openai", "providers/openai.svg"),
    ("openrouter", "providers/openrouter.svg"),
    ("xai", "providers/xai.svg"),
    ("zai", "providers/zai.svg"),
];

const OTHER_FILES: &[(&str, &[u8])] = files![
    "providers/anthropic.svg",
    "providers/cerebras.svg",
    "providers/google.svg",
    "providers/groq.svg",
    "providers/mistral.svg",
    "providers/openai.svg",
    "providers/openrouter.svg",
    "providers/xai.svg",
    "providers/zai.svg",
    "brand/outer.svg",
    "brand/middle.svg",
    "brand/inner.svg",
    "brand/core.svg",
    "brand/compact-outer.svg",
    "brand/compact-inner.svg",
    "brand/compact-core.svg",
];

/// The app's asset source, registered once at startup.
pub struct Assets;

impl Assets {
    fn files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
        ICON_FILES.iter().chain(OTHER_FILES)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::files()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::files()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

/// An icon in `color`, sized to `size`.
pub fn icon(icon: Icon, size: Pixels, color: u32) -> Svg {
    svg()
        .path(icon.path())
        .flex_none()
        .size(size)
        .text_color(rgb(color))
}

/// A provider's logo in `color`, or `None` for a provider without one.
pub fn provider_logo(provider: &str, size: Pixels, color: u32) -> Option<Svg> {
    let provider = provider.trim().to_ascii_lowercase();
    PROVIDERS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, path)| {
            svg()
                .path(*path)
                .flex_none()
                .size(size)
                .text_color(rgb(color))
        })
}

pub const fn status_icon(status: Status) -> Icon {
    match status {
        Status::NeedsYou => Icon::CircleAlert,
        Status::Running => Icon::LoaderCircle,
        Status::Review => Icon::CircleDot,
        Status::Done => Icon::CircleCheck,
        Status::Error => Icon::CircleX,
        Status::Idle => Icon::CircleDashed,
        Status::NearCap => Icon::Gauge,
    }
}

pub const fn tool_icon(group: ToolGroup) -> Icon {
    match group {
        ToolGroup::Read => Icon::Eye,
        ToolGroup::Edit => Icon::FilePen,
        ToolGroup::Shell => Icon::SquareTerminal,
        ToolGroup::Web => Icon::Globe,
        ToolGroup::Mcp => Icon::Plug,
        ToolGroup::Agent => Icon::Bot,
        ToolGroup::Other => Icon::Sparkles,
    }
}

/// A status icon in that status's color for the current theme.
pub fn status_glyph(p: Palette, status: Status, size: Pixels) -> Svg {
    icon(status_icon(status), size, p.status(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loads(path: &str) -> bool {
        Assets
            .load(path)
            .ok()
            .flatten()
            .is_some_and(|bytes| bytes.starts_with(b"<svg"))
    }

    #[test]
    fn every_status_and_tool_icon_is_embedded() {
        for status in Status::ALL {
            let path = status_icon(status).path();
            assert!(loads(path), "{status:?}: {path}");
        }
        for group in ToolGroup::ALL {
            let path = tool_icon(group).path();
            assert!(loads(path), "{group:?}: {path}");
        }
    }

    #[test]
    fn every_known_provider_has_a_logo() {
        for (provider, path) in PROVIDERS {
            assert!(loads(path), "{provider}: {path}");
            assert!(
                crate::settings::key_env_for(provider).is_some(),
                "{provider}"
            );
        }
        assert!(provider_logo(" OpenAI ", gpui_kit::px(16.0), 0).is_some());
        assert!(provider_logo("someone-new", gpui_kit::px(16.0), 0).is_none());
    }

    #[test]
    fn unknown_paths_load_nothing() {
        assert!(!loads("icons/not-an-icon.svg"));
        assert!(!loads("../Cargo.toml"));
    }

    #[test]
    fn every_svg_is_one_mask_color() {
        for (path, bytes) in Assets::files() {
            let text = std::str::from_utf8(bytes).unwrap_or_default();
            assert!(
                text.contains("currentColor"),
                "{path} should paint in currentColor"
            );
            assert!(!text.contains('#'), "{path} hard-codes a color");
        }
    }
}
