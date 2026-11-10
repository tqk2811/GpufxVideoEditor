//! Times and project figures as the UI shows them, and seconds for numeric fields.

use engine::{Time, MIN_FRAME_DURATION, TICKS_PER_SECOND};

/// Frames per second for labels and the frame part of a timecode.
fn frames_per_second(frame_duration: Time) -> i64 {
    let per_frame = frame_duration.ticks().max(1);
    (TICKS_PER_SECOND + per_frame / 2) / per_frame
}

/// `HH:MM:SS:FF`, with FF counted from the whole second before it.
pub fn format_timecode(time: Time, frame_duration: Time) -> String {
    let ticks = time.ticks().max(0);
    let total = ticks / TICKS_PER_SECOND;
    let frame = (ticks % TICKS_PER_SECOND) / frame_duration.ticks().max(1);
    format!("{:02}:{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60, frame)
}

/// `MM:SS`, or `H:MM:SS` for an hour or more.
pub fn format_duration(time: Time) -> String {
    let total = time.ticks().max(0) / TICKS_PER_SECOND;
    if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
    } else {
        format!("{:02}:{:02}", total / 60, total % 60)
    }
}

pub fn format_fps(frame_duration: Time) -> String {
    let per_frame = frame_duration.ticks().max(1);
    let exact = TICKS_PER_SECOND as f64 / per_frame as f64;
    if (exact - exact.round()).abs() < 0.005 {
        format!("{} fps", frames_per_second(frame_duration))
    } else {
        format!("{exact:.2} fps")
    }
}

/// The frame rate `frame_duration` stands for, in frames per second.
pub fn fps_from_frame_duration(frame_duration: Time) -> f64 {
    TICKS_PER_SECOND as f64 / frame_duration.ticks().max(1) as f64
}

/// One frame of a custom rate of `fps` frames per second, rounded to a whole tick and never shorter
/// than [`MIN_FRAME_DURATION`]. `None` for a rate that is not a number or outside 1 to 60 fps.
pub fn frame_duration_from_fps(fps: f64) -> Option<Time> {
    if !fps.is_finite() || !(MIN_FPS..=MAX_FPS).contains(&fps) {
        return None;
    }
    let ticks = (TICKS_PER_SECOND as f64 / fps).round() as i64;
    Some(Time::from_ticks(ticks.max(MIN_FRAME_DURATION.ticks())))
}

pub const MIN_FPS: f64 = 1.0;
pub const MAX_FPS: f64 = 60.0;

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `16:9` when the reduced ratio is small enough to read, else `2.39:1`.
pub fn aspect_label(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return "-".to_owned();
    }
    let divisor = gcd(width, height);
    let (x, y) = (width / divisor, height / divisor);
    if x <= 32 && y <= 32 {
        format!("{x}:{y}")
    } else {
        format!("{:.2}:1", f64::from(width) / f64::from(height))
    }
}

pub fn to_seconds(time: Time) -> f64 {
    time.ticks() as f64 / TICKS_PER_SECOND as f64
}

/// Seconds as ticks, never negative.
pub fn from_seconds(seconds: f64) -> Time {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Time::ZERO;
    }
    Time::from_ticks((seconds * TICKS_PER_SECOND as f64).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THIRTY: Time = Time::from_ticks(3000);

    #[test]
    fn a_timecode_counts_hours_minutes_seconds_and_frames() {
        assert_eq!(format_timecode(Time::ZERO, THIRTY), "00:00:00:00");
        assert_eq!(format_timecode(Time::from_ticks(90_000 * 10), THIRTY), "00:00:10:00");
        assert_eq!(format_timecode(Time::from_ticks(90_000 * 27 + 3000 * 12), THIRTY), "00:00:27:12");
        assert_eq!(format_timecode(Time::from_ticks(90_000 * 3661 + 3000), THIRTY), "01:01:01:01");
        assert_eq!(format_timecode(Time::from_ticks(-5), THIRTY), "00:00:00:00");
    }

    #[test]
    fn a_duration_is_short() {
        assert_eq!(format_duration(Time::from_ticks(90_000 * 27)), "00:27");
        assert_eq!(format_duration(Time::from_ticks(90_000 * 3725)), "1:02:05");
    }

    #[test]
    fn the_frame_rate_is_shown_whole_when_it_is_whole() {
        assert_eq!(format_fps(THIRTY), "30 fps");
        assert_eq!(format_fps(Time::from_ticks(3003)), "29.97 fps");
    }

    #[test]
    fn a_custom_frame_rate_becomes_whole_ticks() {
        assert_eq!(frame_duration_from_fps(1.0), Some(Time::from_ticks(90_000)));
        assert_eq!(frame_duration_from_fps(60.0), Some(Time::from_ticks(1500)));
        assert_eq!(frame_duration_from_fps(30.0), Some(Time::from_ticks(3000)));
        assert_eq!(frame_duration_from_fps(7.0), Some(Time::from_ticks(12_857)));
    }

    #[test]
    fn a_frame_rate_out_of_range_or_not_a_number_is_refused() {
        for fps in [0.5, 61.0, 0.0, -30.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(frame_duration_from_fps(fps), None, "{fps}");
        }
    }

    #[test]
    fn the_frame_rate_of_a_duration_comes_back_to_the_same_duration() {
        for ticks in [3750, 3600, 3003, 3000, 1800, 1500, 12_857, 90_000] {
            let duration = Time::from_ticks(ticks);
            assert_eq!(frame_duration_from_fps(fps_from_frame_duration(duration)), Some(duration));
        }
    }

    #[test]
    fn aspect_ratios_reduce_or_fall_back_to_a_decimal() {
        assert_eq!(aspect_label(1920, 1080), "16:9");
        assert_eq!(aspect_label(1080, 1920), "9:16");
        assert_eq!(aspect_label(1280, 535), "2.39:1");
        assert_eq!(aspect_label(0, 5), "-");
    }

    #[test]
    fn seconds_round_trip_and_never_go_negative() {
        assert_eq!(from_seconds(to_seconds(Time::from_ticks(45_000))), Time::from_ticks(45_000));
        assert_eq!(from_seconds(-3.0), Time::ZERO);
        assert_eq!(from_seconds(f64::NAN), Time::ZERO);
    }
}
