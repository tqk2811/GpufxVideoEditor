//! Timeline time as an integer number of ticks.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Number of ticks in one second. 90000 divides evenly into 24, 25, 30 and 60 fps
/// and into the 1000/1001 NTSC cadences (30000/1001 fps is exactly 3003 ticks per frame).
pub const TICKS_PER_SECOND: i64 = 90_000;

/// A point or length on the timeline, in 1/90000 s ticks.
///
/// All arithmetic is integer and checked: the `checked_*` methods return `None`
/// on overflow, the `saturating_*` methods clamp. There is deliberately no
/// `Add`/`Sub` operator and no conversion from floating point.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Time(i64);

impl Time {
    /// Time zero.
    pub const ZERO: Time = Time(0);
    /// Largest representable time.
    pub const MAX: Time = Time(i64::MAX);

    /// Builds a time from a raw tick count.
    pub const fn from_ticks(ticks: i64) -> Self {
        Time(ticks)
    }

    /// Raw tick count.
    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// Whole seconds as a time, or `None` on overflow.
    pub fn from_seconds(seconds: i64) -> Option<Self> {
        seconds.checked_mul(TICKS_PER_SECOND).map(Time)
    }

    /// `self + other`, or `None` on overflow.
    pub fn checked_add(self, other: Time) -> Option<Time> {
        self.0.checked_add(other.0).map(Time)
    }

    /// `self - other`, or `None` on overflow.
    pub fn checked_sub(self, other: Time) -> Option<Time> {
        self.0.checked_sub(other.0).map(Time)
    }

    /// `self * factor`, or `None` on overflow.
    pub fn checked_mul(self, factor: i64) -> Option<Time> {
        self.0.checked_mul(factor).map(Time)
    }

    /// `self + other`, clamped to the `i64` range.
    pub fn saturating_add(self, other: Time) -> Time {
        Time(self.0.saturating_add(other.0))
    }

    /// `self - other`, clamped to the `i64` range.
    pub fn saturating_sub(self, other: Time) -> Time {
        Time(self.0.saturating_sub(other.0))
    }

    /// Half of `self`, rounded toward negative infinity. Used for the centred transition window.
    pub fn half_floor(self) -> Time {
        Time(self.0.div_euclid(2))
    }

    /// Smallest `n` with `n * unit >= self`, for a positive `unit`; `None` if `unit` is not positive.
    /// Negative times give 0 or less like mathematical ceiling division.
    pub fn div_ceil(self, unit: Time) -> Option<i64> {
        if unit.0 <= 0 {
            return None;
        }
        let q = self.0.div_euclid(unit.0);
        Some(if self.0.rem_euclid(unit.0) == 0 {
            q
        } else {
            q + 1
        })
    }
}

impl fmt::Display for Time {
    /// Prints the raw tick count with its unit, for example `90000t`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}t", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_arithmetic_detects_overflow() {
        assert_eq!(Time::MAX.checked_add(Time::from_ticks(1)), None);
        assert_eq!(
            Time::from_ticks(i64::MIN).checked_sub(Time::from_ticks(1)),
            None
        );
        assert_eq!(Time::MAX.checked_mul(2), None);
        assert_eq!(Time::MAX.saturating_add(Time::from_ticks(1)), Time::MAX);
        assert_eq!(Time::from_seconds(2), Some(Time::from_ticks(180_000)));
        assert_eq!(Time::from_seconds(i64::MAX), None);
    }

    #[test]
    fn div_ceil_rounds_up() {
        let unit = Time::from_ticks(3003);
        assert_eq!(Time::from_ticks(0).div_ceil(unit), Some(0));
        assert_eq!(Time::from_ticks(1).div_ceil(unit), Some(1));
        assert_eq!(Time::from_ticks(3003).div_ceil(unit), Some(1));
        assert_eq!(Time::from_ticks(3004).div_ceil(unit), Some(2));
        assert_eq!(Time::from_ticks(1).div_ceil(Time::ZERO), None);
    }

    #[test]
    fn half_floor_of_odd_value() {
        assert_eq!(Time::from_ticks(7).half_floor(), Time::from_ticks(3));
        assert_eq!(Time::from_ticks(8).half_floor(), Time::from_ticks(4));
    }
}
