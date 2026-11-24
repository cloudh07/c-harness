//! Scroll anchoring for the thread. Rows register while the thread lays out; before the scroll
//! area places its content, the offset moves by however far the row being read moved since the
//! last frame, so output growing or trimmed above the reader leaves them where they were.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, ScrollHandle, SharedString, Window, point, px,
};

/// Where a row sat, measured from the top of the scroll area as if it were not scrolled.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub(super) key: SharedString,
    pub(super) top: f32,
    pub(super) bottom: f32,
}

#[derive(Default)]
struct Frame {
    laid_out: Vec<(SharedString, LayoutId)>,
    last: Vec<Row>,
}

/// Shared by the scroll area and the rows inside it.
#[derive(Clone, Default)]
pub(super) struct Anchor(Rc<RefCell<Frame>>);

impl Anchor {
    /// A row the scroll area can hold still. Keys must stay the same across frames and be
    /// unique within one.
    pub(super) fn row(&self, key: impl Into<SharedString>, child: impl IntoElement) -> Keyed {
        Keyed {
            key: key.into(),
            anchor: self.clone(),
            child: child.into_any_element(),
        }
    }

    /// The scroll area. With `hold` off it only records where the rows are.
    pub(super) fn area(
        &self,
        scroll: &ScrollHandle,
        hold: bool,
        child: impl IntoElement,
    ) -> Anchored {
        Anchored {
            anchor: self.clone(),
            scroll: scroll.clone(),
            hold,
            child: child.into_any_element(),
        }
    }
}

/// The scroll offset that keeps the row being read where it was on screen. GPUI's offset is
/// zero at the top and goes negative as the view scrolls down, so the reader's place in the
/// content is `-offset_y`, and a row that moved down by some amount needs the offset lowered
/// by the same amount.
pub(super) fn held_offset(offset_y: f32, last: &[Row], now: &[Row]) -> f32 {
    offset_y - moved(last, now, -offset_y)
}

/// How far the row being read moved between two frames. That row is the first one still in
/// view below `view_top` that has no registered row inside it; a row that went away hands over
/// to the next one down. Zero when none of them is left.
fn moved(last: &[Row], now: &[Row], view_top: f32) -> f32 {
    let in_view: Vec<&Row> = last.iter().filter(|row| row.bottom > view_top).collect();
    let mut leaves: Vec<&Row> = in_view
        .iter()
        .copied()
        .filter(|row| !in_view.iter().any(|inner| inside(inner, row)))
        .collect();
    leaves.sort_by(|a, b| a.top.total_cmp(&b.top));
    leaves
        .into_iter()
        .find_map(|was| {
            now.iter()
                .find(|row| row.key == was.key)
                .map(|row| row.top - was.top)
        })
        .unwrap_or(0.0)
}

fn inside(inner: &Row, outer: &Row) -> bool {
    inner.key != outer.key && inner.top >= outer.top && inner.bottom <= outer.bottom
}

pub(super) struct Keyed {
    key: SharedString,
    anchor: Anchor,
    child: AnyElement,
}

impl IntoElement for Keyed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Keyed {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let layout = self.child.request_layout(window, cx);
        self.anchor
            .0
            .borrow_mut()
            .laid_out
            .push((self.key.clone(), layout));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

pub(super) struct Anchored {
    anchor: Anchor,
    scroll: ScrollHandle,
    hold: bool,
    child: AnyElement,
}

impl IntoElement for Anchored {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Anchored {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.anchor.0.borrow_mut().laid_out.clear();
        (self.child.request_layout(window, cx), ())
    }

    /// Runs before the scroll area applies its offset, so the rows are measured unscrolled and
    /// the corrected offset is the one this frame paints with.
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin = bounds.origin.y;
        {
            let mut frame = self.anchor.0.borrow_mut();
            let now: Vec<Row> = frame
                .laid_out
                .iter()
                .map(|(key, layout)| {
                    let row = window.layout_bounds(*layout);
                    Row {
                        key: key.clone(),
                        top: f32::from(row.origin.y - origin),
                        bottom: f32::from(row.bottom() - origin),
                    }
                })
                .collect();
            if self.hold {
                let offset = self.scroll.offset();
                let held = held_offset(f32::from(offset.y), &frame.last, &now);
                self.scroll.set_offset(point(offset.x, px(held)));
            }
            frame.last = now;
        }
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{Row, held_offset, moved};

