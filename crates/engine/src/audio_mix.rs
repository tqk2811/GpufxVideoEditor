//! Audio mixer: sums the segments of [`crate::audio_schedule`] into interleaved stereo.
//!
//! This module holds no FFmpeg or device code. A backend implements [`AudioSource`].

use crate::audio_schedule::{audio_segments, MIX_CHANNELS};
use crate::project::Project;
use crate::render::{ClipKey, RenderError};
use crate::stretch::StretchStates;

/// Decoded audio of the sources, as interleaved stereo `f32` at 48 kHz.
pub trait AudioSource {
    /// Fills `out` with `out.len() / 2` stereo frames of clip `key`, whose file is `source`,
    /// beginning at source sample `position`, which is never negative.
    ///
    /// Missing audio is not an error: `out` arrives zeroed, and whatever the source does not write
    /// (no audio stream, or the audio ended early) stays silent.
    fn read(
        &mut self,
        key: ClipKey,
        source: &str,
        position: i64,
        out: &mut [f32],
    ) -> Result<(), RenderError>;
}

/// What happened during one mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MixReport {
    /// Channel samples that were outside `[-1, 1]` and were clamped; a stereo frame can count twice.
    pub clamped_samples: u64,
}

/// Mixes the timeline from sample `start` into `out`, interleaved stereo: `out.len() / 2` frames,
/// and a trailing odd element is set to zero. Sums every segment, then clamps to `[-1, 1]`.
pub fn mix_audio<S: AudioSource + ?Sized>(
    project: &Project,
    source: &mut S,
    start: i64,
    out: &mut [f32],
) -> Result<MixReport, RenderError> {
    mix_audio_with(project, source, &mut StretchStates::default(), start, out)
}

/// Like [`mix_audio`], for a caller that mixes more blocks: `stretch` carries the work of the time
/// stretcher over to the next block, so sequential mixing stays cheap. The result does not depend
/// on it.
pub fn mix_audio_with<S: AudioSource + ?Sized>(
    project: &Project,
    source: &mut S,
    stretch: &mut StretchStates,
    start: i64,
    out: &mut [f32],
) -> Result<MixReport, RenderError> {
    out.fill(0.0);
    let frames = out.len() / MIX_CHANNELS;
    let mut scratch: Vec<f32> = Vec::new();
    let segments = audio_segments(project, start, frames as i64);
    stretch.retain(&segments.iter().filter(|segment| segment.stretch.is_some()).map(|segment| segment.key).collect::<Vec<_>>());
    for segment in segments {
        let len = segment.len as usize;
        scratch.clear();
        scratch.resize(len * MIX_CHANNELS, 0.0);
        if let Some(spec) = segment.stretch {
            stretch.render(source, &segment, spec, &mut scratch)?;
        } else {
            // Samples before the beginning of the source stay silent.
            let skip = segment.source_start.saturating_neg().clamp(0, segment.len) as usize;
            if skip < len {
                source.read(
                    segment.key,
                    segment.source,
                    segment.source_start.max(0),
                    &mut scratch[skip * MIX_CHANNELS..],
                )?;
            }
        }
        let offset = (segment.start - start) as usize * MIX_CHANNELS;
        let target = &mut out[offset..offset + len * MIX_CHANNELS];
        for (i, (from, to)) in scratch
            .chunks_exact(MIX_CHANNELS)
            .zip(target.chunks_exact_mut(MIX_CHANNELS))
            .enumerate()
        {
            let gain = segment.gain.gain_at(segment.start + i as i64);
            to[0] += from[0] * gain;
            to[1] += from[1] * gain;
        }
    }

    let mut report = MixReport::default();
    for sample in &mut out[..frames * MIX_CHANNELS] {
        // A NaN compares unequal to everything, so it is both silenced and counted.
        let clamped = if sample.is_nan() { 0.0 } else { sample.clamp(-1.0, 1.0) };
        if clamped != *sample {
            report.clamped_samples += 1;
        }
        *sample = clamped;
    }
    Ok(report)
}
