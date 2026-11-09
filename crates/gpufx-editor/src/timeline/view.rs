//! Conversion between timeline time and points, zoom and scroll limits.
//!
//! X coordinates are relative to the left edge of the track area, Y coordinates to its top.

use engine::{Project, Time, TICKS_PER_SECOND};

pub const MIN_PIXELS_PER_SECOND: f64 = 1.0;
pub const MAX_PIXELS_PER_SECOND: f64 = 2000.0;
pub const DEFAULT_PIXELS_PER_SECOND: f64 = 80.0;
pub const TRACK_HEIGHT: f64 = 52.0;
/// Empty space after the last clip, so a clip can be dragged past the end.
pub const END_PADDING: f64 = 200.0;
const ZOOM_SPEED: f64 = 0.0025;

/// What limits scrolling, measured on the current frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewArea {
    pub content_end: Time,
    pub width: f64,
    pub track_count: usize,
    pub rows_height: f64,
}

/// One wheel event, with its delta already in points. Positive deltas move the content right and
/// down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wheel {
    pub ctrl: bool,
    pub shift: bool,
    pub delta_x: f64,
    pub delta_y: f64,
    pub cursor_x: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineView {
    pixels_per_second: f64,
    scroll_x: f64,
    scroll_y: f64,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self { pixels_per_second: DEFAULT_PIXELS_PER_SECOND, scroll_x: 0.0, scroll_y: 0.0 }
    }
}

/// The end of the last clip of `project`, or zero when it has no clips.
pub fn content_end(project: &Project) -> Time {
    project
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.start.saturating_add(clip.duration))
        .max()
        .unwrap_or(Time::ZERO)
}

impl TimelineView {
    pub fn pixels_per_second(&self) -> f64 {
        self.pixels_per_second
    }

    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    pub fn scroll_y(&self) -> f64 {
        self.scroll_y
    }

    pub fn x_to_time(&self, x: f64) -> Time {
        Time::from_ticks(self.x_to_ticks(x).round() as i64)
    }

    pub fn time_to_x(&self, time: Time) -> f64 {
        time.ticks() as f64 / TICKS_PER_SECOND as f64 * self.pixels_per_second - self.scroll_x
    }

    fn x_to_ticks(&self, x: f64) -> f64 {
        (x + self.scroll_x) / self.pixels_per_second * TICKS_PER_SECOND as f64
    }

    pub fn max_scroll_x(&self, area: &ViewArea) -> f64 {
        let end = area.content_end.ticks() as f64 / TICKS_PER_SECOND as f64 * self.pixels_per_second;
        (end + END_PADDING - area.width).max(0.0)
    }

    pub fn max_scroll_y(&self, area: &ViewArea) -> f64 {
        (area.track_count as f64 * TRACK_HEIGHT - area.rows_height).max(0.0)
    }

