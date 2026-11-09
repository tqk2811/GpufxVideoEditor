//! Where the panels go for a window size and the sizes the user dragged. A panel with no dragged
//! size follows the window. The sizes shrink towards their minimums in turn, so no panel overlaps
//! another at any size.

use egui::{Pos2, Rect, Vec2};

pub const GAP: f32 = 6.0;
pub const TOP_BAR_HEIGHT: f32 = 46.0;
pub const STATUS_HEIGHT: f32 = 22.0;

const LEFT_SHARE: f32 = 0.22;
const LEFT_MIN: f32 = 180.0;
const LEFT_MAX: f32 = 420.0;
const RIGHT_SHARE: f32 = 0.2;
const RIGHT_MIN: f32 = 180.0;
const RIGHT_MAX: f32 = 380.0;
const CENTER_MIN: f32 = 240.0;
const TIMELINE_SHARE: f32 = 0.36;
const TIMELINE_MIN: f32 = 180.0;
const MAIN_MIN: f32 = 160.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Panels {
    pub top_bar: Rect,
    pub left: Rect,
    pub center: Rect,
    pub right: Rect,
    pub timeline: Rect,
    pub status: Rect,
}

/// The sizes the user dragged, in points. `None` follows the window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PanelSizes {
    pub left: Option<f32>,
    pub right: Option<f32>,
    pub timeline: Option<f32>,
}

/// A bar between panels that resizes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Splitter {
    Left,
    Right,
    Timeline,
}

impl Splitter {
    pub const ALL: [Self; 3] = [Self::Left, Self::Right, Self::Timeline];

    pub fn is_vertical_bar(self) -> bool {
        !matches!(self, Self::Timeline)
    }

    /// The size this bar sets, as `panels` laid it out.
    pub fn size_in(self, panels: &Panels) -> f32 {
        match self {
            Self::Left => panels.left.width(),
            Self::Right => panels.right.width(),
            Self::Timeline => panels.timeline.height(),
        }
    }

    /// How much the panel grows when the pointer moves by `delta` from where the drag began.
    pub fn growth(self, delta: Vec2) -> f32 {
        match self {
            Self::Left => delta.x,
            Self::Right => -delta.x,
            Self::Timeline => -delta.y,
        }
    }

    /// The gap between the panels this bar sits in.
    pub fn rect_in(self, panels: &Panels) -> Rect {
        match self {
            Self::Left => Rect::from_min_max(Pos2::new(panels.left.right(), panels.left.top()), Pos2::new(panels.center.left(), panels.left.bottom())),
            Self::Right => Rect::from_min_max(Pos2::new(panels.center.right(), panels.right.top()), Pos2::new(panels.right.left(), panels.right.bottom())),
            Self::Timeline => Rect::from_min_max(Pos2::new(panels.timeline.left(), panels.center.bottom()), Pos2::new(panels.timeline.right(), panels.timeline.top())),
        }
    }
}

/// A splitter being dragged: where it started, and what to go back to if the drag is cancelled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitterDrag {
    pub splitter: Splitter,
    pub start_size: f32,
    pub before: PanelSizes,
    /// What the drag starts from; see [`PanelSizes::frozen_for_drag`].
    pub frozen: PanelSizes,
}

fn body_bounds(height: f32) -> (f32, f32) {
    let top = TOP_BAR_HEIGHT.min(height);
    let status_top = (height - STATUS_HEIGHT).max(top);
    (top, (status_top - GAP).max(top))
}

fn columns_width(width: f32) -> f32 {
    (width - 4.0 * GAP).max(0.0)
}

impl PanelSizes {
    /// These sizes with the side panel across from `splitter` held at the size it is displayed at
    /// in `laid_out`. A saved size too big for this window would otherwise come back as the drag
    /// makes room, and that panel would jump. A panel following the window stays that way.
    pub fn frozen_for_drag(self, splitter: Splitter, laid_out: &Panels) -> Self {
        let mut frozen = self;
        match splitter {
            Splitter::Left if self.right.is_some() => frozen.right = Some(laid_out.right.width()),
            Splitter::Right if self.left.is_some() => frozen.left = Some(laid_out.left.width()),
            _ => {}
        }
        frozen
    }

