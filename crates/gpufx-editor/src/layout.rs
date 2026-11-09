//! Where the panels go for a window size. The sizes shrink towards their minimums in turn, so no
//! panel overlaps another at any size.

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

fn clamp_low(value: f32, low: f32) -> f32 {
    value.max(low)
}

/// Divides a window of `size` points.
pub fn panels(size: Vec2) -> Panels {
    let width = size.x.max(0.0);
    let height = size.y.max(0.0);
    let top_bar = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, TOP_BAR_HEIGHT.min(height)));
    let status_top = (height - STATUS_HEIGHT).max(top_bar.bottom());
    let status = Rect::from_min_max(Pos2::new(0.0, status_top), Pos2::new(width, height));

    let body_top = top_bar.bottom();
    let body_bottom = (status.top() - GAP).max(body_top);
    let body_height = body_bottom - body_top;
    let timeline_height = (body_height * TIMELINE_SHARE).max(TIMELINE_MIN).min((body_height - MAIN_MIN - GAP).max(0.0));
    let timeline_top = body_bottom - timeline_height;
    let main_bottom = (timeline_top - GAP).max(body_top);

    let inner_width = (width - 2.0 * GAP).max(0.0);
    let columns_width = (inner_width - 2.0 * GAP).max(0.0);
    let mut left_width = (width * LEFT_SHARE).clamp(LEFT_MIN, LEFT_MAX);
    let mut right_width = (width * RIGHT_SHARE).clamp(RIGHT_MIN, RIGHT_MAX);
    let center_room = columns_width - left_width - right_width;
    if center_room < CENTER_MIN {
        // The sides give way evenly, and below their minimums only when the window is tiny.
        let excess = CENTER_MIN - center_room;
        left_width = clamp_low(left_width - excess / 2.0, 0.0);
        right_width = clamp_low(right_width - excess / 2.0, 0.0);
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
    fn the_grid_fits_whole_cells_only() {
        assert_eq!(grid_columns(400.0, 120.0, 8.0), 3);
        assert_eq!(grid_columns(120.0, 120.0, 8.0), 1);
        assert_eq!(grid_columns(30.0, 120.0, 8.0), 1, "never no column");
    }
}
