//! Audio scheduler: a pure function from a project and a sample range to the source segments to
//! mix, each with its gain.
//!
//! The mix format is 48 kHz stereo. All positions are `i64` sample counts and are computed from
//! ticks with integer arithmetic only; floating point appears only in gain values. The functions
//! assume a project that passed [`crate::validate`]; on an invalid project they do not panic but
//! the result is unspecified.
//!
//! # Rounding rules (defined once, here)
//!
//! * **Sample position.** Tick `t` maps to sample `floor(t * 8 / 15)`, see [`sample_position`].
//! * **Clip coverage.** A clip covers samples `[position(start), position(start + duration))`.
//!   Both ends round the same way, so touching clips neither leave a gap nor share a sample.
//! * **Source position.** Timeline sample `s` of a clip reads source sample
//!   `position(source_in) + (s - position(start))`, so a clip reads consecutive source samples.
//! * **Speed.** A clip at another speed reads around source sample
//!   `position(source_in) + floor((s - position(start)) * speed)`, through the time stretcher of
//!   [`crate::stretch`], which keeps the pitch. Its grains stay within 11 ms of that position, so
//!   the sound follows the picture; at speed 1 nothing here changes.
//! * **Transition window.** The same window the video uses (see [`crate::schedule`]), converted to
//!   samples: from `position(cut - duration / 2)` to `position(cut - duration / 2 + duration)`.
//!   Inside it both clips sound, each extended past its own end with the formula above. A source
//!   position below zero means there is no audio yet, which the mixer treats as silence.
//! * **Crossfade.** With `i` samples of an `n` sample window elapsed, the earlier clip has gain
//!   `cos(i / n * pi / 2)` and the later one `sin(i / n * pi / 2)` (equal power).
//! * **Fades.** Linear in amplitude and anchored to the clip's own start and end. `fade_in` gives
//!   `i / n` at `i` samples after the first clip sample. `fade_out` mirrors it, so the last clip
//!   sample has gain 0. A fade on the side of a transition is rejected by [`crate::validate`]
//!   (`FadeBesideTransition`), because it would silence part of the window; the scheduler assumes
//!   it does not occur.
//! * **Total length.** Audio stops with the last video frame, see [`audio_sample_count`].

use crate::project::{Clip, Project, Track, Transition};
use crate::render::ClipKey;
use crate::schedule::{frame_count, frame_time, transition_window};
use crate::stretch::Stretch;
use crate::time::Time;
use std::f64::consts::FRAC_PI_2;

/// Samples per second of the mix.
pub const MIX_SAMPLE_RATE: u32 = 48_000;
/// Channels of the mix.
pub const MIX_CHANNELS: usize = 2;

/// `floor(t * 8 / 15)`, the sample at time `t`. Exact for every `Time`: the product is computed in
/// 128 bits and its quotient is smaller in magnitude than `t`, so it always fits in an `i64`.
pub fn sample_position(t: Time) -> i64 {
    (i128::from(t.ticks()) * 8).div_euclid(15) as i64
}

/// Number of mixed samples per channel in the output: the position of the end of the last video
/// frame, so audio and video end together. A project without video tracks has 0.
pub fn audio_sample_count(project: &Project) -> i64 {
    let end = frame_time(project, frame_count(project)).unwrap_or(Time::MAX);
    sample_position(end)
}

/// A transition window in samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    start: i64,
    len: i64,
}

impl Window {
    fn end(self) -> i64 {
        self.start.saturating_add(self.len)
    }
}

/// How the gain of one clip changes over the timeline. Each value depends only on the sample, so
/// the result does not depend on how a render is split into blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainEnvelope {
    amplitude: f64,
    clip_start: i64,
    clip_end: i64,
    fade_in: i64,
    fade_out: i64,
    crossfade_in: Option<Window>,
    crossfade_out: Option<Window>,
}

impl GainEnvelope {
    /// The linear gain at timeline sample `sample`.
    pub fn gain_at(&self, sample: i64) -> f32 {
        let mut gain = self.amplitude;
        if self.fade_in > 0 {
            gain *= ramp(sample.saturating_sub(self.clip_start), self.fade_in);
        }
        if self.fade_out > 0 {
            gain *= ramp(
                self.clip_end.saturating_sub(1).saturating_sub(sample),
                self.fade_out,
            );
        }
        if let Some(window) = self.crossfade_in {
            let elapsed = sample.saturating_sub(window.start);
            if elapsed < window.len {
                gain *= (elapsed.max(0) as f64 / window.len as f64 * FRAC_PI_2).sin();
            }
        }
        if let Some(window) = self.crossfade_out {
            let elapsed = sample.saturating_sub(window.start);
            if elapsed >= window.len {
                return 0.0;
            }
            gain *= (elapsed.max(0) as f64 / window.len as f64 * FRAC_PI_2).cos();
        }
        gain as f32
    }
}

