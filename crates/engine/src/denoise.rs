//! Noise reduction of a clip's source audio, as a layer over an [`AudioSource`].
//!
//! # Determinism
//!
//! The output at a source position must not depend on where processing began, so that preview
//! and export are identical from any seek point. The two methods get there differently.
//!
//! The spectral gate (general mode) is stateless: its windows sit on a grid of absolute source
//! positions, and the gain of a window depends only on the windows around it and on the noise
//! profile at its position, which is itself a function of the source audio around it (see
//! [Spectral gate](#spectral-gate)). Each chunk of [`CHUNK`] samples is computed on its own,
//! the windows that straddle a chunk boundary are computed identically for both chunks, and the
//! overlap-add joins them exactly: no preroll and no crossfade.
//!
//! RNNoise (voice mode) carries recurrent state from one frame to the next. Its processing is cut into
//! fixed chunks of [`CHUNK`] samples on a grid of absolute source positions. Chunk `j` is produced
//! by a fresh processor that starts [`PREROLL`] samples before it (silence before the file) and
//! runs [`CROSSFADE`] samples past it. The first [`CROSSFADE`] samples of a chunk blend the tail
//! of the run of chunk `j - 1` into the run of chunk `j`, so the restart is inaudible. Both runs
//! process the same audio and nearly agree there, so the two gains of the blend sum to one: an
//! equal-power blend would raise the level 3 dB in its middle, once per second. A sample
//! therefore depends only on the source audio, the method and the strength: a block read after a
//! seek is bit for bit what a sequential export reads, as long as the source gives the same
//! samples for a position however it is reached (see [`crate::stretch`]). Runs are kept between
//! reads, which is only memoisation, and the source is read forward only.
//!
//! Runs are processed at full strength and keep the dry audio beside the wet; the strength only
//! mixes the two when a block is read (see [`mix_weight`]), so a new strength keeps every run.
//!
//! The price is work: a voice chunk costs `(PREROLL + CHUNK + CROSSFADE) / CHUNK`, about 1.5
//! times the audio, and a seek computes two runs before the first sample. Half a second of
//! preroll lets the recurrent state of RNNoise settle; a longer one only costs more after each
//! seek. The restart still leaves the residual noise of steady noise about 2 dB higher at the
//! start of each chunk than at its end.
//!
//! # Stereo
//!
//! RNNoise works on mono 48 kHz frames of 480 samples. Each channel gets its own network, so the
//! stereo image stays; mid/side would leave the noise of the side channel untouched. The spectral
//! gate computes one gain per frequency from the power of both channels and applies it to both,
//! which removes noise without moving sounds between left and right.
//!
//! # Spectral gate
//!
//! Short-time Fourier transform with 1024-sample square-root Hann windows at half overlap; window
//! `w` starts at source sample `w * GATE_HOP`. A bin is turned down by how far its power, averaged
//! over the five windows around it, stands above the noise profile, to at most 30 dB at strength
//! 1. Steady broadband noise (hiss, fans, rain) goes; narrow tones such as mains hum are kept.
//!
//! The noise profile is not tracked over time, which would make it depend on where tracking
//! began (and, restarted per chunk, made the residual noise rise and fall 6 dB once per second).
//! It is estimated from fixed stretches of the source instead:
//!
//! * Per chunk of the source, each bin keeps the 20th percentile of its power over the windows
//!   that start in the chunk. Quiet moments between words set it, not the words.
//! * At the start of chunk `b`, the profile is the per-bin median of that statistic over the
//!   [`PROFILE_SPAN`] chunks around it, then the median over the 17 bins around each bin, so a
//!   steady tone, which fills only a few bins, does not count as noise. A factor brings the
//!   percentile up to the mean power of noise.
//! * A window takes the profile at its centre, interpolated linearly between the two chunk
//!   starts around it, so the profile glides and never steps.
//!
//! The profile therefore follows noise that changes over a few seconds, depends on the source file
//! and not on the clip (both halves of a split clip sound as before), and needs no pass over the
//! whole file: a 10-minute file would have to be decoded completely before the first sample. The
//! price is reading ahead: a chunk needs the source from [`PROFILE_SPAN`] / 2 + 1 chunks before
//! it to as many after it, which a seek decodes before its first sample.

use std::collections::{BTreeMap, VecDeque};
use std::f32::consts::PI;
use std::sync::{Arc, OnceLock};

