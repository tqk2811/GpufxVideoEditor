//! Noise reduction of a clip's source audio, as a layer over an [`AudioSource`].
//!
//! # Determinism
//!
//! Both methods carry state from one sample to the next, so the output at a source position would
//! depend on where processing began. To keep preview and export identical, processing is cut into
//! fixed chunks of [`CHUNK`] samples on a grid of absolute source positions. Chunk `j` is produced
//! by a fresh processor that starts [`PREROLL`] samples before it (silence before the file) and
//! runs [`CROSSFADE`] samples past it. The first [`CROSSFADE`] samples of a chunk blend the tail
//! of the run of chunk `j - 1` into the run of chunk `j`, so the restart is inaudible. A sample
//! therefore depends only on the source audio, the method and the strength: a block read after a
//! seek is bit for bit what a sequential export reads, as long as the source gives the same
//! samples for a position however it is reached (see [`crate::stretch`]). Runs are kept between
//! reads, which is only memoisation, and the source is read forward only.
//!
//! The price is work: a chunk costs `(PREROLL + CHUNK + CROSSFADE) / CHUNK`, about 1.5 times the
//! audio, and a seek computes two runs before the first sample. Half a second of preroll lets the
//! recurrent state of RNNoise and the noise floor of the spectral gate settle; a longer one only
//! costs more after each seek.
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
//! Short-time Fourier transform with 1024-sample square-root Hann windows at half overlap. Per
//! bin, the local level is the median of the smoothed power over the 17 bins around it, so a
//! steady tone, which fills only a few bins, does not raise it; the noise floor follows the
//! minimum of that level over time, falling at once and rising 3 dB per second. A bin is turned
//! down by how far its power stands above the floor, to at most 30 dB at strength 1. Steady
//! broadband noise (hiss, fans, rain) goes; narrow tones such as mains hum are kept.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::{Arc, OnceLock};

use nnnoiseless::{DenoiseState, RnnModel};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::audio_mix::AudioSource;
use crate::audio_schedule::MIX_CHANNELS;
use crate::project::{ClipDenoise, DenoiseMode};
use crate::render::{ClipKey, RenderError};

/// Samples of one chunk: one second. A multiple of the RNNoise frame and of the gate hop.
pub const CHUNK: i64 = 48_000;
/// Samples each chunk is processed ahead of its start: half a second.
pub const PREROLL: i64 = 24_000;
/// Samples over which consecutive chunks are blended: 10 ms.
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
/// Bins either side whose median makes the local level.
const GATE_MEDIAN_RADIUS: usize = 8;
/// How fast the tracked noise floor may rise per hop: 3 dB per second. It falls at once.
const GATE_RISE: f32 = 1.007_4;
/// The median of the smoothed power lies below its mean; this brings the floor back up to it.
const GATE_BIAS: f32 = 2.0;
/// The deepest cut at strength 1, in decibels.
const GATE_DEPTH_DB: f32 = 30.0;
/// How much the gain of a bin keeps of its previous hop when it falls, against musical noise.
const GATE_RELEASE: f32 = 0.6;

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
}

/// The processed audio of one clip.
pub struct DenoiseCache {
    spec: DenoiseSpec,
    source: String,
    runs: VecDeque<Run>,
    /// Finished chunks, crossfade applied: `(chunk, CHUNK stereo frames)`.
    chunks: VecDeque<(i64, Vec<f32>)>,
    /// The first frame past the end of the source, once a read has reached it.
    source_end: Option<i64>,
    /// Source frames from `raw_start`, kept so that consecutive runs read the source forward.
    raw: Vec<f32>,
    raw_start: i64,
    input: Vec<f32>,
    gate: Option<Gate>,
}

impl DenoiseCache {
    /// An empty cache for `spec` over the file `source`.
    pub fn new(spec: DenoiseSpec, source: &str) -> Self {
        Self { spec, source: source.to_owned(), runs: VecDeque::new(), chunks: VecDeque::new(), source_end: None, raw: Vec::new(), raw_start: 0, input: Vec::new(), gate: None }
    }

    /// Whether this cache computes `spec` over `source`.
    pub fn matches(&self, spec: DenoiseSpec, source: &str) -> bool {
        self.spec == spec && self.source == source
    }

