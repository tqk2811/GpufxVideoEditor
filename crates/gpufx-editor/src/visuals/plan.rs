//! Which picture and which audio samples belong where along a clip. No window, no files.

use std::ops::Range;

use engine::{Clip, Time, TICKS_PER_SECOND};

/// The smallest gap between two pictures: 1/16 s. Deeper zoom repeats pictures instead of asking
/// for more of them.
pub const MIN_BUCKET_TICKS: i64 = TICKS_PER_SECOND / 16;
/// Height of a thumbnail in pixels.
pub const THUMBNAIL_HEIGHT: u32 = 72;
/// A bar is this wide, with a gap of [`BAR_GAP`] after it.
pub const BAR_WIDTH: f64 = 2.0;
pub const BAR_GAP: f64 = 1.0;
/// Space kept for the level bars under the pictures of a clip with sound.
const BARS_SHARE: f32 = 0.34;

/// The gap between pictures for tiles lasting `tile_ticks`: the largest doubling of
/// [`MIN_BUCKET_TICKS`] that still gives each tile its own picture. The grids of two zoom levels
/// nest, so a picture made for one is the right one for the other.
pub fn bucket_step(tile_ticks: i64) -> i64 {
    let mut step = MIN_BUCKET_TICKS;
    while step.saturating_mul(2) <= tile_ticks {
        step *= 2;
    }
    step
}

/// `ticks` rounded to the grid of `step`, kept inside a source `length` long when it is known.
pub fn bucket_time(ticks: i64, step: i64, length: Option<Time>) -> i64 {
    let bucket = (ticks.max(0) + step / 2).div_euclid(step) * step;
    match length {
        Some(length) if length.ticks() > 0 => bucket.min((length.ticks() - 1).div_euclid(step) * step),
        _ => bucket,
    }
}

/// Ticks of screen width `x` at `pixels_per_second`.
fn width_ticks(x: f64, pixels_per_second: f64) -> i64 {
    (x / pixels_per_second * TICKS_PER_SECOND as f64).round() as i64
}

/// The source ticks a tile `tile_width` wide covers of `clip`.
pub fn tile_ticks(clip: &Clip, tile_width: f64, pixels_per_second: f64) -> i64 {
    clip.source_offset(Time::from_ticks(width_ticks(tile_width, pixels_per_second))).ticks().max(1)
}

/// The source position at the middle of tile `index` of a clip, inside what the clip plays.
pub fn tile_source_ticks(clip: &Clip, tile_width: f64, pixels_per_second: f64, index: usize) -> i64 {
    let middle = (index as f64 + 0.5) * tile_width;
    let into_clip = width_ticks(middle, pixels_per_second).clamp(0, clip.duration.ticks().saturating_sub(1).max(0));
    clip.source_in.saturating_add(clip.source_offset(Time::from_ticks(into_clip))).ticks()
}

/// The cells of width `cell` from `origin` that overlap `visible`, within `extent` of cells.
pub fn visible_cells(origin: f64, cell: f64, extent: f64, visible: Range<f64>) -> Range<usize> {
    if cell <= 0.0 || extent <= 0.0 || visible.end <= origin {
        return 0..0;
    }
    let total = (extent / cell).ceil() as usize;
    let first = ((visible.start - origin) / cell).floor().max(0.0) as usize;
    let last = (((visible.end - origin) / cell).ceil().max(0.0) as usize).min(total);
    first.min(last)..last
}

/// The source ticks a level bar `index` covers.
pub fn bar_span(clip: &Clip, pixels_per_second: f64, index: usize) -> Range<i64> {
    let pitch = BAR_WIDTH + BAR_GAP;
    let begin = width_ticks(index as f64 * pitch, pixels_per_second);
    let end = width_ticks((index + 1) as f64 * pitch, pixels_per_second).min(clip.duration.ticks());
    let at = |ticks: i64| clip.source_in.saturating_add(clip.source_offset(Time::from_ticks(ticks))).ticks();
    at(begin)..at(end.max(begin + 1)).max(at(begin) + 1)
}

