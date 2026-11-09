//! Pitch-preserving time stretch for clips at another speed: WSOLA over decoded stereo.
//!
//! # Why this is not a filter
//!
//! FFmpeg's `atempo` would need libavfilter in both builds, a chain of stages below 0.5x, and a
//! graph restart after every seek. The result is also the only state the mixer would keep across
//! blocks. Here the output is a function of the source audio and the clip alone, with no history:
//! a block mixed after a seek, or split differently, is bit for bit what a sequential export
//! writes, as long as the source gives the same samples for a position however it is reached. The
//! file decoder does for 48 kHz sources; after a seek it restarts the resampler for the others, so
//! those can differ slightly from a sequential read.
//!
//! # Method
//!
//! A grain of [`GRAIN`] samples is laid down every [`HOP`] timeline samples of the clip, under a
//! Hann window, so the windows add up to one. Grain `m` is centred at timeline sample `m * HOP` of
//! the clip and reads the source around its nominal position `floor(m * HOP * speed)`; picture
//! and sound therefore share one mapping and cannot drift. Within [`SEARCH`] samples of that
//! position, the grain starts where it best continues the previous grain.
//!
//! A grain reaching outside the source adds nothing, and the samples it would have covered are
//! scaled back up by the window weights of the grains that do reach them, so the level holds up
//! to the very first and last sample of the file.
//!
//! Every [`KEY_INTERVAL`] grains the position is the nominal one, so the alignment of any grain
//! depends on at most [`KEY_INTERVAL`] grains before it, and a seek replays only those. Grain
//! positions are kept between blocks, which is only memoisation: rebuilding them gives the same
//! numbers. A clip cut in two starts a new grid at each half. A key grain is not in phase with
//! the grain before it, and where the two overlap the level is held by dividing out what their
//! correlation says they cancel, so the join is a phase jump and no dip.

use std::collections::{HashMap, VecDeque};
use std::f32::consts::PI;

use crate::audio_mix::AudioSource;
use crate::audio_schedule::{AudioSegment, MIX_CHANNELS};
use crate::project::SPEED_SCALE;
use crate::render::{ClipKey, RenderError};

/// Length of a grain: about 43 ms.
const GRAIN: i64 = 2048;
/// Distance between grains: half a grain, so the Hann windows add up to one.
const HOP: i64 = GRAIN / 2;
/// How far from its nominal position a grain may start: about ±10 ms, half the period of 47 Hz.
const SEARCH: i64 = 512;
/// A grain that is not aligned to the previous one, every 64 grains (1.4 s).
const KEY_INTERVAL: i64 = 64;
/// The search compares this one sample out of [`DECIMATE`], and refines at full rate.
const DECIMATE: i64 = 4;
/// Grain positions kept between blocks: the last ones a following block may still need.
const KEPT_POSITIONS: usize = 3;
/// Below this sum of window weights a sample is not boosted further, only tapered: there the only
/// valid grain is itself nearly silent.
const MIN_WEIGHT: f32 = 0.01;
/// From this sum of window weights the sample is complete.
const FULL_WEIGHT: f32 = 0.999;
/// The strongest correction of the join at a key grain, a limit on how far a cancellation is undone.
const MAX_JOIN_GAIN: f32 = 8.0;

/// Where a stretched segment reads from. Equal specs produce equal audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stretch {
    /// Timeline sample of the first sample of the clip.
    pub clip_start: i64,
    /// Source sample of the first sample of the clip.
    pub source_base: i64,
    /// The speed, in [`SPEED_SCALE`] parts.
    pub speed_milli: i64,
}

impl Stretch {
    /// The nominal source sample of timeline sample `k` of the clip.
    pub fn nominal(&self, k: i64) -> i64 {
        let product = i128::from(k) * i128::from(self.speed_milli);
        self.source_base.saturating_add(product.div_euclid(i128::from(SPEED_SCALE)) as i64)
    }
}

/// The audio a clip has decoded and the grain positions found so far.
struct ClipStretch {
    spec: Stretch,
    source: String,
    /// Stereo samples of the source from `window_start`. Reads only go forward from its end.
    window: Vec<f32>,
    window_start: i64,
    /// Positions of the latest grains, ascending: `(grain, centre in the source)`.
    positions: VecDeque<(i64, i64)>,
    /// The first frame past the end of the source, once a read has reached it.
    source_end: Option<i64>,
    reference: Vec<f32>,
    candidates: Vec<f32>,
    /// The window weights that went into each output sample.
    weights: Vec<f32>,
    /// The key grains of this block with what came before them: `(grain, correlation)`.
    joins: Vec<(i64, f32)>,
}