use nnnoiseless::{DenoiseState, RnnModel};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::audio_mix::AudioSource;
use crate::audio_schedule::MIX_CHANNELS;
use crate::project::{ClipDenoise, DenoiseMode};
use crate::render::{ClipKey, RenderError};

/// Samples of one chunk: one second. A multiple of the RNNoise frame. It need not be a multiple of
/// the gate hop, because the gate windows sit on their own grid of source positions.
pub const CHUNK: i64 = 48_000;
/// Samples each voice chunk is processed ahead of its start: half a second.
pub const PREROLL: i64 = 24_000;
/// Samples over which consecutive voice chunks are blended: 10 ms.
pub const CROSSFADE: i64 = 480;
/// Runs and chunks kept between reads.
const KEPT_CHUNKS: usize = 3;

/// The RNNoise frame.
const VOICE_FRAME: usize = DenoiseState::FRAME_SIZE;
/// Samples by which RNNoise output lags its input: one frame, from its overlapping windows.
pub const VOICE_DELAY: usize = VOICE_FRAME;
/// RNNoise takes samples scaled like 16-bit integers.
const PCM_SCALE: f32 = 32_768.0;

/// The window of the spectral gate: 1024 samples, 47 Hz per bin.
const GATE_SIZE: usize = 1024;
/// Hop of the spectral gate: half a window, so the square-root Hann windows add up to one.
const GATE_HOP: usize = GATE_SIZE / 2;
/// Bins either side whose median makes the noise profile of a bin.
const GATE_MEDIAN_RADIUS: usize = 8;
/// The percentile of the power of a bin over the windows of a chunk that estimates its noise.
const GATE_PERCENTILE: f32 = 0.2;
/// The power of a noise bin is the mean of two squared Gaussian magnitudes, a gamma distribution
/// of shape 2 whose 20th percentile lies at 0.41 times its mean; this brings it back to the mean.
const GATE_BIAS: f32 = 2.43;
/// How far above the noise profile a bin must stand before it is kept whole, as a factor.
const GATE_OVER: f32 = 2.0;
/// The deepest cut at strength 1, in decibels.
const GATE_DEPTH_DB: f32 = 30.0;
/// Weights of the windows `w - 2 ..= w + 2` whose power sets the gain of window `w`: a fixed
/// smoothing over time against musical noise, reaching 25 ms either side.
const GATE_SMOOTHING: [f32; 5] = [1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];
/// Chunks whose statistics make the profile at the start of chunk `b`: `b - 2 .. b + 2`.
pub const PROFILE_SPAN: i64 = 4;

/// What a [`DenoiseCache`] computes. Equal specs produce equal audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenoiseSpec {
    /// Voice or general; never off.
    pub mode: DenoiseMode,
    /// Above 0, up to 1.
    pub strength: f32,
}

impl DenoiseSpec {
    /// The spec of a clip, or `None` when its noise reduction does nothing.
    pub fn of(denoise: &ClipDenoise) -> Option<Self> {
        let strength = if denoise.strength.is_finite() { denoise.strength.clamp(0.0, 1.0) as f32 } else { 0.0 };
        (denoise.mode != DenoiseMode::Off && strength > 0.0).then_some(Self { mode: denoise.mode, strength })
    }

    /// Samples past the end of a run that the method reads to finish it.
    fn lookahead(&self) -> usize {
        match self.mode {
            DenoiseMode::Voice => VOICE_DELAY,
            _ => GATE_SIZE,
        }
    }
}

/// One processed run: the output of chunk `chunk` and the [`CROSSFADE`] samples after it.
struct Run {
    chunk: i64,
    samples: Vec<f32>,
    /// The source frames of the chunk itself, unprocessed.
    dry: Vec<f32>,
}

/// A finished chunk: the processed frames, crossfade applied, and the dry frames beside them.
struct Chunk {
    chunk: i64,
    wet: Vec<f32>,
    dry: Vec<f32>,
}

/// How much of the processed audio a clip at `strength` takes, the rest being the dry audio.
/// RNNoise mixes in proportion. For the gate, steady noise left at `1 - w + w g` of its level,
/// `g` being the gain of the gate at full depth, matches a gate of depth `GATE_DEPTH_DB * strength`
/// for this `w`.
pub fn mix_weight(mode: DenoiseMode, strength: f32) -> f32 {
    match mode {
        DenoiseMode::Voice => strength,
        _ => {
            let deepest = 10f32.powf(-GATE_DEPTH_DB / 20.0);
            let target = 10f32.powf(-GATE_DEPTH_DB * strength / 20.0);
            ((1.0 - target) / (1.0 - deepest)).clamp(0.0, 1.0)
        }
    }
}

