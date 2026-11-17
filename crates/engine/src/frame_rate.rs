//! Output frame rate as an exact fraction of frames per second.
//!
//! A rate like 24000/1001 (23.976 fps) gives frames of 3753.75 ticks, which no whole tick count
//! can hold. The rate is therefore kept as `num / den` frames per second and every frame boundary
//! is computed from the frame index with integers: frame `n` starts at `floor(n * den * 90000 /
//! num)` ticks. Each boundary is rounded on its own, so nothing accumulates and an hour of 23.976
//! is exactly 86313.6 frames long, to the tick.

use crate::time::{Time, TICKS_PER_SECOND};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Frames per second as the fraction `num / den`, both positive. Equal rates compare equal only
/// when reduced the same way; [`FrameRate::new`] reduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    /// Numerator: frames.
    pub num: u32,
    /// Denominator: seconds.
    pub den: u32,
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

impl FrameRate {
    /// 24000/1001, called 23.976 fps.
    pub const NTSC_FILM: FrameRate = FrameRate { num: 24000, den: 1001 };
    /// 30000/1001, called 29.97 fps.
    pub const NTSC: FrameRate = FrameRate { num: 30000, den: 1001 };
    /// 60000/1001, called 59.94 fps.
    pub const NTSC_DOUBLE: FrameRate = FrameRate { num: 60000, den: 1001 };
    /// The fastest output rate, 60 fps.
    pub const MAX: FrameRate = FrameRate { num: 60, den: 1 };
    /// The slowest output rate, 1 fps.
    pub const MIN: FrameRate = FrameRate { num: 1, den: 1 };

    /// `num / den` reduced. A zero part is kept as given, so [`FrameRate::is_valid`] can refuse it.
    pub const fn new(num: u32, den: u32) -> Self {
        let divisor = gcd(num as u64, den as u64);
        if divisor <= 1 {
            FrameRate { num, den }
        } else {
            FrameRate { num: (num as u64 / divisor) as u32, den: (den as u64 / divisor) as u32 }
        }
    }

    /// Whole frames per second.
    pub const fn whole(fps: u32) -> Self {
        FrameRate { num: fps, den: 1 }
    }

    /// The rate whose frames last exactly `duration` ticks (90000 / duration fps). `None` for a
    /// duration that is not positive or does not fit a `u32`.
    pub fn from_frame_duration(duration: Time) -> Option<Self> {
        let ticks = u32::try_from(duration.ticks()).ok().filter(|t| *t > 0)?;
        Some(FrameRate::new(TICKS_PER_SECOND as u32, ticks))
    }

    /// Both parts are positive.
    pub const fn is_valid(self) -> bool {
        self.num > 0 && self.den > 0
    }

    /// Whether `self` is faster than `other`.
    pub fn is_faster_than(self, other: FrameRate) -> bool {
        u64::from(self.num) * u64::from(other.den) > u64::from(other.num) * u64::from(self.den)
    }

    /// A valid rate from 1 to 60 fps, the range an output may use.
    pub fn is_output_rate(self) -> bool {
        self.is_valid() && !self.is_faster_than(Self::MAX) && !Self::MIN.is_faster_than(self)
    }

    /// The rate in frames per second, for labels and typed values only.
    pub fn fps(self) -> f64 {
        f64::from(self.num) / f64::from(self.den.max(1))
    }

    /// The nearest output rate to `fps`: the NTSC rates when `fps` is within 0.005 of 23.976, 29.97
    /// or 59.94, else `fps` rounded to a thousandth. `None` outside 1 to 60 fps or for no number.
    pub fn from_fps(fps: f64) -> Option<Self> {
        if !fps.is_finite() {
            return None;
        }
        let ntsc = [Self::NTSC_FILM, Self::NTSC, Self::NTSC_DOUBLE];
        let rate = ntsc.into_iter().find(|rate| (rate.fps() - fps).abs() < 0.005).unwrap_or_else(|| {
            let thousandths = (fps * 1000.0).round().clamp(0.0, f64::from(u32::MAX));
            FrameRate::new(thousandths as u32, 1000)
        });
        rate.is_output_rate().then_some(rate)
    }