/// The stretchers of the clips being mixed. Keep one for a whole render or playback.
#[derive(Default)]
pub struct StretchStates {
    clips: HashMap<ClipKey, ClipStretch>,
}

impl StretchStates {
    /// Forgets the clips that are not in `keep`, which have stopped sounding.
    pub(crate) fn retain(&mut self, keep: &[ClipKey]) {
        self.clips.retain(|key, _| keep.contains(key));
    }

    /// Writes the `segment.len` stretched frames of `segment` to `out`, interleaved and zeroed.
    pub(crate) fn render<S: AudioSource + ?Sized>(&mut self, source: &mut S, segment: &AudioSegment<'_>, spec: Stretch, out: &mut [f32]) -> Result<(), RenderError> {
        let state = self.clips.entry(segment.key).or_insert_with(|| ClipStretch::new(spec, segment.source));
        if state.spec != spec || state.source != segment.source {
            *state = ClipStretch::new(spec, segment.source);
        }
        state.render(source, segment.key, segment.start, segment.len, out)
    }
}

impl ClipStretch {
    fn new(spec: Stretch, source: &str) -> Self {
        Self { spec, source: source.to_owned(), window: Vec::new(), window_start: 0, positions: VecDeque::new(), source_end: None, reference: Vec::new(), candidates: Vec::new(), weights: Vec::new(), joins: Vec::new() }
    }

    fn nominal(&self, grain: i64) -> i64 {
        self.spec.nominal(grain.saturating_mul(HOP))
    }

    fn render<S: AudioSource + ?Sized>(&mut self, source: &mut S, key: ClipKey, timeline_start: i64, frames: i64, out: &mut [f32]) -> Result<(), RenderError> {
        let begin = timeline_start.saturating_sub(self.spec.clip_start);
        let end = begin.saturating_add(frames);
        // The first grain reaching past `begin`, and the last one starting before `end`.
        let first = (begin - GRAIN / 2).div_euclid(HOP) + 1;
        let last = (end + GRAIN / 2 - 1).div_euclid(HOP);
        let mut weights = std::mem::take(&mut self.weights);
        weights.clear();
        weights.resize(frames as usize, 0.0);
        let mut joins = std::mem::take(&mut self.joins);
        joins.clear();
        let mut before = None;
        for grain in first..=last {
            let centre = self.position_of(source, key, grain)?;
            let from = centre - GRAIN / 2;
            self.ensure(source, key, from, from + GRAIN)?;
            if let (0, Some(previous)) = (grain.rem_euclid(KEY_INTERVAL), before) {
                joins.push((grain, self.join_correlation(previous, centre)));
            }
            before = Some(centre);
            let output_from = grain * HOP - GRAIN / 2;
            for i in 0..GRAIN {
                let timeline = output_from + i;
                if timeline < begin || timeline >= end {
                    continue;
                }
                let Some((left, right)) = self.valid_frame(from + i) else { continue };
                let weight = hann(i);
                let index = (timeline - begin) as usize;
                let at = index * MIX_CHANNELS;
                out[at] += left * weight;
                out[at + 1] += right * weight;
                weights[index] += weight;
            }
        }
        // A grain reaching outside the source adds nothing, so the windows of the others no longer
        // add up to one. Silence stays silence: with no valid grain the sum is zero.
        for (frame, &total) in out.chunks_exact_mut(MIX_CHANNELS).zip(&weights) {
            if total < FULL_WEIGHT {
                let scale = 1.0 / total.max(MIN_WEIGHT);
                frame[0] *= scale;
                frame[1] *= scale;
            }
        }
        for &(grain, correlation) in &joins {
            apply_join_gain(out, begin, grain, correlation);
        }
        self.weights = weights;
        self.joins = joins;
        self.trim();
        Ok(())
    }