/// The processed audio of one clip.
pub struct DenoiseCache {
    spec: DenoiseSpec,
    source: String,
    runs: VecDeque<Run>,
    /// Finished chunks of [`CHUNK`] stereo frames.
    chunks: VecDeque<Chunk>,
    /// The first frame past the end of the source, once a read has reached it.
    source_end: Option<i64>,
    /// Source frames from `raw_start`, kept so that consecutive runs read the source forward.
    raw: Vec<f32>,
    raw_start: i64,
    input: Vec<f32>,
    /// The spectral gate and its noise statistics, for the general mode.
    gate: Option<GateState>,
}

impl DenoiseCache {
    /// An empty cache for `spec` over the file `source`.
    pub fn new(spec: DenoiseSpec, source: &str) -> Self {
        Self { spec, source: source.to_owned(), runs: VecDeque::new(), chunks: VecDeque::new(), source_end: None, raw: Vec::new(), raw_start: 0, input: Vec::new(), gate: None }
    }

    /// Whether this cache can serve `spec` over `source`: the same mode and file. The strength
    /// may differ; [`Self::set_strength`] takes it over and keeps every run.
    pub fn matches(&self, spec: DenoiseSpec, source: &str) -> bool {
        self.spec.mode == spec.mode && self.source == source
    }

    /// The strength the next reads mix at.
    pub fn set_strength(&mut self, strength: f32) {
        self.spec.strength = strength;
    }