    /// `(den * 90000, num)`: frame `n` starts at exactly `n * a / b` ticks.
    fn ticks_ratio(self) -> (i128, i128) {
        (i128::from(self.den) * i128::from(TICKS_PER_SECOND), i128::from(self.num))
    }

    /// Start of frame `frame`, `floor(frame * den * 90000 / num)` ticks. `None` for an invalid rate
    /// or on overflow.
    pub fn frame_start(self, frame: i64) -> Option<Time> {
        if !self.is_valid() {
            return None;
        }
        let (a, b) = self.ticks_ratio();
        i64::try_from((i128::from(frame) * a).div_euclid(b)).ok().map(Time::from_ticks)
    }

    /// Number of frames that start before `end`: the smallest `n` with `frame_start(n) >= end`,
    /// `ceil(end * num / (den * 90000))`. A final partial frame counts. 0 for an invalid rate or an
    /// end at or before zero.
    pub fn frames_before(self, end: Time) -> i64 {
        if !self.is_valid() || end <= Time::ZERO {
            return 0;
        }
        let (a, b) = self.ticks_ratio();
        let numerator = i128::from(end.ticks()) * b;
        let count = (numerator + a - 1).div_euclid(a);
        i64::try_from(count).unwrap_or(i64::MAX)
    }

    /// The frame shown at `time`: the largest `n` with `frame_start(n) <= time`. 0 for an invalid
    /// rate.
    pub fn frame_at(self, time: Time) -> i64 {
        if !self.is_valid() {
            return 0;
        }
        // floor(n a / b) <= t  <=>  n a / b < t + 1  <=>  n < (t + 1) b / a.
        let (a, b) = self.ticks_ratio();
        let limit = (i128::from(time.ticks()) + 1) * b;
        let count = (limit + a - 1).div_euclid(a) - 1;
        i64::try_from(count).unwrap_or(if count < 0 { i64::MIN } else { i64::MAX })
    }

    /// Length of one frame rounded down to a whole tick (3753 at 23.976 fps), at least one tick:
    /// for tolerances and shortest lengths, never for placing frames.
    pub fn frame_duration(self) -> Time {
        if !self.is_valid() {
            return Time::from_ticks(1);
        }
        let (a, b) = self.ticks_ratio();
        Time::from_ticks(i64::try_from(a / b).unwrap_or(i64::MAX).max(1))
    }

    /// The audio sample at which frame `frame` starts at `sample_rate` samples per second,
    /// `floor(frame * den * sample_rate / num)`: 1601.6 samples a frame at 29.97 and 48000 Hz,
    /// spread so the total after `n` frames is exact. `None` for an invalid rate or on overflow.
    pub fn frame_sample(self, frame: i64, sample_rate: u32) -> Option<i64> {
        if !self.is_valid() {
            return None;
        }
        let numerator = i128::from(frame) * i128::from(self.den) * i128::from(sample_rate);
        i64::try_from(numerator.div_euclid(i128::from(self.num))).ok()
    }
}

impl fmt::Display for FrameRate {
    /// `24000/1001`, or `30` for a whole rate.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Time = Time::from_ticks(3600 * TICKS_PER_SECOND);

    #[test]
    fn new_reduces_and_keeps_zero() {
        assert_eq!(FrameRate::new(60000, 2002), FrameRate::NTSC);
        assert_eq!(FrameRate::new(90000, 3000), FrameRate::whole(30));
        assert!(!FrameRate::new(0, 5).is_valid());
        assert!(!FrameRate::new(5, 0).is_valid());
    }

    #[test]
    fn ntsc_frames_do_not_drift_over_an_hour() {
        // 3600 s * 24000 / 1001 = 86313.686... frames: the 86314th starts inside the hour.
        let film = FrameRate::NTSC_FILM;
        assert_eq!(film.frames_before(HOUR), 86_314);
        // Every 4 frames of 23.976 are exactly 15015 ticks; the boundary after a million frames
        // is where the exact fraction says, not a million times a rounded duration.
        assert_eq!(film.frame_start(4_000_000), Some(Time::from_ticks(15_015_000_000)));
        assert_eq!(film.frame_start(1), Some(Time::from_ticks(3753)));
        assert_eq!(film.frame_start(2), Some(Time::from_ticks(7507)));
        let fifty_nine = FrameRate::NTSC_DOUBLE;
        assert_eq!(fifty_nine.frames_before(HOUR), 215_785);
        assert_eq!(fifty_nine.frame_start(2), Some(Time::from_ticks(3003)));
        assert_eq!(FrameRate::NTSC.frame_start(107_892), Some(Time::from_ticks(107_892 * 3003)));
        // The start of the last frame of the hour stays within one frame of the real hour.
        let last = film.frame_start(film.frames_before(HOUR) - 1).unwrap();
        assert!(last < HOUR && HOUR.ticks() - last.ticks() <= film.frame_duration().ticks() + 1);
    }