    /// These sizes with the panel of `splitter` at `wanted`, kept inside its limits in a window of
    /// `size` with the other panels as they are, so the bar stops where the panel can go no further.
    pub fn dragged_to(self, size: Vec2, splitter: Splitter, wanted: f32) -> Self {
        let laid_out = panels(size, &self);
        let room_beside = |other: f32| columns_width(size.x) - CENTER_MIN - other;
        let mut next = self;
        match splitter {
            Splitter::Left => next.left = Some(wanted.min(room_beside(laid_out.right.width())).min(LEFT_MAX).max(LEFT_MIN)),
            Splitter::Right => next.right = Some(wanted.min(room_beside(laid_out.left.width())).min(RIGHT_MAX).max(RIGHT_MIN)),
            Splitter::Timeline => {
                let (top, bottom) = body_bounds(size.y);
                next.timeline = Some(wanted.min(bottom - top - MAIN_MIN - GAP).max(TIMELINE_MIN));
            }
        }
        next
    }
}

/// Divides a window of `size` points.
pub fn panels(size: Vec2, sizes: &PanelSizes) -> Panels {
    let width = size.x.max(0.0);
    let height = size.y.max(0.0);
    let top_bar = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, TOP_BAR_HEIGHT.min(height)));
    let status_top = (height - STATUS_HEIGHT).max(top_bar.bottom());
    let status = Rect::from_min_max(Pos2::new(0.0, status_top), Pos2::new(width, height));

    let (body_top, body_bottom) = body_bounds(height);
    let body_height = body_bottom - body_top;
    let timeline_height = sizes.timeline.unwrap_or(body_height * TIMELINE_SHARE).max(TIMELINE_MIN).min((body_height - MAIN_MIN - GAP).max(0.0));
    let timeline_top = body_bottom - timeline_height;
    let main_bottom = (timeline_top - GAP).max(body_top);

    let columns_width = columns_width(width);
    let mut left_width = sizes.left.unwrap_or(width * LEFT_SHARE).clamp(LEFT_MIN, LEFT_MAX);
    let mut right_width = sizes.right.unwrap_or(width * RIGHT_SHARE).clamp(RIGHT_MIN, RIGHT_MAX);
    let center_room = columns_width - left_width - right_width;
    if center_room < CENTER_MIN {
        // The sides first give up what they have above their minimums, each in proportion to it.
        // Only in a tiny window do they go below, then evenly.
        let excess = CENTER_MIN - center_room;
        let (left_spare, right_spare) = (left_width - LEFT_MIN, right_width - RIGHT_MIN);
        let spare = left_spare + right_spare;
        let from_spare = excess.min(spare);
        if spare > 0.0 {
            left_width -= from_spare * left_spare / spare;
            right_width -= from_spare * right_spare / spare;
        }
        let rest = (excess - from_spare) / 2.0;
        left_width = (left_width - rest).max(0.0);
        right_width = (right_width - rest).max(0.0);
    }
    left_width = left_width.min(columns_width);
    right_width = right_width.min((columns_width - left_width).max(0.0));

    let left = Rect::from_min_max(Pos2::new(GAP, body_top), Pos2::new(GAP + left_width, main_bottom));
    let right = Rect::from_min_max(Pos2::new(width - GAP - right_width, body_top), Pos2::new(width - GAP, main_bottom));
    let center = Rect::from_min_max(Pos2::new(left.right() + GAP, body_top), Pos2::new((right.left() - GAP).max(left.right() + GAP), main_bottom));
    let timeline = Rect::from_min_max(Pos2::new(GAP, timeline_top), Pos2::new(width - GAP, body_bottom));
    Panels { top_bar, left, center, right, timeline, status }
}