/// `i / len` limited to `0..=1`, for a positive `len`.
fn ramp(i: i64, len: i64) -> f64 {
    i.clamp(0, len) as f64 / len as f64
}

/// A run of consecutive samples of one clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSegment<'a> {
    /// The clip this segment belongs to.
    pub key: ClipKey,
    /// Source media path.
    pub source: &'a str,
    /// First timeline sample.
    pub start: i64,
    /// Number of samples per channel; always positive.
    pub len: i64,
    /// Source sample that plays at `start`. Negative when the segment begins before the source.
    pub source_start: i64,
    /// Gain of the segment.
    pub gain: GainEnvelope,
    /// Set for a clip at another speed: its samples come from the time stretcher, not from
    /// consecutive source samples starting at `source_start`.
    pub stretch: Option<Stretch>,
}

/// The segments sounding in the `len` samples from `start`, in track then clip order. Muted clips,
/// hidden tracks and samples no clip covers produce nothing.
pub fn audio_segments(project: &Project, start: i64, len: i64) -> Vec<AudioSegment<'_>> {
    let end = start.saturating_add(len.max(0));
    let mut segments = Vec::new();
    for (t, track) in project.tracks.iter().enumerate().filter(|(_, track)| !track.hidden) {
        for (c, clip) in track.clips.iter().enumerate() {
            // A still image has nothing to play, and its file is no audio source to open.
            if clip.muted || clip.is_image() {
                continue;
            }
            let crossfade_in = c
                .checked_sub(1)
                .and_then(|previous| find_transition(track, previous))
                .and_then(|tr| window_of(tr, clip));
            let crossfade_out = find_transition(track, c)
                .and_then(|tr| Some((tr, track.clips.get(c + 1)?)))
                .and_then(|(tr, next)| window_of(tr, next));

            let clip_start = sample_position(clip.start);
            let clip_end = sample_position(clip.start.saturating_add(clip.duration));
            let span_start = crossfade_in.map_or(clip_start, |w| w.start.min(clip_start));
            let span_end = crossfade_out.map_or(clip_end, |w| w.end().max(clip_end));

            let first = span_start.max(start);
            let last = span_end.min(end);
            if first >= last {
                continue;
            }
            let source_base = sample_position(clip.source_in);
            let stretch = clip.has_speed().then_some(Stretch {
                clip_start,
                source_base,
                speed_milli: clip.speed_milli(),
            });
            segments.push(AudioSegment {
                key: ClipKey { track: t, clip: c },
                source: &clip.source,
                start: first,
                len: last - first,
                source_start: stretch.map_or_else(
                    || source_base.saturating_add(first.saturating_sub(clip_start)),
                    |stretch| stretch.nominal(first.saturating_sub(clip_start)),
                ),
                gain: envelope(clip, clip_start, clip_end, crossfade_in, crossfade_out),
                stretch,
            });
        }
    }
    segments
}

fn find_transition(track: &Track, after_clip: usize) -> Option<&Transition> {
    track.transitions.iter().find(|tr| tr.after_clip == after_clip)
}

/// The window of `transition`, whose later clip is `next`. `None` when it is shorter than a sample.
fn window_of(transition: &Transition, next: &Clip) -> Option<Window> {
    let (start_time, end_time) = transition_window(transition, next);
    let start = sample_position(start_time);
    let end = sample_position(end_time);
    (end > start).then_some(Window {
        start,
        len: end - start,
    })
}

fn envelope(
    clip: &Clip,
    clip_start: i64,
    clip_end: i64,
    crossfade_in: Option<Window>,
    crossfade_out: Option<Window>,
) -> GainEnvelope {
    let clip_end_time = clip.start.saturating_add(clip.duration);
    GainEnvelope {
        amplitude: 10f64.powf(clip.gain_db / 20.0),
        clip_start,
        clip_end,
        fade_in: sample_position(clip.start.saturating_add(clip.fade_in)) - clip_start,
        fade_out: clip_end - sample_position(clip_end_time.saturating_sub(clip.fade_out)),
        crossfade_in,
        crossfade_out,
    }
}