    /// Reads the denoised audio of `key` from `inner` like [`AudioSource::read_counted`].
    pub fn read<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, position: i64, out: &mut [f32]) -> Result<usize, RenderError> {
        let frames = (out.len() / MIX_CHANNELS) as i64;
        let end = position.saturating_add(frames);
        let mut at = position.max(0);
        let weight = mix_weight(self.spec.mode, self.spec.strength);
        while at < end {
            if self.source_end.is_some_and(|source_end| at >= source_end) {
                break;
            }
            let chunk = at.div_euclid(CHUNK);
            let take = ((chunk + 1) * CHUNK).min(end) - at;
            let done = self.chunk(inner, key, chunk)?;
            let from = ((at - chunk * CHUNK) as usize) * MIX_CHANNELS;
            let to = ((at - position) as usize) * MIX_CHANNELS;
            let len = take as usize * MIX_CHANNELS;
            for ((sample, &wet), &dry) in out[to..to + len].iter_mut().zip(&done.wet[from..from + len]).zip(&done.dry[from..from + len]) {
                *sample = dry + (wet - dry) * weight;
            }
            at += take;
        }
        let covered = match self.source_end {
            Some(source_end) => (source_end - position).clamp(0, frames),
            None => frames,
        };
        // Nothing past the end of the file sounds, whatever the processing left there.
        out[covered as usize * MIX_CHANNELS..].fill(0.0);
        Ok(covered as usize)
    }

    /// The finished samples of `chunk`.
    fn chunk<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, chunk: i64) -> Result<&Chunk, RenderError> {
        if let Some(index) = self.chunks.iter().position(|known| known.chunk == chunk) {
            return Ok(&self.chunks[index]);
        }
        if self.spec.mode != DenoiseMode::Voice {
            let done = self.gate_chunk(inner, key, chunk)?;
            return Ok(self.keep_chunk(done));
        }
        // The run before first, so that the source is read forward only.
        if chunk > 0 {
            self.run(inner, key, chunk - 1)?;
        }
        self.run(inner, key, chunk)?;
        let run = self.runs.iter().find(|run| run.chunk == chunk).expect("run computed");
        let mut samples = run.samples[..CHUNK as usize * MIX_CHANNELS].to_vec();
        let dry = run.dry.clone();
        // The first chunk of the file has nothing before it to blend with: its run starts in the
        // silence before the file like every other run.
        if chunk > 0 {
            let tail = &self.run_samples(chunk - 1)[CHUNK as usize * MIX_CHANNELS..];
            for i in 0..CROSSFADE as usize {
                // Raised cosine: the two gains sum to one.
                let rise = 0.5 - 0.5 * ((i as f32 + 0.5) / CROSSFADE as f32 * PI).cos();
                let fall = 1.0 - rise;
                for c in 0..MIX_CHANNELS {
                    let at = i * MIX_CHANNELS + c;
                    samples[at] = samples[at] * rise + tail[at] * fall;
                }
            }
        }
        Ok(self.keep_chunk(Chunk { chunk, wet: samples, dry }))
    }

    fn keep_chunk(&mut self, done: Chunk) -> &Chunk {
        self.chunks.push_back(done);
        while self.chunks.len() > KEPT_CHUNKS {
            self.chunks.pop_front();
        }
        self.chunks.back().expect("chunk just pushed")
    }

    fn run_samples(&self, chunk: i64) -> &[f32] {
        &self.runs.iter().find(|run| run.chunk == chunk).expect("run computed").samples
    }

    /// Makes source frames `[from, to)` available in `raw`, reading forward only: a decoder seeks
    /// on every read that goes back.
    fn ensure<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, from: i64, to: i64) -> Result<(), RenderError> {
        let (from, to) = (from.max(0), to.max(0));
        let raw_end = self.raw_start + (self.raw.len() / MIX_CHANNELS) as i64;
        let read_from = if self.raw.is_empty() || from < self.raw_start || from > raw_end {
            self.raw.clear();
            self.raw_start = from;
            from
        } else {
            raw_end
        };
        if to <= read_from {
            return Ok(());
        }
        let at = self.raw.len();
        self.raw.resize(at + (to - read_from) as usize * MIX_CHANNELS, 0.0);
        let written = inner.read_counted(key, &self.source, read_from, &mut self.raw[at..])? as i64;
        if written < to - read_from {
            let end = read_from + written;
            self.source_end = Some(self.source_end.map_or(end, |known| known.min(end)));
        }
        Ok(())
    }

    /// Gates `chunk` with the noise profile around it. Every value depends only on the source.
    fn gate_chunk<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, chunk: i64) -> Result<Chunk, RenderError> {
        let hop = GATE_HOP as i64;
        let radius = (GATE_SMOOTHING.len() / 2) as i64;
        // The windows over the samples of the chunk: window `w` covers `[w * hop, w * hop + 2 * hop)`.
        let first = (chunk * CHUNK).div_euclid(hop) - 1;
        let last = ((chunk + 1) * CHUNK - 1).div_euclid(hop);
        let profile_of = |w: i64| (w * hop + GATE_SIZE as i64 / 2).div_euclid(CHUNK);
        let (low, high) = (profile_of(first), profile_of(last) + 1);
        // The statistics first, in source order, so that the source is read forward only.
        let half = PROFILE_SPAN / 2;
        for c in low - half..high + half {
            self.chunk_stats(inner, key, c)?;
        }
        for b in low..=high {
            self.profile(b);
        }
        self.ensure(inner, key, (first - radius) * hop, (last + radius) * hop + GATE_SIZE as i64)?;

        let state = self.gate.as_mut().expect("statistics computed");
        let bins = GATE_SIZE / 2 + 1;
        let outputs = (last - first + 1) as usize;
        let mut powers = vec![0.0f32; (outputs + 2 * radius as usize) * bins];
        let mut spectra = vec![Complex32::default(); outputs * GATE_SIZE];
        for (n, w) in (first - radius..=last + radius).enumerate() {
            state.gate.analyse(&self.raw, self.raw_start, w, &mut powers[n * bins..(n + 1) * bins]);
            if let Some(output) = n.checked_sub(radius as usize).filter(|output| *output < outputs) {
                spectra[output * GATE_SIZE..(output + 1) * GATE_SIZE].copy_from_slice(&state.gate.spectrum);
            }
        }
        let floor = 10f32.powf(-GATE_DEPTH_DB / 20.0);
        let mut wet = vec![0.0f32; CHUNK as usize * MIX_CHANNELS];
        let mut raw_gains = vec![1.0f32; bins];
        let mut gains = vec![1.0f32; bins];
        for (n, w) in (first..=last).enumerate() {
            let centre = w * hop + GATE_SIZE as i64 / 2;
            let b = centre.div_euclid(CHUNK);
            let along = (centre - b * CHUNK) as f32 / CHUNK as f32;
            let (before, after) = (&state.profiles[&b], &state.profiles[&(b + 1)]);
            for k in 0..bins {
                let noise = before[k] + (after[k] - before[k]) * along;
                let level: f32 = GATE_SMOOTHING.iter().enumerate().map(|(d, weight)| weight * powers[(n + d) * bins + k]).sum();
                raw_gains[k] = (1.0 - GATE_OVER * noise / level.max(1e-20)).max(floor);
            }
            // A little smoothing across frequency, so that lone bins do not twinkle.
            for k in 0..bins {
                gains[k] = 0.25 * raw_gains[k.saturating_sub(1)] + 0.5 * raw_gains[k] + 0.25 * raw_gains[(k + 1).min(bins - 1)];
            }
            let spectrum = &mut state.gate.spectrum;
            spectrum.copy_from_slice(&spectra[n * GATE_SIZE..(n + 1) * GATE_SIZE]);
            for (k, &gain) in gains.iter().enumerate() {
                spectrum[k] *= gain;
                let mirror = (GATE_SIZE - k) % GATE_SIZE;
                if mirror != k {
                    spectrum[mirror] *= gain;
                }
            }
            state.gate.inverse.process_with_scratch(&mut state.gate.spectrum, &mut state.gate.scratch);
            let scale = 1.0 / GATE_SIZE as f32;
            for (i, value) in state.gate.spectrum.iter().enumerate() {
                let at = w * hop + i as i64 - chunk * CHUNK;
                if !(0..CHUNK).contains(&at) {
                    continue;
                }
                let base = at as usize * MIX_CHANNELS;
                let weight = state.gate.window[i] * scale;
                wet[base] += value.re * weight;
                wet[base + 1] += value.im * weight;
            }
        }
        let mut dry = vec![0.0f32; CHUNK as usize * MIX_CHANNELS];
        let skip = (-chunk * CHUNK).clamp(0, CHUNK) as usize;
        if skip < CHUNK as usize {
            let from = ((chunk * CHUNK + skip as i64 - self.raw_start) as usize) * MIX_CHANNELS;
            dry[skip * MIX_CHANNELS..].copy_from_slice(&self.raw[from..from + (CHUNK as usize - skip) * MIX_CHANNELS]);
        }
        // The next chunk needs nothing before its first windows, nor statistics further back.
        let keep_from = ((chunk + 1) * CHUNK).div_euclid(hop) * hop - (radius + 2) * hop;
        let keep_from = keep_from.max(self.raw_start);
        let drop = (((keep_from - self.raw_start) as usize) * MIX_CHANNELS).min(self.raw.len());
        self.raw.drain(..drop);
        self.raw_start += (drop / MIX_CHANNELS) as i64;
        state.stats.retain(|c, _| (chunk - half - 1..=chunk + half + 2).contains(c));
        state.profiles.retain(|b, _| (chunk - 1..=chunk + 3).contains(b));
        Ok(Chunk { chunk, wet, dry })
    }

    /// Computes the noise statistic of source chunk `chunk` unless known: per bin, the
    /// [`GATE_PERCENTILE`] of the power over the windows starting in it. `None` outside the file.
    fn chunk_stats<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, chunk: i64) -> Result<(), RenderError> {
        let state = self.gate.get_or_insert_with(GateState::new);
        if state.stats.contains_key(&chunk) {
            return Ok(());
        }
        let hop = GATE_HOP as i64;
        let first = (chunk * CHUNK + hop - 1).div_euclid(hop);
        let past = ((chunk + 1) * CHUNK + hop - 1).div_euclid(hop);
        let outside = |end: Option<i64>| chunk < 0 || end.is_some_and(|end| chunk * CHUNK >= end);
        if !outside(self.source_end) {
            // Reading the chunk tells whether the file ends in it.
            self.ensure(inner, key, first * hop, (past - 1) * hop + GATE_SIZE as i64)?;
        }
        let state = self.gate.as_mut().expect("created above");
        if outside(self.source_end) {
            state.stats.insert(chunk, None);
            return Ok(());
        }
        // Windows that start past the end of the file hold only its silence.
        let past = self.source_end.map_or(past, |end| past.min((end + hop - 1).div_euclid(hop)));
        // CHUNK is not a multiple of the hop, so a file may end after the chunk starts but before
        // its first window does: no window starts in it, and it counts as outside the file.
        if past <= first {
            state.stats.insert(chunk, None);
            return Ok(());
        }
        let bins = GATE_SIZE / 2 + 1;
        let count = (past - first) as usize;
        let mut powers = vec![0.0f32; count * bins];
        for (n, w) in (first..past).enumerate() {
            state.gate.analyse(&self.raw, self.raw_start, w, &mut powers[n * bins..(n + 1) * bins]);
        }
        let rank = ((count - 1) as f32 * GATE_PERCENTILE).round() as usize;
        let mut column = vec![0.0f32; count];
        let stat = (0..bins)
            .map(|k| {
                for (n, value) in column.iter_mut().enumerate() {
                    *value = powers[n * bins + k];
                }
                *column.select_nth_unstable_by(rank, f32::total_cmp).1
            })
            .collect();
        state.stats.insert(chunk, Some(stat));
        Ok(())
    }

    /// Computes the noise profile at the start of source chunk `b` unless known, from the
    /// statistics of the chunks around it, which must be known.
    fn profile(&mut self, b: i64) {
        let state = self.gate.as_mut().expect("statistics computed");
        if state.profiles.contains_key(&b) {
            return;
        }
        let half = PROFILE_SPAN / 2;
        let known: Vec<&Vec<f32>> = (b - half..b + half).filter_map(|c| state.stats[&c].as_ref()).collect();
        let bins = GATE_SIZE / 2 + 1;
        let mut values = Vec::with_capacity(known.len());
        // The median over time of each bin; the middle two averaged for an even count.
        let over_time: Vec<f32> = (0..bins)
            .map(|k| {
                if known.is_empty() {
                    return 0.0;
                }
                values.clear();
                values.extend(known.iter().map(|stat| stat[k]));
                values.sort_by(f32::total_cmp);
                let middle = values.len() / 2;
                if values.len() % 2 == 0 { 0.5 * (values[middle - 1] + values[middle]) } else { values[middle] }
            })
            .collect();
        // The median over frequency, so that a steady tone does not count as noise.
        let mut neighbours = Vec::with_capacity(2 * GATE_MEDIAN_RADIUS + 1);
        let profile = (0..bins)
            .map(|k| {
                neighbours.clear();
                neighbours.extend_from_slice(&over_time[k.saturating_sub(GATE_MEDIAN_RADIUS)..(k + GATE_MEDIAN_RADIUS + 1).min(bins)]);
                let middle = neighbours.len() / 2;
                *neighbours.select_nth_unstable_by(middle, f32::total_cmp).1 * GATE_BIAS
            })
            .collect();
        state.profiles.insert(b, profile);
    }

    /// Makes sure the run of `chunk` is computed.
    fn run<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, chunk: i64) -> Result<(), RenderError> {
        if self.runs.iter().any(|run| run.chunk == chunk) {
            return Ok(());
        }
        let start = chunk * CHUNK - PREROLL;
        let len = (PREROLL + CHUNK + CROSSFADE) as usize;
        let total = len + self.spec.lookahead();
        self.ensure(inner, key, start, start + total as i64)?;
        self.input.clear();
        self.input.resize(total * MIX_CHANNELS, 0.0);
        // Before the file is silence; the run still starts at the same grid position.
        let skip = (-start).clamp(0, total as i64) as usize;
        if skip < total {
            let from = ((start + skip as i64 - self.raw_start) as usize) * MIX_CHANNELS;
            self.input[skip * MIX_CHANNELS..].copy_from_slice(&self.raw[from..from + (total - skip) * MIX_CHANNELS]);
        }
        // The next run starts one chunk later; nothing before that is needed again.
        let keep_from = (start + CHUNK).max(self.raw_start);
        let drop = (((keep_from - self.raw_start) as usize) * MIX_CHANNELS).min(self.raw.len());
        self.raw.drain(..drop);
        self.raw_start += (drop / MIX_CHANNELS) as i64;

        let mut samples = vec![0.0; len * MIX_CHANNELS];
        // At full strength: the strength of the clip mixes when a block is read.
        voice(&self.input, 1.0, &mut samples);
        // Keep only what follows the preroll, which was only there to settle the state.
        let kept = samples.split_off(PREROLL as usize * MIX_CHANNELS);
        let dry_from = PREROLL as usize * MIX_CHANNELS;
        let dry = self.input[dry_from..dry_from + CHUNK as usize * MIX_CHANNELS].to_vec();
        self.runs.push_back(Run { chunk, samples: kept, dry });
        while self.runs.len() > KEPT_CHUNKS {
            self.runs.pop_front();
        }
        Ok(())
    }
}