/// How many thumbnails fit in a row `width` wide with cells at least `cell` wide.
pub fn grid_columns(width: f32, cell: f32, spacing: f32) -> usize {
    (((width + spacing) / (cell + spacing)).floor() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panels(size: Vec2) -> Panels {
        super::panels(size, &PanelSizes::default())
    }

    fn assert_disjoint(panels: &Panels) {
        let all = [panels.top_bar, panels.left, panels.center, panels.right, panels.timeline, panels.status];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate().skip(i + 1) {
                assert!(!a.shrink(0.25).intersects(b.shrink(0.25)), "panels {i} and {j} overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn no_panel_overlaps_another_at_any_window_size() {
        for size in [(1920.0, 1080.0), (1280.0, 720.0), (1000.0, 600.0), (640.0, 400.0), (300.0, 200.0), (50.0, 30.0)] {
            assert_disjoint(&panels(Vec2::new(size.0, size.1)));
        }
    }

    #[test]
    fn the_usual_window_gives_every_panel_a_working_size() {
        let p = panels(Vec2::new(1280.0, 720.0));
        assert!(p.left.width() >= 240.0 && p.right.width() >= 220.0, "{p:?}");
        assert!(p.center.width() >= 500.0, "{p:?}");
        assert!(p.timeline.height() >= 180.0 && p.center.height() >= 200.0, "{p:?}");
        assert_eq!(p.left.right() + GAP, p.center.left());
        assert_eq!(p.center.right() + GAP, p.right.left());
        assert_eq!(p.center.bottom() + GAP, p.timeline.top());
    }

    #[test]
    fn the_centre_keeps_its_minimum_when_the_window_narrows() {
        let p = panels(Vec2::new(560.0, 600.0));
        assert!(p.center.width() >= CENTER_MIN - 1.0, "{p:?}");
    }

    #[test]
    fn side_panels_stop_growing_at_their_maximum() {
        let p = panels(Vec2::new(3840.0, 2160.0));
        assert_eq!(p.left.width(), LEFT_MAX);
        assert_eq!(p.right.width(), RIGHT_MAX);
    }

    #[test]
    fn a_short_window_keeps_the_timeline_usable() {
        let p = panels(Vec2::new(1280.0, 500.0));
        assert!(p.timeline.height() >= TIMELINE_MIN - 1.0, "{p:?}");
        assert!(p.center.height() >= 100.0, "{p:?}");
    }

    #[test]
    fn a_dragged_size_is_used_and_the_others_still_follow_the_window() {
        let window = Vec2::new(1280.0, 720.0);
        let sizes = PanelSizes { left: Some(300.0), right: None, timeline: Some(250.0) };
        let p = super::panels(window, &sizes);
        assert_eq!(p.left.width(), 300.0);
        assert_eq!(p.timeline.height(), 250.0);
        assert_eq!(p.right.width(), panels(window).right.width());
    }

    #[test]
    fn a_drag_stops_at_the_limits_and_nothing_collapses_at_the_smallest_window() {
        let window = Vec2::new(800.0, 500.0);
        for splitter in Splitter::ALL {
            for wanted in [-500.0, 0.0, 200.0, 5000.0] {
                let sizes = PanelSizes::default().dragged_to(window, splitter, wanted);
                let p = super::panels(window, &sizes);
                assert_disjoint(&p);
                assert!(p.left.width() >= LEFT_MIN && p.right.width() >= RIGHT_MIN, "{splitter:?} {wanted} {p:?}");
                assert!(p.center.width() >= CENTER_MIN - 0.5, "{splitter:?} {wanted} {p:?}");
                assert!(p.timeline.height() >= TIMELINE_MIN && p.center.height() >= MAIN_MIN - 0.5, "{splitter:?} {wanted} {p:?}");
            }
        }
    }

    #[test]
    fn saved_sizes_too_big_for_the_window_give_way_above_the_minimums_first() {
        let sizes = PanelSizes { left: Some(420.0), right: Some(180.0), timeline: Some(9000.0) };
        let p = super::panels(Vec2::new(800.0, 500.0), &sizes);
        assert_disjoint(&p);
        assert_eq!(p.right.width(), RIGHT_MIN, "the panel at its minimum gives nothing");
        assert!(p.left.width() >= LEFT_MIN && p.center.width() >= CENTER_MIN - 0.5, "{p:?}");
        assert!(p.center.height() >= MAIN_MIN - 0.5, "{p:?}");
    }

    #[test]
    fn each_splitter_sits_in_its_gap() {
        let p = panels(Vec2::new(1280.0, 720.0));
        assert_eq!(Splitter::Left.rect_in(&p).width(), GAP);
        assert_eq!(Splitter::Right.rect_in(&p).left(), p.center.right());
        assert_eq!(Splitter::Timeline.rect_in(&p).height(), GAP);
    }

    #[test]
    fn the_grid_fits_whole_cells_only() {
        assert_eq!(grid_columns(400.0, 120.0, 8.0), 3);
        assert_eq!(grid_columns(120.0, 120.0, 8.0), 1);
        assert_eq!(grid_columns(30.0, 120.0, 8.0), 1, "never no column");
    }
}
