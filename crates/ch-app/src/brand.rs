//! The C-Harness mark: three arcs closing on a core. `svg()` paints one color per file, so the
//! mark is drawn as one lane per tone, stacked in the same box.

use ch_ui::Palette;
use gpui_kit::{Div, FontWeight, IntoElement, ParentElement, Pixels, Styled, div, px, rgb, svg};

/// At or below this size the three arcs blur together, so the two-arc compact mark is drawn.
const COMPACT_MAX: f32 = 32.0;

fn lanes(p: Palette, size: Pixels) -> Vec<(&'static str, u32)> {
    if size <= px(COMPACT_MAX) {
        vec![
            ("brand/compact-outer.svg", p.mark.outer),
            ("brand/compact-inner.svg", p.mark.middle),
            ("brand/compact-core.svg", p.mark.core),
        ]
    } else {
        vec![
            ("brand/outer.svg", p.mark.outer),
            ("brand/middle.svg", p.mark.middle),
            ("brand/inner.svg", p.mark.inner),
            ("brand/core.svg", p.mark.core),
        ]
    }
}

pub fn mark(p: Palette, size: Pixels) -> Div {
    div()
        .relative()
        .flex_none()
        .size(size)
        .children(lanes(p, size).into_iter().map(move |(path, color)| {
            svg()
                .path(path)
                .absolute()
                .top_0()
                .left_0()
                .size(size)
                .text_color(rgb(color))
        }))
}

/// The mark next to the product name, for the sidebar header.
pub fn lockup(p: Palette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(mark(p, px(20.0)))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(p.text))
                .child("C-Harness"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Assets;
    use gpui_kit::AssetSource;

    /// The part of a lane file that says where it draws: a path's `d` or a circle's radius.
    fn geometry(svg: &str) -> String {
        let key = if svg.contains("<path") {
            "d=\""
        } else {
            "r=\""
        };
        let start = svg.find(key).expect("lane has geometry") + key.len();
        let end = start + svg[start..].find('"').expect("closed attribute");
        format!("{key}{}\"", &svg[start..end])
    }

    fn lane(path: &str) -> String {
        let bytes = Assets.load(path).ok().flatten().expect(path);
        String::from_utf8(bytes.into_owned()).expect("utf-8")
    }

    #[test]
    fn lanes_redraw_the_published_marks() {
        let full = include_str!("../../../assets/brand/mark-on-dark.svg");
        let compact = include_str!("../../../assets/brand/mark-compact.svg");
        for (size, whole) in [(px(64.0), full), (px(16.0), compact)] {
            for (path, _) in lanes(ch_ui::DARK, size) {
                let shape = geometry(&lane(path));
                assert!(
                    whole.contains(&shape),
                    "{path}: {shape} is not in the published mark"
                );
            }
        }
    }

    #[test]
    fn small_sizes_use_the_compact_mark() {
        assert_eq!(lanes(ch_ui::DARK, px(16.0)).len(), 3);
        assert_eq!(lanes(ch_ui::DARK, px(32.0)).len(), 3);
        assert_eq!(lanes(ch_ui::DARK, px(48.0)).len(), 4);
    }
}