/// The shared RNNoise weights.
fn model() -> &'static RnnModel {
    static MODEL: OnceLock<RnnModel> = OnceLock::new();
    MODEL.get_or_init(RnnModel::default)
}

/// RNNoise over interleaved stereo `input`, each channel on its own network starting fresh.
/// `input` has [`VOICE_DELAY`] frames more than `out`, which receives the output aligned with the
/// input, blended with the input by `strength`.
pub fn voice(input: &[f32], strength: f32, out: &mut [f32]) {
    let frames = out.len() / MIX_CHANNELS;
    let total = input.len() / MIX_CHANNELS;
    let mut mono_in = vec![0.0f32; VOICE_FRAME];
    let mut mono_out = vec![0.0f32; VOICE_FRAME];
    for channel in 0..MIX_CHANNELS {
        let mut state = DenoiseState::with_model(model());
        let mut at = 0;
        while at + VOICE_FRAME <= total {
            for (i, sample) in mono_in.iter_mut().enumerate() {
                *sample = input[(at + i) * MIX_CHANNELS + channel] * PCM_SCALE;
            }
            state.process_frame(&mut mono_out, &mono_in);
            // Output sample `at + i` belongs to input sample `at + i - VOICE_DELAY`.
            for (i, &sample) in mono_out.iter().enumerate() {
                let Some(index) = (at + i).checked_sub(VOICE_DELAY).filter(|index| *index < frames) else { continue };
                let dry = input[index * MIX_CHANNELS + channel];
                let wet = sample / PCM_SCALE;
                out[index * MIX_CHANNELS + channel] = dry + (wet - dry) * strength;
            }
            at += VOICE_FRAME;
        }
    }
}

