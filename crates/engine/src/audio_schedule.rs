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
//!   `cos(i / n * pi / 2)` and the later one `sin(i / n * pi / 2)` (equal power). The window is
//!   the part of the transition window where both clips have audio: the earlier clip past its out
//!   point while its file lasts, the later one before its in point back to the start of its file
//!   (the length of a file comes from [`crate::AudioSource::source_len`]; unknown means audio
//!   everywhere). Before that part the earlier clip plays at full gain and the later one is
//!   silent, after it the other way round, so a clip without audio beyond its in or out point does
//!   not fade towards silence. Nothing moves in time, so sync is kept. Two clips with less than
//!   [`ANTI_CLICK_SAMPLES`] of audio in common (two whole files joined) get a cut: the earlier one
//!   ramps down over the last `ANTI_CLICK_SAMPLES` of its audio and the later one up over the first
//!   of its, with the same curves. Both clips are still read over the whole transition window.
//! * **Fades.** Linear in amplitude and anchored to the clip's own start and end. `fade_in` gives
//!   `i / n` at `i` samples after the first clip sample. `fade_out` mirrors it, so the last clip
//!   sample has gain 0. A fade on the side of a transition is rejected by [`crate::validate`]
//!   (`FadeBesideTransition`), because it would silence part of the window; the scheduler assumes
//!   it does not occur.
//! * **Reverse.** A reversed clip reads a mirrored source: source sample `v` of the formulas
//!   above is file sample `mirror - v`, with `mirror = position(source_in) + position(source_end) - 1`,
//!   so the clip plays the samples of its source range from the last to the first. A mirrored
//!   position below zero (past `source_end` in the file) is silence; one past `mirror` (before
//!   the file begins) is the end of the source. Everything below sees only the mirrored audio, so
//!   noise reduction, stretch, pitch and voice apply to the reversed sound.
//! * **Voice.** Per clip, in this order: noise reduction of the source audio ([`crate::denoise`]),
//!   the stretch with the pitch shift ([`crate::stretch`]), the voice effect ([`VoiceFx`]), and
//!   last the gain and fades.
//! * **Total length.** Audio stops with the last video frame, see [`audio_sample_count`].

use crate::denoise::DenoiseSpec;
use crate::project::{Clip, Project, Track, Transition, VoiceEffect, ROBOT_HZ};
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
/// frame, so audio and video end together. The frame count covers audio clips too (see
/// [`frame_count`]), so the end of the last audio clip is always inside. An empty project has 0.
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
            if elapsed < 0 {
                return 0.0;
            }
            if elapsed < window.len {
                gain *= (elapsed as f64 / window.len as f64 * FRAC_PI_2).sin();
            }
        }
        if let Some(window) = self.crossfade_out {
            let elapsed = sample.saturating_sub(window.start);
            if elapsed >= window.len {
                return 0.0;
            }
            if elapsed > 0 {
                gain *= (elapsed as f64 / window.len as f64 * FRAC_PI_2).cos();
            }
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
    /// Set for a clip with noise reduction, which applies to its source audio before the stretch.
    pub denoise: Option<DenoiseSpec>,
    /// Set for a reversed clip: the mirror of its source positions. Every source position of this
    /// segment, `source_start` and the stretch included, counts in the mirrored source, whose
    /// sample `v` is file sample `mirror - v`; see the module rules.
    pub reverse: Option<i64>,
    /// Set for a clip with a voice effect, which applies after the stretch and before the gain.
    pub voice_effect: Option<VoiceFx>,
}

/// A voice effect over the timeline samples of one clip. Each value depends only on the sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceFx {
    /// Which effect; never [`VoiceEffect::None`].
    pub effect: VoiceEffect,
}

impl VoiceFx {
    /// The factor the effect multiplies timeline sample `sample` by.
    pub fn factor_at(&self, sample: i64) -> f32 {
        match self.effect {
            VoiceEffect::None => 1.0,
            // Ring modulation; the phase is taken modulo a whole period so that it stays exact far
            // into a long project. It follows the timeline sample, not the start of the clip, so
            // the two halves of a split clip go on in phase and the cut does not click.
            VoiceEffect::Robot => {
                let period = (f64::from(MIX_SAMPLE_RATE) / ROBOT_HZ).round() as i64;
                let phase = sample.rem_euclid(period) as f64 / period as f64;
                (phase * std::f64::consts::TAU).sin() as f32
            }
        }
    }
}

/// The segments sounding in the `len` samples from `start`, in track then clip order. Muted clips,
/// hidden tracks and samples no clip covers produce nothing.
pub fn audio_segments(project: &Project, start: i64, len: i64) -> Vec<AudioSegment<'_>> {
    audio_segments_with(project, start, len, &mut |_, _| None)
}