    /// Reads the denoised audio of `key` from `inner` like [`AudioSource::read_counted`].
    pub fn read<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, position: i64, out: &mut [f32]) -> Result<usize, RenderError> {
        let frames = (out.len() / MIX_CHANNELS) as i64;
        let end = position.saturating_add(frames);
        let mut at = position.max(0);
        while at < end {
            if self.source_end.is_some_and(|source_end| at >= source_end) {
                break;
            }
            let chunk = at.div_euclid(CHUNK);
            let take = ((chunk + 1) * CHUNK).min(end) - at;
            let samples = self.chunk(inner, key, chunk)?;
            let from = ((at - chunk * CHUNK) as usize) * MIX_CHANNELS;
            let to = ((at - position) as usize) * MIX_CHANNELS;
            let len = take as usize * MIX_CHANNELS;
            out[to..to + len].copy_from_slice(&samples[from..from + len]);
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
    fn chunk<S: AudioSource + ?Sized>(&mut self, inner: &mut S, key: ClipKey, chunk: i64) -> Result<&[f32], RenderError> {
        if let Some(index) = self.chunks.iter().position(|(known, _)| *known == chunk) {
            return Ok(&self.chunks[index].1);
        }
        // The run before first, so that the source is read forward only.
        if chunk > 0 {
            self.run(inner, key, chunk - 1)?;
        }
        self.run(inner, key, chunk)?;
        let mut samples = self.run_samples(chunk)[..CHUNK as usize * MIX_CHANNELS].to_vec();
        // The first chunk of the file has nothing before it to blend with: its run starts in the
        // silence before the file like every other run.
        if chunk > 0 {
            let tail = &self.run_samples(chunk - 1)[CHUNK as usize * MIX_CHANNELS..];
            for i in 0..CROSSFADE as usize {
                let fade = (i as f32 + 0.5) / CROSSFADE as f32 * PI / 2.0;
                let (rise, fall) = (fade.sin(), fade.cos());
                for c in 0..MIX_CHANNELS {
                    let at = i * MIX_CHANNELS + c;
                    samples[at] = samples[at] * rise + tail[at] * fall;
                }
            }
        }
        self.chunks.push_back((chunk, samples));
        while self.chunks.len() > KEPT_CHUNKS {
            self.chunks.pop_front();
        }
        Ok(&self.chunks.back().expect("chunk just pushed").1)
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
        match self.spec.mode {
            DenoiseMode::Voice => voice(&self.input, self.spec.strength, &mut samples),
            _ => self.gate.get_or_insert_with(Gate::new).process(&self.input, self.spec.strength, &mut samples),
        }
        // Keep only what follows the preroll, which was only there to settle the state.
        let kept = samples.split_off(PREROLL as usize * MIX_CHANNELS);
        self.runs.push_back(Run { chunk, samples: kept });
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

/// The spectral gate: FFT plans and buffers, reused between runs. It holds no audio state.
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

    /// Gates interleaved stereo `input` into `out`, from a fresh noise floor. `input` may be longer
    /// than `out`; what lies past it only finishes the last windows.
    pub fn process(&mut self, input: &[f32], strength: f32, out: &mut [f32]) {
        let frames = out.len() / MIX_CHANNELS;
        let total = input.len() / MIX_CHANNELS;
        let bins = GATE_SIZE / 2 + 1;
        let floor = 10f32.powf(-GATE_DEPTH_DB * strength / 20.0);
        let over = 1.0 + strength;
        let mut smoothed = vec![0.0f32; bins];
        let mut noise = vec![0.0f32; bins];
        let mut held = vec![1.0f32; bins];
        let mut gains = vec![1.0f32; bins];
        let mut neighbours = Vec::with_capacity(2 * GATE_MEDIAN_RADIUS + 1);
        out.fill(0.0);
        // Window `w` starts at sample `w * GATE_HOP - GATE_HOP`, so every output sample lies under
        // two windows.
        let mut first = true;
        let mut window_start = -(GATE_HOP as i64);
        while window_start < frames as i64 {
            // Both channels in one transform: left real, right imaginary.
            for (i, value) in self.spectrum.iter_mut().enumerate() {
                let at = window_start + i as i64;
                *value = if at >= 0 && (at as usize) < total {
                    let base = at as usize * MIX_CHANNELS;
                    Complex32::new(input[base], input[base + 1]) * self.window[i]
                } else {
                    Complex32::default()
                };
            }
            self.forward.process_with_scratch(&mut self.spectrum, &mut self.scratch);
            for (k, level) in smoothed.iter_mut().enumerate() {
                let mirror = (GATE_SIZE - k) % GATE_SIZE;
                // (|L|² + |R|²) / 2, from the transform of `l + i r`.
                let power = (self.spectrum[k].norm_sqr() + self.spectrum[mirror].norm_sqr()) / 4.0;
                *level = if first { power } else { 0.5 * *level + 0.5 * power };
            }
            for k in 0..bins {
                neighbours.clear();
                neighbours.extend_from_slice(&smoothed[k.saturating_sub(GATE_MEDIAN_RADIUS)..(k + GATE_MEDIAN_RADIUS + 1).min(bins)]);
                let middle = neighbours.len() / 2;
                let local = *neighbours.select_nth_unstable_by(middle, f32::total_cmp).1;
                noise[k] = if first || local < noise[k] { local } else { noise[k] * GATE_RISE };
            }
            for k in 0..bins {
                let gain = (1.0 - over * GATE_BIAS * noise[k] / smoothed[k].max(1e-20)).max(floor);
                // Falls gently, rises at once.
                held[k] = if gain >= held[k] { gain } else { GATE_RELEASE * held[k] + (1.0 - GATE_RELEASE) * gain };
            }
            // A little smoothing across frequency, so that lone bins do not twinkle.
            for k in 0..bins {
                gains[k] = 0.25 * held[k.saturating_sub(1)] + 0.5 * held[k] + 0.25 * held[(k + 1).min(bins - 1)];
            }
            for (k, &gain) in gains.iter().enumerate() {
                self.spectrum[k] *= gain;
                let mirror = (GATE_SIZE - k) % GATE_SIZE;
                if mirror != k {
                    self.spectrum[mirror] *= gain;
                }
            }
            self.inverse.process_with_scratch(&mut self.spectrum, &mut self.scratch);
            let scale = 1.0 / GATE_SIZE as f32;
            for (i, value) in self.spectrum.iter().enumerate() {
                let at = window_start + i as i64;
                if at < 0 || at as usize >= frames {
                    continue;
                }
                let base = at as usize * MIX_CHANNELS;
                let weight = self.window[i] * scale;
                out[base] += value.re * weight;
                out[base + 1] += value.im * weight;
            }
            first = false;
            window_start += GATE_HOP as i64;
        }
    }
}
