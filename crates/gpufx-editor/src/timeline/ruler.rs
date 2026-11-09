//! Time ruler ticks and labels.

use super::view::TimelineView;
use engine::{Time, TICKS_PER_SECOND};

const MIN_MAJOR_SPACING: f64 = 80.0;
const MINOR_PER_MAJOR: i64 = 5;
const STEPS: [i64; 15] = [
    9_000,
    18_000,
    45_000,
    90_000,
    180_000,
    450_000,
    900_000,
    1_350_000,
    2_700_000,
    5_400_000,
    10_800_000,
    27_000_000,
    54_000_000,
    162_000_000,
    324_000_000,
];

#[derive(Debug, Clone, PartialEq)]
pub struct RulerTick {
    pub time: Time,
    pub x: f64,
    pub major: bool,
}

/// The ticks between major labels of the current zoom.
pub fn major_step(view: &TimelineView) -> i64 {
    let per_tick = view.pixels_per_second() / TICKS_PER_SECOND as f64;
    STEPS.iter().copied().find(|&step| step as f64 * per_tick >= MIN_MAJOR_SPACING).unwrap_or(STEPS[STEPS.len() - 1])
}

/// The ticks visible in a ruler `width` points wide, none before time zero.
pub fn ruler_ticks(view: &TimelineView, width: f64) -> Vec<RulerTick> {
    let major = major_step(view);
    let minor = major / MINOR_PER_MAJOR;
    let first = view.x_to_time(0.0).ticks().max(0).div_euclid(minor);
    let mut ticks = Vec::new();
    for index in first.. {
        let time = Time::from_ticks(index * minor);
        let x = view.time_to_x(time);
        if x > width {
            break;
        }
        if x >= 0.0 {
            ticks.push(RulerTick { time, x, major: index % MINOR_PER_MAJOR == 0 });
        }
    }
    ticks
}

/// `m:ss`, or `m:ss.d` when `step` is below one second.
pub fn format_time(time: Time, step: i64) -> String {
    let ticks = time.ticks().max(0);
    let seconds = ticks / TICKS_PER_SECOND;
    let label = format!("{}:{:02}", seconds / 60, seconds % 60);
    if step < TICKS_PER_SECOND {
        format!("{label}.{}", (ticks % TICKS_PER_SECOND) / (TICKS_PER_SECOND / 10))
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::view::{ViewArea, MAX_PIXELS_PER_SECOND, MIN_PIXELS_PER_SECOND};

    fn view_at(factor: f64) -> TimelineView {
        let area = ViewArea { content_end: Time::ZERO, width: 1000.0, track_count: 0, rows_height: 0.0 };
        let mut view = TimelineView::default();
        view.zoom_at(factor, 0.0, &area);
        view
    }

    #[test]
    fn labels_are_spaced_at_every_zoom() {
        for factor in [1e-6, 0.1, 1.0, 5.0, 1e6] {
            let view = view_at(factor);
            let step = major_step(&view);
            let spacing = step as f64 / TICKS_PER_SECOND as f64 * view.pixels_per_second();
            assert!(spacing >= MIN_MAJOR_SPACING, "zoom {}", view.pixels_per_second());
        }
        assert!(view_at(1e6).pixels_per_second() == MAX_PIXELS_PER_SECOND);
        assert!(view_at(1e-6).pixels_per_second() == MIN_PIXELS_PER_SECOND);
    }

    #[test]
    fn ticks_stay_in_the_ruler_and_start_at_zero() {
        let ticks = ruler_ticks(&TimelineView::default(), 400.0);
        assert_eq!(ticks[0].time, Time::ZERO);
        assert!(ticks[0].major);
        assert!(ticks.iter().all(|tick| tick.x >= 0.0 && tick.x <= 400.0));
        assert!(ticks.iter().any(|tick| !tick.major));
    }

    #[test]
    fn labels_show_tenths_only_below_a_second() {
        assert_eq!(format_time(Time::from_ticks(90_000 * 75 + 9_000), 45_000), "1:15.1");
        assert_eq!(format_time(Time::from_ticks(90_000 * 75), 90_000), "1:15");
    }
}
