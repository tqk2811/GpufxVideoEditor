//! Audio levels of a source, computed once at a fixed rate, and how a level becomes a bar. Pure.

use engine::TICKS_PER_SECOND;

/// Peak values kept per second of audio: one every 20 ms.
pub const PEAKS_PER_SECOND: i64 = 50;
/// The quietest level a bar shows; everything below draws nothing.
pub const FLOOR_DB: f32 = -60.0;
/// How much each level of the pyramid is coarser than the one below.
const LEVEL_FACTOR: usize = 8;
/// A query reads at most about this many entries per level.
const MAX_READ: usize = 32;

/// `peak` (linear) with `gain_db` applied as a fraction of bar height: dBFS on a linear scale from
/// [`FLOOR_DB`] to 0 dBFS. Anything over 0 dBFS is a full bar.
pub fn bar_fraction(peak: f32, gain_db: f64) -> f32 {
    if peak <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * peak.log10() + gain_db as f32;
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// The peak bucket of `ticks` of the source.
pub fn peak_bucket(ticks: i64) -> i64 {
    ticks.max(0).div_euclid(TICKS_PER_SECOND / PEAKS_PER_SECOND)
}

/// Peaks of one source, with a pyramid of maxima so a range costs the same at every zoom.
#[derive(Debug, Clone, Default)]
pub struct PeakData {
    levels: Vec<Vec<f32>>,
}

impl PeakData {
    /// Appends more of the source, as the background reads it.
    pub fn append(&mut self, peaks: &[f32]) {
        if self.levels.is_empty() {
            self.levels.push(Vec::new());
        }
        self.levels[0].extend_from_slice(peaks);
        let mut level = 0;
        while self.levels[level].len() > MAX_READ {
            if self.levels.len() == level + 1 {
                self.levels.push(Vec::new());
            }
            let (lower, upper) = self.levels.split_at_mut(level + 1);
            let finer = &lower[level];
            let coarser = &mut upper[0];
            // The last entry may have covered a group that was still filling.
            let start = coarser.len().saturating_sub(1);
            coarser.truncate(start);
            coarser.extend(finer[start * LEVEL_FACTOR..].chunks(LEVEL_FACTOR).map(max_of));
            level += 1;
        }
    }

    /// Buckets read so far.
    pub fn len(&self) -> usize {
        self.levels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes held, for the memory limit.
    pub fn bytes(&self) -> usize {
        self.levels.iter().map(|level| level.len() * size_of::<f32>()).sum()
    }

    /// The loudest peak of buckets `start..end`, 0 where nothing was read yet.
    pub fn max_between(&self, start: i64, end: i64) -> f32 {
        let Some(base) = self.levels.first() else { return 0.0 };
        let (start, end) = (start.max(0) as usize, (end.max(start + 1).max(0) as usize).min(base.len()));
        if start >= end {
            return 0.0;
        }
        let mut level = 0;
        let mut scale = 1;
        while (end - start) / scale > MAX_READ && level + 1 < self.levels.len() {
            level += 1;
            scale *= LEVEL_FACTOR;
        }
        let entries = &self.levels[level];
        let from = (start / scale).min(entries.len());
        let to = ((end - 1) / scale + 1).min(entries.len());
        entries[from..to].iter().copied().fold(0.0, f32::max)
    }
}

fn max_of(values: &[f32]) -> f32 {
    values.iter().copied().fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_is_a_full_bar_and_the_floor_is_none() {
        assert_eq!(bar_fraction(1.0, 0.0), 1.0);
        assert!(bar_fraction(0.001, 0.0).abs() < 1e-3, "-60 dBFS");
        assert_eq!(bar_fraction(0.0, 0.0), 0.0, "silence");
        assert_eq!(bar_fraction(0.0005, 0.0), 0.0, "below the floor");
        assert!((bar_fraction(0.1, 0.0) - 2.0 / 3.0).abs() < 1e-3, "-20 dBFS");
    }

    #[test]
    fn gain_moves_the_bar_and_cannot_exceed_full() {
        assert!((bar_fraction(0.1, 20.0) - 1.0).abs() < 1e-3);
        assert_eq!(bar_fraction(1.0, 12.0), 1.0);
        assert!(bar_fraction(0.5, -6.0) < bar_fraction(0.5, 0.0));
    }

    #[test]
    fn a_bucket_is_twenty_milliseconds() {
        assert_eq!(peak_bucket(TICKS_PER_SECOND), 50);
        assert_eq!(peak_bucket(-1), 0);
        assert_eq!(peak_bucket(TICKS_PER_SECOND / 50 - 1), 0);
    }

    #[test]
    fn a_range_finds_its_loudest_peak_at_any_length() {
        let mut data = PeakData::default();
        let mut peaks = vec![0.1; 40_000];
        peaks[30_000] = 0.9;
        peaks[10] = 0.5;
        data.append(&peaks);
        assert_eq!(data.max_between(0, 5), 0.1);
        assert_eq!(data.max_between(5, 20), 0.5);
        assert_eq!(data.max_between(0, 40_000), 0.9, "the full range");
        assert_eq!(data.max_between(29_000, 31_000), 0.9);
    }

    #[test]
    fn appending_in_pieces_equals_appending_at_once() {
        let peaks: Vec<f32> = (0..5000).map(|i| ((i * 37) % 101) as f32 / 100.0).collect();
        let mut whole = PeakData::default();
        whole.append(&peaks);
        let mut pieces = PeakData::default();
        for part in peaks.chunks(37) {
            pieces.append(part);
        }
        for (start, end) in [(0, 5000), (17, 19), (100, 4000), (4990, 5000), (4999, 6000)] {
            assert_eq!(pieces.max_between(start, end), whole.max_between(start, end), "{start} {end}");
        }
    }

    #[test]
    fn a_range_outside_what_was_read_is_silent() {
        let mut data = PeakData::default();
        data.append(&[0.5; 10]);
        assert_eq!(data.max_between(10, 20), 0.0);
        assert_eq!(PeakData::default().max_between(0, 10), 0.0);
        assert_eq!(data.max_between(-5, 2), 0.5);
    }
}