    /// Brings both scroll positions inside their limits, for when the content has shrunk.
    pub fn clamp_scroll(&mut self, area: &ViewArea) {
        self.scroll_x = self.scroll_x.clamp(0.0, self.max_scroll_x(area));
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y(area));
    }

    /// Scrolls by `delta` points; true when the position changed.
    pub fn scroll_x_by(&mut self, delta: f64, area: &ViewArea) -> bool {
        let next = (self.scroll_x + delta).clamp(0.0, self.max_scroll_x(area));
        let changed = next != self.scroll_x;
        self.scroll_x = next;
        changed
    }

    /// Scrolls by `delta` points; true when the position changed.
    pub fn scroll_y_by(&mut self, delta: f64, area: &ViewArea) -> bool {
        let next = (self.scroll_y + delta).clamp(0.0, self.max_scroll_y(area));
        let changed = next != self.scroll_y;
        self.scroll_y = next;
        changed
    }

    /// Multiplies the zoom by `factor`, keeping the time under `cursor_x` in place unless a scroll
    /// limit prevents it. True when the zoom changed.
    pub fn zoom_at(&mut self, factor: f64, cursor_x: f64, area: &ViewArea) -> bool {
        let next = (self.pixels_per_second * factor).clamp(MIN_PIXELS_PER_SECOND, MAX_PIXELS_PER_SECOND);
        if next == self.pixels_per_second {
            return false;
        }
        let ticks = self.x_to_ticks(cursor_x);
        self.pixels_per_second = next;
        self.scroll_x = ticks / TICKS_PER_SECOND as f64 * next - cursor_x;
        self.clamp_scroll(area);
        true
    }

    /// Applies a wheel event; true when it did something, so the caller may consume it. An event
    /// that can do nothing here returns false and must be left for others.
    pub fn apply_wheel(&mut self, wheel: Wheel, area: &ViewArea) -> bool {
        if wheel.ctrl {
            let factor = ((wheel.delta_x + wheel.delta_y) * ZOOM_SPEED).exp();
            return self.zoom_at(factor, wheel.cursor_x.max(0.0), area);
        }
        if wheel.shift {
            let amount = if wheel.delta_y != 0.0 { wheel.delta_y } else { wheel.delta_x };
            return self.scroll_x_by(-amount, area);
        }
        // A diagonal swipe goes along the axis it mostly follows, so a vertical one is not lost to
        // a small sideways drift when the view is at its left limit.
        if wheel.delta_x.abs() > wheel.delta_y.abs() {
            return self.scroll_x_by(-wheel.delta_x, area);
        }
        self.scroll_y_by(-wheel.delta_y, area)
    }

    /// Top of the row of `track`. Track 0 is at the bottom, as in compositing order.
    pub fn track_top(&self, track: usize, track_count: usize) -> f64 {
        (track_count - 1 - track) as f64 * TRACK_HEIGHT - self.scroll_y
    }

    /// The track whose row contains `y`, or `None` when `y` is outside the visible area of
    /// `height` points, so a pointer over the ruler does not reach a track scrolled out of sight.
    pub fn track_at_y(&self, y: f64, track_count: usize, height: f64) -> Option<usize> {
        if y < 0.0 || y >= height {
            return None;
        }
        let row = ((y + self.scroll_y) / TRACK_HEIGHT).floor();
        if row < 0.0 || row >= track_count as f64 {
            return None;
        }
        Some(track_count - 1 - row as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZOOMS: [f64; 5] = [1.0, 7.5, 80.0, 613.0, 2000.0];

    fn area() -> ViewArea {
        ViewArea { content_end: Time::from_ticks(3600 * TICKS_PER_SECOND), width: 1000.0, track_count: 3, rows_height: 100.0 }
    }

    fn view(pixels_per_second: f64, scroll_x: f64) -> TimelineView {
        TimelineView { pixels_per_second, scroll_x, scroll_y: 0.0 }
    }

    #[test]
    fn time_and_x_round_trip_at_every_zoom() {
        for zoom in ZOOMS {
            let view = view(zoom, 137.5);
            for ticks in [0_i64, 1, 3003, 90_000, 123_456_789] {
                let time = Time::from_ticks(ticks);
                let back = view.x_to_time(view.time_to_x(time));
                assert_eq!(back, time, "zoom {zoom}");
            }
        }
    }

    #[test]
    fn x_to_time_matches_the_zoom_exactly() {
        assert_eq!(view(80.0, 0.0).x_to_time(80.0), Time::from_ticks(90_000));
        assert_eq!(view(80.0, 40.0).x_to_time(40.0), Time::from_ticks(90_000));
        assert_eq!(view(2000.0, 0.0).x_to_time(1.0), Time::from_ticks(45));
    }

    #[test]
    fn zoom_keeps_the_time_under_the_cursor() {
        let area = area();
        for zoom in ZOOMS {
            for cursor in [0.0, 333.0, 999.0] {
                for factor in [1.1, 2.5, 0.9, 0.4] {
                    let mut view = view(zoom, 1000.0 * zoom / 100.0);
                    let before = view.x_to_time(cursor);
                    if view.zoom_at(factor, cursor, &area) && view.scroll_x > 0.0 && view.scroll_x < view.max_scroll_x(&area) {
                        let after = view.x_to_time(cursor);
                        assert!((after.ticks() - before.ticks()).abs() <= 1, "zoom {zoom} factor {factor} cursor {cursor}: {before} became {after}");
                    }
                }
            }
        }
    }

    #[test]
    fn zoom_keeps_the_time_under_the_cursor_in_a_fixed_case() {
        let area = area();
        let mut view = view(100.0, 5000.0);
        let before = view.x_to_time(250.0);
        assert!(view.zoom_at(3.0, 250.0, &area));
        assert_eq!(view.pixels_per_second(), 300.0);
        assert_eq!(view.x_to_time(250.0), before);
    }

    #[test]
    fn zoom_stays_inside_its_limits() {
        let area = area();
        let mut view = view(MAX_PIXELS_PER_SECOND, 0.0);
        assert!(!view.zoom_at(2.0, 10.0, &area));
        let mut low = TimelineView { pixels_per_second: MIN_PIXELS_PER_SECOND, ..view.clone() };
        assert!(!low.zoom_at(0.5, 10.0, &area));
        assert!(view.zoom_at(0.5, 10.0, &area));
    }

    #[test]
    fn zooming_out_never_scrolls_past_the_limits() {
        let area = ViewArea { content_end: Time::from_seconds(10).unwrap(), ..area() };
        let mut view = view(1000.0, 9000.0);
        view.zoom_at(0.01, 500.0, &area);
        assert!(view.scroll_x() >= 0.0 && view.scroll_x() <= view.max_scroll_x(&area));
    }

    #[test]
    fn scroll_stops_at_both_ends() {
        let area = area();
        let mut view = view(1.0, 0.0);
        let max = view.max_scroll_x(&area);
        assert_eq!(max, 3600.0 + END_PADDING - 1000.0);
        assert!(!view.scroll_x_by(-10.0, &area));
        assert!(view.scroll_x_by(10.0, &area));
        assert!(view.scroll_x_by(1e9, &area));
        assert_eq!(view.scroll_x(), max);
        assert!(!view.scroll_x_by(1.0, &area));
    }

    #[test]
    fn scroll_is_clamped_after_the_content_shrinks() {
        let mut view = view(1.0, 2000.0);
        view.clamp_scroll(&ViewArea { content_end: Time::ZERO, ..area() });
        assert_eq!(view.scroll_x(), 0.0);
    }

    fn wheel(ctrl: bool, shift: bool, delta_y: f64) -> Wheel {
        Wheel { ctrl, shift, delta_x: 0.0, delta_y, cursor_x: 100.0 }
    }

    #[test]
    fn shift_wheel_is_not_consumed_when_nothing_can_scroll() {
        let area = ViewArea { content_end: Time::ZERO, ..area() };
        let mut view = view(80.0, 0.0);
        assert!(!view.apply_wheel(wheel(false, true, -50.0), &area));
        assert!(!view.apply_wheel(wheel(false, true, 50.0), &area));
        assert_eq!(view.scroll_x(), 0.0);
    }

    #[test]
    fn shift_wheel_is_only_consumed_in_a_direction_that_can_scroll() {
        let area = area();
        let mut view = view(1.0, 0.0);
        assert!(!view.apply_wheel(wheel(false, true, 50.0), &area), "already at the start");
        assert!(view.apply_wheel(wheel(false, true, -50.0), &area));
        assert_eq!(view.scroll_x(), 50.0);
    }

    #[test]
    fn ctrl_wheel_zooms_and_a_limit_passes_the_event_on() {
        let area = area();
        let mut view = view(80.0, 0.0);
        assert!(view.apply_wheel(wheel(true, false, 50.0), &area));
        assert!(view.pixels_per_second() > 80.0);
        let mut top = TimelineView { pixels_per_second: MAX_PIXELS_PER_SECOND, ..view.clone() };
        assert!(!top.apply_wheel(wheel(true, false, 50.0), &area));
    }

    #[test]
    fn a_plain_wheel_scrolls_the_tracks_vertically_only_on_overflow() {
        let tall = area();
        let mut view = view(80.0, 0.0);
        assert!(view.apply_wheel(wheel(false, false, -30.0), &tall));
        assert_eq!(view.scroll_y(), 30.0);
        let fits = ViewArea { track_count: 1, ..tall };
        let mut other = TimelineView::default();
        assert!(!other.apply_wheel(wheel(false, false, -30.0), &fits));
    }

    #[test]
    fn a_pointer_outside_the_visible_rows_hits_no_track() {
        let view = TimelineView { pixels_per_second: 80.0, scroll_x: 0.0, scroll_y: TRACK_HEIGHT };
        // Over the ruler: y is negative, but y + scroll_y still lands in a row scrolled out of sight.
        assert_eq!(view.track_at_y(-10.0, 3, 100.0), None);
        assert_eq!(view.track_at_y(0.0, 3, 100.0), Some(1));
        assert_eq!(view.track_at_y(99.0, 3, 100.0), Some(0));
        assert_eq!(view.track_at_y(100.0, 3, 100.0), None);
    }

    #[test]
    fn a_mostly_vertical_swipe_scrolls_vertically_at_the_left_limit() {
        let tall = area();
        let mut view = view(80.0, 0.0);
        let swipe = Wheel { ctrl: false, shift: false, delta_x: 3.0, delta_y: -40.0, cursor_x: 100.0 };
        assert!(view.apply_wheel(swipe, &tall));
        assert_eq!(view.scroll_y(), 40.0);
        let sideways = Wheel { ctrl: false, shift: false, delta_x: -40.0, delta_y: 3.0, cursor_x: 100.0 };
        assert!(view.apply_wheel(sideways, &tall));
        assert_eq!(view.scroll_x(), 40.0);
    }

    #[test]
    fn track_rows_put_track_zero_at_the_bottom() {
        let view = TimelineView::default();
        assert_eq!(view.track_at_y(1.0, 3, 1000.0), Some(2));
        assert_eq!(view.track_at_y(TRACK_HEIGHT * 2.5, 3, 1000.0), Some(0));
        assert_eq!(view.track_at_y(-1.0, 3, 1000.0), None);
        assert_eq!(view.track_at_y(TRACK_HEIGHT * 3.0, 3, 1000.0), None);
        assert_eq!(view.track_top(2, 3), 0.0);
        assert_eq!(view.track_top(0, 3), TRACK_HEIGHT * 2.0);
    }
}