/// Heights of the picture strip and of the level bars in a clip body `height` tall.
pub fn split_body(height: f32, film: bool, bars: bool) -> (f32, f32) {
    match (film, bars) {
        (true, true) => {
            let bar_height = (height * BARS_SHARE).round();
            (height - bar_height, bar_height)
        }
        (true, false) => (height, 0.0),
        (false, true) => (0.0, height),
        (false, false) => (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(source_in: i64, duration: i64) -> Clip {
        let mut clip = crate::editor::new_clip("a.mp4", Time::ZERO, Time::from_seconds(duration).unwrap());
        clip.source_in = Time::from_seconds(source_in).unwrap();
        clip
    }

    #[test]
    fn the_step_is_the_largest_doubling_that_fits_a_tile() {
        assert_eq!(bucket_step(1), MIN_BUCKET_TICKS, "deep zoom stops at the smallest step");
        assert_eq!(bucket_step(MIN_BUCKET_TICKS * 3), MIN_BUCKET_TICKS * 2);
        assert_eq!(bucket_step(TICKS_PER_SECOND * 5), MIN_BUCKET_TICKS * 64);
    }

    #[test]
    fn zoom_levels_share_their_pictures() {
        // The picture a coarser zoom shows is also on the finer grid.
        let coarse = bucket_time(TICKS_PER_SECOND * 4 + 7, TICKS_PER_SECOND, None);
        assert_eq!(bucket_time(coarse, TICKS_PER_SECOND / 2, None), coarse);
    }

    #[test]
    fn neighbouring_tiles_of_a_zoom_get_different_pictures() {
        let pixels_per_second = 10.0;
        let tile = 16.0;
        let clip = clip(0, 100);
        let step = bucket_step(tile_ticks(&clip, tile, pixels_per_second));
        let times: Vec<i64> = (0..40).map(|i| bucket_time(tile_source_ticks(&clip, tile, pixels_per_second, i), step, None)).collect();
        for pair in times.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?}");
        }
    }

    #[test]
    fn a_bucket_stays_inside_the_source() {
        let length = Some(Time::from_ticks(TICKS_PER_SECOND * 3));
        assert_eq!(bucket_time(TICKS_PER_SECOND * 9, TICKS_PER_SECOND, length), TICKS_PER_SECOND * 2);
        assert_eq!(bucket_time(-5, TICKS_PER_SECOND, length), 0);
    }

    #[test]
    fn a_tile_shows_the_source_position_after_the_in_point() {
        let clip = clip(10, 20);
        // 100 px/s, a 200 px tile: the middle of tile 0 is one second in.
        assert_eq!(tile_source_ticks(&clip, 200.0, 100.0, 0), 11 * TICKS_PER_SECOND);
        assert_eq!(tile_source_ticks(&clip, 200.0, 100.0, 1), 13 * TICKS_PER_SECOND);
    }

    #[test]
    fn a_tile_stops_at_the_end_of_the_clip() {
        let clip = clip(0, 1);
        assert!(tile_source_ticks(&clip, 2000.0, 100.0, 3) < TICKS_PER_SECOND);
    }

    #[test]
    fn a_bar_covers_the_part_of_the_source_after_the_in_point() {
        let clip = clip(10, 5);
        let span = bar_span(&clip, 300.0, 0);
        assert_eq!(span.start, 10 * TICKS_PER_SECOND);
        assert_eq!(span.end - span.start, TICKS_PER_SECOND / 100);
        let last = bar_span(&clip, 300.0, 499);
        assert!(last.end <= 15 * TICKS_PER_SECOND);
    }

    #[test]
    fn tiles_and_bars_follow_the_speed_of_the_clip() {
        let mut fast = clip(10, 20);
        fast.speed = 2.0;
        // A 200 px tile at 100 px/s is two timeline seconds, four of the source.
        assert_eq!(tile_source_ticks(&fast, 200.0, 100.0, 0), 12 * TICKS_PER_SECOND);
        assert_eq!(tile_source_ticks(&fast, 200.0, 100.0, 1), 16 * TICKS_PER_SECOND);
        assert_eq!(tile_ticks(&fast, 200.0, 100.0), 4 * TICKS_PER_SECOND);
        let bar = bar_span(&fast, 300.0, 0);
        assert_eq!(bar.start, 10 * TICKS_PER_SECOND);
        assert_eq!(bar.end - bar.start, 2 * TICKS_PER_SECOND / 100, "a bar covers twice the source");
        let mut slow = clip(10, 20);
        slow.speed = 0.5;
        assert_eq!(tile_source_ticks(&slow, 200.0, 100.0, 0), 10 * TICKS_PER_SECOND + TICKS_PER_SECOND / 2);
        assert_eq!(tile_ticks(&slow, 200.0, 100.0), TICKS_PER_SECOND);
    }

    #[test]
    fn only_cells_on_screen_are_visible() {
        assert_eq!(visible_cells(100.0, 40.0, 400.0, 150.0..210.0), 1..3);
        assert_eq!(visible_cells(100.0, 40.0, 400.0, 0.0..1000.0), 0..10, "the clip ends");
        assert_eq!(visible_cells(100.0, 40.0, 400.0, 0.0..50.0), 0..0, "off screen");
        assert_eq!(visible_cells(0.0, 40.0, 100.0, 90.0..95.0), 2..3, "a partial last cell counts");
    }

    #[test]
    fn a_clip_body_is_divided_by_what_it_has() {
        assert_eq!(split_body(50.0, true, false), (50.0, 0.0));
        assert_eq!(split_body(50.0, false, true), (0.0, 50.0));
        let (film, bars) = split_body(50.0, true, true);
        assert!(bars >= 15.0 && film + bars == 50.0, "{film} {bars}");
        assert_eq!(split_body(50.0, false, false), (0.0, 0.0));
    }
}