    #[test]
    fn frame_at_and_frames_before_agree_with_frame_start() {
        for rate in [FrameRate::NTSC_FILM, FrameRate::NTSC, FrameRate::NTSC_DOUBLE, FrameRate::whole(25), FrameRate::new(7, 1)] {
            for n in (0..2000).chain([1_000_003, 86_313, 86_314]) {
                let start = rate.frame_start(n).unwrap();
                assert_eq!(rate.frame_at(start), n, "{rate} frame {n}");
                assert_eq!(rate.frame_at(start.saturating_sub(Time::from_ticks(1))), n - 1, "{rate} before frame {n}");
                assert_eq!(rate.frames_before(start.saturating_add(Time::from_ticks(1))), n + 1, "{rate} just after frame {n}");
                if n > 0 {
                    assert_eq!(rate.frames_before(start), n, "{rate} at frame {n}");
                }
            }
        }
    }

    #[test]
    fn audio_samples_per_frame_add_up_exactly() {
        // 48000 * 1001 / 30000 = 1601.6 samples a frame: 1601 or 1602, five frames are 8008.
        let ntsc = FrameRate::NTSC;
        let counts: Vec<i64> = (0..5).map(|n| ntsc.frame_sample(n + 1, 48_000).unwrap() - ntsc.frame_sample(n, 48_000).unwrap()).collect();
        assert_eq!(counts.iter().sum::<i64>(), 8008);
        assert!(counts.iter().all(|c| *c == 1601 || *c == 1602), "{counts:?}");
        // An hour of frames carries exactly as many samples as the fraction says.
        let frames = ntsc.frames_before(HOUR);
        assert_eq!(ntsc.frame_sample(frames, 48_000), Some(frames * 48_000 * 1001 / 30_000));
        assert_eq!(FrameRate::NTSC_FILM.frame_sample(1, 48_000), Some(2002));
        assert_eq!(FrameRate::NTSC_DOUBLE.frame_sample(10, 48_000), Some(8008));
    }

    #[test]
    fn from_fps_snaps_to_ntsc_and_checks_the_range() {
        assert_eq!(FrameRate::from_fps(23.976), Some(FrameRate::NTSC_FILM));
        assert_eq!(FrameRate::from_fps(29.97), Some(FrameRate::NTSC));
        assert_eq!(FrameRate::from_fps(59.94), Some(FrameRate::NTSC_DOUBLE));
        assert_eq!(FrameRate::from_fps(30.0), Some(FrameRate::whole(30)));
        assert_eq!(FrameRate::from_fps(12.5), Some(FrameRate::new(25, 2)));
        for bad in [0.5, 60.5, f64::NAN, f64::INFINITY, -3.0] {
            assert_eq!(FrameRate::from_fps(bad), None, "{bad}");
        }
    }

    #[test]
    fn frame_duration_is_a_floor_of_one_frame() {
        assert_eq!(FrameRate::NTSC_FILM.frame_duration(), Time::from_ticks(3753));
        assert_eq!(FrameRate::NTSC.frame_duration(), Time::from_ticks(3003));
        assert_eq!(FrameRate::MAX.frame_duration(), Time::from_ticks(1500));
        assert_eq!(FrameRate::from_frame_duration(Time::from_ticks(3003)), Some(FrameRate::NTSC));
        assert_eq!(FrameRate::from_frame_duration(Time::ZERO), None);
    }

    #[test]
    fn serializes_as_num_and_den() {
        assert_eq!(serde_json::to_string(&FrameRate::NTSC_FILM).unwrap(), r#"{"num":24000,"den":1001}"#);
        assert_eq!(FrameRate::NTSC_FILM.to_string(), "24000/1001");
        assert_eq!(FrameRate::whole(25).to_string(), "25");
    }
}