/// The spectral gate: FFT plans and buffers, reused between chunks. It holds no audio state.
pub struct Gate {
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    spectrum: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    /// Plans the transforms.
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(GATE_SIZE);
        let inverse = planner.plan_fft_inverse(GATE_SIZE);
        let scratch_len = forward.get_inplace_scratch_len().max(inverse.get_inplace_scratch_len());
        // Square root of a periodic Hann window, at analysis and at synthesis.
        let window = (0..GATE_SIZE).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f32 / GATE_SIZE as f32).cos()).sqrt()).collect();
        Self { forward, inverse, window, spectrum: vec![Complex32::default(); GATE_SIZE], scratch: vec![Complex32::default(); scratch_len] }
    }

    /// Transforms window `w` of `raw`, which holds source frames from `raw_start`, into
    /// `self.spectrum`, both channels in one transform (left real, right imaginary), and writes
    /// the power of each bin, `(|L|² + |R|²) / 2`, to `power`. Before the file is silence.
    fn analyse(&mut self, raw: &[f32], raw_start: i64, w: i64, power: &mut [f32]) {
        let start = w * GATE_HOP as i64;
        for (i, value) in self.spectrum.iter_mut().enumerate() {
            let at = start + i as i64;
            *value = if at < 0 {
                Complex32::default()
            } else {
                let base = (at - raw_start) as usize * MIX_CHANNELS;
                Complex32::new(raw[base], raw[base + 1]) * self.window[i]
            };
        }
        self.forward.process_with_scratch(&mut self.spectrum, &mut self.scratch);
        for (k, level) in power.iter_mut().enumerate() {
            let mirror = (GATE_SIZE - k) % GATE_SIZE;
            *level = (self.spectrum[k].norm_sqr() + self.spectrum[mirror].norm_sqr()) / 4.0;
        }
    }
}

/// The gate of one clip with the noise statistics and profiles it has computed, by source chunk.
struct GateState {
    gate: Gate,
    /// Per chunk, its noise statistic; `None` for a chunk outside the file.
    stats: BTreeMap<i64, Option<Vec<f32>>>,
    /// Per chunk, the noise profile at its start.
    profiles: BTreeMap<i64, Vec<f32>>,
}

impl GateState {
    fn new() -> Self {
        Self { gate: Gate::new(), stats: BTreeMap::new(), profiles: BTreeMap::new() }
    }
}