/// Like [`audio_segments`], with `source_len` telling how many samples the file of a clip holds
/// (see [`crate::AudioSource::source_len`]), so a crossfade fits the audio both clips have.
pub fn audio_segments_with<'a>(
    project: &'a Project,
    start: i64,
    len: i64,
    source_len: &mut dyn FnMut(ClipKey, &str) -> Option<i64>,
) -> Vec<AudioSegment<'a>> {
    let end = start.saturating_add(len.max(0));
    let mut segments = Vec::new();
    for (t, track) in project.tracks.iter().enumerate().filter(|(_, track)| !track.hidden) {
        for (c, clip) in track.clips.iter().enumerate() {
            // A still image or text has nothing to play, and its source is no audio file to open.
            if clip.muted || clip.is_still() {
                continue;
            }
            let crossfade_in = c
                .checked_sub(1)
                .and_then(|previous| find_transition(track, previous))
                .and_then(|tr| window_of(tr, clip));
            let crossfade_out = find_transition(track, c)
                .and_then(|tr| Some((tr, track.clips.get(c + 1)?)))
                .and_then(|(tr, next)| window_of(tr, next));

            // The holds of a clip are silent: only its moving part sounds.
            let (media_start, media_end) = clip.media_span();
            let clip_start = sample_position(media_start);
            let clip_end = sample_position(media_end);
            let span_start = crossfade_in.map_or(clip_start, |w| w.start.min(clip_start));
            let span_end = crossfade_out.map_or(clip_end, |w| w.end().max(clip_end));

            let first = span_start.max(start);
            let last = span_end.min(end);
            if first >= last {
                continue;
            }
            // The gains follow the part of each window where both clips have audio.
            let crossfade_in = crossfade_in.map(|w| fitted(w, t, track, c - 1, source_len).1);
            let crossfade_out = crossfade_out.map(|w| fitted(w, t, track, c, source_len).0);
            let source_base = sample_position(clip.source_in);
            let stretch = (clip.has_speed() || clip.voice.pitch_cents() != 0).then_some(Stretch {
                clip_start,
                source_base,
                speed_milli: clip.speed_milli(),
                pitch_cents: clip.voice.pitch_cents(),
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
                denoise: DenoiseSpec::of(&clip.denoise),
                reverse: clip.plays_reversed().then(|| sample_position(clip.source_in).saturating_add(sample_position(clip.source_end())).saturating_sub(1)),
                voice_effect: (clip.voice.effect != VoiceEffect::None).then_some(VoiceFx { effect: clip.voice.effect }),
            });
        }
    }
    segments
}

/// The file sample clip `clip` reads at timeline sample `sample`, by the rules of the module (the
/// nominal position for a clip at another speed). Outside `0..length of the file` there is none.
fn file_position(clip: &Clip, sample: i64) -> i64 {
    let clip_start = sample_position(clip.media_span().0);
    let source_base = sample_position(clip.source_in);
    let elapsed = sample.saturating_sub(clip_start);
    let v = if clip.has_speed() || clip.voice.pitch_cents() != 0 {
        Stretch { clip_start, source_base, speed_milli: clip.speed_milli(), pitch_cents: clip.voice.pitch_cents() }.nominal(elapsed)
    } else {
        source_base.saturating_add(elapsed)
    };
    if clip.plays_reversed() {
        sample_position(clip.source_in).saturating_add(sample_position(clip.source_end())).saturating_sub(1).saturating_sub(v)
    } else {
        v
    }
}

/// The first sample of `lo..hi` where `holds` is true, or `hi`; `holds` turns true once and stays.
fn first_where(mut lo: i64, mut hi: i64, holds: impl Fn(i64) -> bool) -> i64 {
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if holds(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// Samples of the ramps of a cut without common audio, about 2.7 ms: short enough to keep the cut,
/// long enough not to click.
pub const ANTI_CLICK_SAMPLES: i64 = 128;

/// The windows the earlier and the later clip of transition window `window`, between clip
/// `earlier` of track `t` and the next, fade out and in over: the part where both clips have
/// audio (see the module rules). When that part is shorter than [`ANTI_CLICK_SAMPLES`], the
/// earlier clip ramps down over the last of its audio and the later one up over the first of its.
fn fitted(window: Window, t: usize, track: &Track, earlier: usize, source_len: &mut dyn FnMut(ClipKey, &str) -> Option<i64>) -> (Window, Window) {
    let (a, b) = (&track.clips[earlier], &track.clips[earlier + 1]);
    let a_len = source_len(ClipKey { track: t, clip: earlier }, &a.source);
    let b_len = source_len(ClipKey { track: t, clip: earlier + 1 }, &b.source);
    let has_audio = |clip: &Clip, len: Option<i64>, sample: i64| {
        let position = file_position(clip, sample);
        position >= 0 && len.is_none_or(|len| position < len)
    };
    // The earlier clip has audio up to its own end at least; look for where it stops after that.
    let a_clip_end = sample_position(a.media_span().1).clamp(window.start, window.end());
    let a_end = first_where(a_clip_end, window.end(), |s| !has_audio(a, a_len, s));
    // The later clip has audio from its own start at least; look for where it begins before that.
    let b_clip_start = sample_position(b.media_span().0).clamp(window.start, window.end());
    let b_start = first_where(window.start, b_clip_start, |s| has_audio(b, b_len, s));
    if a_end - b_start >= ANTI_CLICK_SAMPLES {
        let common = Window { start: b_start, len: a_end - b_start };
        (common, common)
    } else {
        let ramp = ANTI_CLICK_SAMPLES;
        (Window { start: a_end - ramp, len: ramp }, Window { start: b_start, len: ramp })
    }
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
    let (media_start, clip_end_time) = clip.media_span();
    GainEnvelope {
        amplitude: 10f64.powf(clip.gain_db / 20.0),
        clip_start,
        clip_end,
        fade_in: sample_position(media_start.saturating_add(clip.fade_in)) - clip_start,
        fade_out: clip_end - sample_position(clip_end_time.saturating_sub(clip.fade_out)),
        crossfade_in,
        crossfade_out,
    }
}
