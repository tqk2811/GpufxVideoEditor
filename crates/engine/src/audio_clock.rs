//! Audio clock and pull API, independent of any playback device.
//!
//! A device (real or fake) asks for samples with [`AudioClock::pull`]; the caller keeps samples
//! ready with [`AudioClock::prepare`]. The clock's [`AudioClock::time`] counts only the samples the
//! device consumed, so it is what a preview follows as its master clock.
//!
//! # Rules
//!
//! * **Position.** `position` is the timeline sample the device consumes next. It starts at 0 and
//!   moves only when real samples are consumed.
//! * **Time.** Sample `s` maps to `ceil(s * 15 / 8)` ticks, the earliest tick with
//!   `sample_position(tick) == s`, see [`sample_time`].
//! * **Seek.** [`AudioClock::seek`] moves the position to the sample of the time asked for, limited
//!   to the timeline, and discards every prepared sample. Nothing prepared before a seek can be
//!   heard after it.
//! * **Underrun.** When fewer samples are prepared than the device asks for, the missing part of
//!   its buffer is silence and the clock stands still for it: the next real sample continues where
//!   the audio stopped, so a stall delays the audio and never skips it. [`PullReport::underrun`]
//!   counts those frames.
//! * **End of the timeline.** After the last sample, a pull is silence that is not an underrun.
//!   The clock stays at the end until a seek.

use crate::audio_mix::{mix_audio, AudioSource, MixReport};
use crate::audio_schedule::{audio_sample_count, sample_position, MIX_CHANNELS};
use crate::project::Project;
use crate::render::RenderError;
use crate::time::Time;
use std::collections::VecDeque;

/// The earliest time whose [`sample_position`] is `sample`, computed with integers only.
pub fn sample_time(sample: i64) -> Time {
    let ticks = (i128::from(sample) * 15 + 7).div_euclid(8);
    Time::from_ticks(ticks as i64)
}

/// What one [`AudioClock::pull`] did. The three counts add up to the frames asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PullReport {
    /// Frames of real audio handed to the device.
    pub played: usize,
    /// Frames of silence because the audio was not prepared in time.
    pub underrun: usize,
    /// Frames of silence after the end of the timeline.
    pub past_end: usize,
}

/// Tracks what a device has consumed and holds the samples prepared for it.
#[derive(Debug, Clone)]
pub struct AudioClock {
    position: i64,
    end: i64,
    /// Interleaved stereo samples from `position` on.
    prepared: VecDeque<f32>,
}

impl AudioClock {
    /// A clock at the start of `project`.
    pub fn new(project: &Project) -> Self {
        Self {
            position: 0,
            end: audio_sample_count(project),
            prepared: VecDeque::new(),
        }
    }

    /// The timeline sample the device consumes next.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// The time of [`AudioClock::position`], derived from the samples the device consumed.
    pub fn time(&self) -> Time {
        sample_time(self.position)
    }

    /// Frames mixed and waiting for the device.
    pub fn prepared_frames(&self) -> usize {
        self.prepared.len() / MIX_CHANNELS
    }

    /// Mixes up to `frames` more frames after what is prepared, stopping at the end of the
    /// timeline. Ask again until enough is prepared; a real device layer does this ahead of time.
    pub fn prepare<S: AudioSource + ?Sized>(
        &mut self,
        project: &Project,
        source: &mut S,
        frames: usize,
    ) -> Result<MixReport, RenderError> {
        let start = self.position + self.prepared_frames() as i64;
        let count = (frames as i64).min(self.end - start).max(0) as usize;
        let mut mixed = vec![0.0; count * MIX_CHANNELS];
        let report = mix_audio(project, source, start, &mut mixed)?;
        self.prepared.extend(mixed);
        Ok(report)
    }

    /// Fills `out` as the device consumes it, interleaved stereo; a trailing odd element is zeroed.
    pub fn pull(&mut self, out: &mut [f32]) -> PullReport {
        out.fill(0.0);
        let frames = out.len() / MIX_CHANNELS;
        let remaining = (self.end - self.position).max(0) as usize;
        let played = frames.min(self.prepared_frames()).min(remaining);
        for (target, sample) in out.iter_mut().zip(self.prepared.drain(..played * MIX_CHANNELS)) {
            *target = sample;
        }
        self.position += played as i64;
        let past_end = (frames - played).saturating_sub(remaining - played);
        PullReport {
            played,
            underrun: frames - played - past_end,
            past_end,
        }
    }

    /// Moves to the sample of `time`, limited to the timeline, and discards the prepared samples.
    pub fn seek(&mut self, time: Time) {
        self.position = sample_position(time).clamp(0, self.end);
        self.prepared.clear();
    }
}
