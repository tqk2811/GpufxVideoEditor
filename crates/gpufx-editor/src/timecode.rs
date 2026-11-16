//! Times and project figures as the UI shows them, and seconds for numeric fields.

use engine::{FrameRate, Time, TICKS_PER_SECOND};

/// `HH:MM:SS:FF`: the whole seconds of `time`, and FF the frames of `frame_rate` that start inside
/// that second before the frame at `time`. Seconds are real time, so a 29.97 fps timeline shows
/// 01:00:00 after an hour, and FF counts 0 to 29 inside most seconds.
pub fn format_timecode(time: Time, frame_rate: FrameRate) -> String {
    let ticks = time.ticks().max(0);
    let total = ticks / TICKS_PER_SECOND;
    let second_start = Time::from_ticks(total * TICKS_PER_SECOND);
    let frame = (frame_rate.frame_at(Time::from_ticks(ticks)) - frame_rate.frames_before(second_start)).max(0);
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

/// `30 fps`, `23.976 fps` or `12.5 fps`: up to three decimals, trailing zeros dropped.
pub fn format_fps(frame_rate: FrameRate) -> String {
    let text = format!("{:.3}", frame_rate.fps());
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} fps")
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

    const THIRTY: FrameRate = FrameRate::whole(30);

    #[test]
    fn a_timecode_counts_hours_minutes_seconds_and_frames() {
        assert_eq!(format_timecode(Time::ZERO, THIRTY), "00:00:00:00");
        assert_eq!(format_timecode(Time::from_ticks(90_000 * 10), THIRTY), "00:00:10:00");
        assert_eq!(format_timecode(Time::from_ticks(90_000 * 27 + 3000 * 12), THIRTY), "00:00:27:12");
        assert_eq!(format_timecode(Time::from_ticks(90_000 * 3661 + 3000), THIRTY), "01:01:01:01");
        assert_eq!(format_timecode(Time::from_ticks(-5), THIRTY), "00:00:00:00");
    }

    #[test]
    fn an_ntsc_timecode_keeps_real_seconds_after_an_hour() {
        let hour = Time::from_ticks(90_000 * 3600);
        assert_eq!(format_timecode(hour, FrameRate::NTSC), "01:00:00:00");
        // The last frame before the hour is the 29th of that second (frames 0 to 29 start in it).
        assert_eq!(format_timecode(Time::from_ticks(hour.ticks() - 1), FrameRate::NTSC), "00:59:59:29");
        assert_eq!(format_timecode(Time::from_ticks(hour.ticks() - 1), FrameRate::NTSC_FILM), "00:59:59:23");
        assert_eq!(format_timecode(Time::from_ticks(90_000 + 3754), FrameRate::NTSC_FILM), "00:00:01:00");
    }

    #[test]
    fn a_duration_is_short() {
        assert_eq!(format_duration(Time::from_ticks(90_000 * 27)), "00:27");
        assert_eq!(format_duration(Time::from_ticks(90_000 * 3725)), "1:02:05");
    }

    #[test]
    fn the_frame_rate_is_shown_whole_when_it_is_whole() {
        assert_eq!(format_fps(THIRTY), "30 fps");
        assert_eq!(format_fps(FrameRate::NTSC), "29.97 fps");
        assert_eq!(format_fps(FrameRate::NTSC_FILM), "23.976 fps");
        assert_eq!(format_fps(FrameRate::NTSC_DOUBLE), "59.94 fps");
        assert_eq!(format_fps(FrameRate::new(25, 2)), "12.5 fps");
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