    /// The source centre of `grain`, replaying from the last known position at or after its key.
    fn position_of<S: AudioSource + ?Sized>(&mut self, source: &mut S, key: ClipKey, grain: i64) -> Result<i64, RenderError> {
        if let Some(&(_, centre)) = self.positions.iter().find(|&&(known, _)| known == grain) {
            return Ok(centre);
        }
        let start = grain - grain.rem_euclid(KEY_INTERVAL);
        let resume = self.positions.back().copied().filter(|&(known, _)| known >= start && known < grain);
        let (mut next, mut previous) = match resume {
            Some((known, centre)) => (known + 1, centre),
            // A key right after the grain held last: that grain may be asked for again by the next
            // block, so its audio and position stay. Reading up to the key here keeps the reads
            // contiguous instead of jumping over what the grain before it does not need.
            None if grain == start && self.positions.back().is_some_and(|&(known, _)| known == grain - 1) => {
                let centre = self.nominal(start);
                if let Some(&(_, held)) = self.positions.back() {
                    self.ensure(source, key, held - GRAIN / 2, centre + GRAIN / 2)?;
                }
                self.remember(start, centre);
                return Ok(centre);
            }
            None => {
                self.positions.clear();
                let centre = self.nominal(start);
                self.remember(start, centre);
                (start + 1, centre)
            }
        };
        while next <= grain {
            previous = self.aligned(source, key, next, previous)?;
            self.remember(next, previous);
            next += 1;
            self.trim();
        }
        Ok(previous)
    }

    fn remember(&mut self, grain: i64, centre: i64) {
        self.positions.push_back((grain, centre));
        while self.positions.len() > KEPT_POSITIONS {
            self.positions.pop_front();
        }
    }

    /// The centre of `grain` near its nominal position that best continues the grain `previous`.
    fn aligned<S: AudioSource + ?Sized>(&mut self, source: &mut S, key: ClipKey, grain: i64, previous: i64) -> Result<i64, RenderError> {
        let nominal = self.nominal(grain);
        let reference_start = previous + HOP - GRAIN / 2;
        let search_start = nominal - SEARCH - GRAIN / 2;
        self.ensure(source, key, reference_start.min(search_start), (reference_start + GRAIN).max(search_start + GRAIN + 2 * SEARCH))?;
        let points = GRAIN / DECIMATE;
        let mut reference = std::mem::take(&mut self.reference);
        let mut candidates = std::mem::take(&mut self.candidates);
        self.mono(reference_start, points, &mut reference);
        self.mono(search_start, points + 2 * SEARCH / DECIMATE, &mut candidates);
        // Near an end of the source the search only moves the grain further into it, so that
        // aligning never trades away audio the grain could have read.
        let low = (GRAIN / 2 + SEARCH - nominal).clamp(0, SEARCH);
        let high = self.source_end.map_or(2 * SEARCH, |end| (end - GRAIN / 2 + SEARCH - nominal).clamp(SEARCH, 2 * SEARCH));
        let energy: f32 = reference.iter().map(|sample| sample * sample).sum();
        let mut best = (f32::MIN, SEARCH);
        if energy > 1e-9 {
            for lag in (low + DECIMATE - 1) / DECIMATE..=high / DECIMATE {
                let score = correlation(&reference, &candidates[lag as usize..(lag + points) as usize]);
                if score > best.0 {
                    best = (score, lag * DECIMATE);
                }
            }
            // A sample of the search grid is four of the mix, so look a little around it.
            let (coarse, mut refined) = (best.1, best.1);
            let mut top = f32::MIN;
            for fine in (coarse - DECIMATE + 1).max(low)..=(coarse + DECIMATE - 1).min(high) {
                let score = self.full_rate_correlation(reference_start, search_start + fine);
                if score > top {
                    top = score;
                    refined = fine;
                }
            }
            best.1 = refined;
        }
        self.reference = reference;
        self.candidates = candidates;
        Ok(nominal - SEARCH + best.1)
    }

    /// How alike grain `centre` and the grain before it, at `previous`, sound where they overlap,
    /// from -1 to 1. Silence counts as alike, so that it is left alone.
    fn join_correlation(&self, previous: i64, centre: i64) -> f32 {
        let (mut dot, mut before_energy, mut after_energy) = (0.0, 0.0, 0.0);
        for offset in (0..HOP).step_by(2) {
            let (before, after) = (self.mono_sample(previous + offset), self.mono_sample(centre - HOP + offset));
            dot += before * after;
            before_energy += before * before;
            after_energy += after * after;
        }
        if before_energy < 1e-9 || after_energy < 1e-9 {
            return 1.0;
        }
        dot / (before_energy * after_energy).sqrt()
    }

    fn full_rate_correlation(&self, reference_start: i64, candidate_start: i64) -> f32 {
        let mut dot = 0.0;
        let mut energy = 1e-12;
        for i in (0..GRAIN).step_by(2) {
            let candidate = self.mono_sample(candidate_start + i);
            dot += self.mono_sample(reference_start + i) * candidate;
            energy += candidate * candidate;
        }
        dot / energy.sqrt()
    }

