//! Times and project figures as the UI shows them, and seconds for numeric fields.

use engine::{Time, TICKS_PER_SECOND};

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