    fn row(key: &'static str, top: f32, bottom: f32) -> Row {
        Row {
            key: key.into(),
            top,
            bottom,
        }
    }

    fn thread(reply: f32, tools: &[&'static str]) -> Vec<Row> {
        let mut rows = vec![row("turn-now", 0.0, reply)];
        let card_top = reply + 16.0;
        let mut top = card_top + 40.0;
        for key in tools {
            rows.push(row(key, top, top + 20.0));
            top += 28.0;
        }
        rows.push(row("tools", card_top, top + 12.0));
        rows.push(row("log", top + 28.0, top + 128.0));
        rows
    }

    #[test]
    fn growth_above_the_reader_moves_the_view_with_it() {
        let last = thread(300.0, &["tool-a", "tool-b"]);
        let now = thread(380.0, &["tool-a", "tool-b"]);
        assert_eq!(moved(&last, &now, 360.0), 80.0, "reading a tool row");
        assert_eq!(moved(&last, &now, 470.0), 80.0, "reading the log");
    }

    #[test]
    fn growth_below_the_reader_moves_nothing() {
        let last = thread(300.0, &["tool-a"]);
        let now = thread(380.0, &["tool-a"]);
        assert_eq!(moved(&last, &now, 100.0), 0.0);
    }

    #[test]
    fn a_gap_anchors_to_the_next_row_down() {
        let last = thread(300.0, &["tool-a", "tool-b"]);
        let now = thread(340.0, &["tool-a", "tool-b"]);
        assert_eq!(
            moved(&last, &now, 305.0),
            40.0,
            "between the reply and the card"
        );
        assert_eq!(moved(&last, &now, 378.0), 40.0, "between two tool rows");
    }

    #[test]
    fn a_trimmed_tool_list_keeps_the_row_being_read() {
        let last = thread(300.0, &["tool-a", "tool-b", "tool-c"]);
        let now = thread(300.0, &["tool-b", "tool-c"]);
        let at_c = last[3].top + 5.0;
        assert_eq!(moved(&last, &now, at_c), -28.0);
    }

    #[test]
    fn a_row_that_went_away_hands_over_to_the_next() {
        let last = thread(300.0, &["tool-a", "tool-b"]);
        let now = thread(300.0, &["tool-b"]);
        let at_a = last[1].top + 5.0;
        assert_eq!(moved(&last, &now, at_a), -28.0);
    }

    fn top_of(rows: &[Row], key: &str) -> f32 {
        rows.iter()
            .find(|row| row.key == key)
            .map(|row| row.top)
            .expect("a row with that key")
    }

    /// A row shows on screen at its place in the content plus the offset, so holding the
    /// reader means that sum stays the same while rows above them grow or go away.
    #[test]
    fn the_held_offset_keeps_the_row_being_read_still_on_screen() {
        let cases = [
            (
                "tool-b",
                thread(300.0, &["tool-a", "tool-b"]),
                thread(380.0, &["tool-a", "tool-b"]),
            ),
            (
                "tool-c",
                thread(300.0, &["tool-a", "tool-b", "tool-c"]),
                thread(300.0, &["tool-b", "tool-c"]),
            ),
        ];
        for (key, last, now) in cases {
            let offset_y = -(top_of(&last, key) + 5.0);
            let held = held_offset(offset_y, &last, &now);
            assert_eq!(
                top_of(&now, key) + held,
                top_of(&last, key) + offset_y,
                "{key}"
            );
        }
    }

    #[test]
    fn nothing_to_hold_moves_nothing() {
        let last = thread(300.0, &["tool-a"]);
        assert_eq!(moved(&last, &[], 360.0), 0.0);
        assert_eq!(moved(&[], &last, 360.0), 0.0);
        assert_eq!(moved(&last, &last, 9_999.0), 0.0, "below every row");
    }
}