    /// Mono decimated by [`DECIMATE`], from `start`.
    fn mono(&self, start: i64, points: i64, out: &mut Vec<f32>) {
        out.clear();
        for point in 0..points {
            let from = start + point * DECIMATE;
            let total: f32 = (from..from + DECIMATE).map(|at| self.mono_sample(at)).sum();
            out.push(total / DECIMATE as f32);
        }
    }

    fn mono_sample(&self, at: i64) -> f32 {
        let (left, right) = self.frame(at);
        (left + right) * 0.5
    }

    /// The stereo source frame at `at`, or silence outside what the source has.
    fn frame(&self, at: i64) -> (f32, f32) {
        self.valid_frame(at).unwrap_or((0.0, 0.0))
    }

    /// The stereo source frame at `at`, if the source has one there.
    fn valid_frame(&self, at: i64) -> Option<(f32, f32)> {
        let index = at.saturating_sub(self.window_start).saturating_mul(MIX_CHANNELS as i64);
        if index < 0 || index as usize + 1 >= self.window.len() || self.source_end.is_some_and(|end| at >= end) {
            return None;
        }
        Some((self.window[index as usize], self.window[index as usize + 1]))
    }

    /// Makes source frames `[from, to)` available, reading forward only.
    fn ensure<S: AudioSource + ?Sized>(&mut self, source: &mut S, key: ClipKey, from: i64, to: i64) -> Result<(), RenderError> {
        let (from, to) = (from.max(0), to.max(0));
        let window_end = self.window_start + (self.window.len() / MIX_CHANNELS) as i64;
        if self.window.is_empty() || from < self.window_start || from > window_end {
            self.window.clear();
            self.window_start = from;
            return self.read_forward(source, key, from, to);
        }
        self.read_forward(source, key, window_end, to)
    }

    fn read_forward<S: AudioSource + ?Sized>(&mut self, source: &mut S, key: ClipKey, from: i64, to: i64) -> Result<(), RenderError> {
        if to <= from {
            return Ok(());
        }
        let at = self.window.len();
        self.window.resize(at + (to - from) as usize * MIX_CHANNELS, 0.0);
        let written = source.read_counted(key, &self.source, from, &mut self.window[at..])? as i64;
        if written < to - from {
            let end = from + written;
            self.source_end = Some(self.source_end.map_or(end, |known| known.min(end)));
        }
        Ok(())
    }

    /// Drops source audio no retained grain needs.
    fn trim(&mut self) {
        let Some(&(_, oldest)) = self.positions.front() else { return };
        let keep_from = (oldest - GRAIN / 2 - 2 * SEARCH).max(self.window_start);
        let drop_frames = (keep_from - self.window_start) as usize;
        let drop_samples = (drop_frames * MIX_CHANNELS).min(self.window.len());
        self.window.drain(..drop_samples);
        self.window_start = keep_from;
    }
}

/// The periodic Hann window at sample `i` of a grain.
fn hann(i: i64) -> f32 {
    0.5 - 0.5 * (2.0 * PI * i as f32 / GRAIN as f32).cos()
}

/// Keeps the level through the join at key grain `grain`, whose alignment does not depend on the
/// grain before it. Two grains that are not in phase cancel in their overlap: with correlation `c`
/// their windows `a` and `b` sum to a power of `a * a + b * b + 2 * a * b * c`, which this divides
/// back out. A phase jump remains, but no dip in the level.
fn apply_join_gain(out: &mut [f32], begin: i64, grain: i64, correlation: f32) {
    let overlap_start = grain * HOP - GRAIN / 2;
    let frames = (out.len() / MIX_CHANNELS) as i64;
    for timeline in overlap_start.max(begin)..(overlap_start + HOP).min(begin + frames) {
        let offset = timeline - overlap_start;
        let (before, after) = (hann(offset + HOP), hann(offset));
        let power = before * before + after * after + 2.0 * before * after * correlation;
        let gain = (1.0 / power.max(0.0).sqrt()).min(MAX_JOIN_GAIN);
        let at = (timeline - begin) as usize * MIX_CHANNELS;
        out[at] *= gain;
        out[at + 1] *= gain;
    }
}

/// How alike two blocks are, not caring about the level of `candidate`.
fn correlation(reference: &[f32], candidate: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut energy = 1e-12;
    for (a, b) in reference.iter().zip(candidate) {
        dot += a * b;
        energy += b * b;
    }
    dot / energy.sqrt()
}
