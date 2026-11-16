//! Beat detection on decoded sound, for the beats of a clip (see [`crate::ClipBeats`]).
//!
//! The steps are the classic ones of D. P. W. Ellis, "Beat Tracking by Dynamic Programming"
//! (2007):
//!
//! 1. A short-time Fourier transform (Hann window of about 43 ms, hop of an eighth of it) gives a
//!    log-compressed magnitude spectrum per frame.
//! 2. The spectral flux, the summed rise of every bin from one frame to the next, is the onset
//!    envelope. Its local mean (over about a second) is taken away and it is scaled to unit
//!    deviation, so loud and quiet passages weigh alike.
//! 3. The tempo is the period whose autocorrelation of the envelope, summed over its first four
//!    multiples and weighted towards 120 BPM on a log scale, is highest; it is searched in hundredths
//!    of a frame, so a steady tempo comes out well within 1 BPM.
//! 4. Beats come from dynamic programming: each frame's best score is its onset strength plus the
//!    best score of an earlier frame, less a penalty that grows with the log of how far the gap is
//!    from the period. The best frame of the last period is traced back. Each beat then moves to the
//!    strongest onset within a frame of it, and quiet beats at either end (silence before or after
//!    the music) are dropped.
//! 5. Bars of [`BEATS_PER_BAR`] beats start at the phase whose beats are strongest: those beats are
//!    the downbeats.
//!
//! The function takes mono samples at any rate; the editor gives it [`ANALYSIS_RATE`]. It is pure:
//! decoding and the choice of thread are the caller's.

use crate::project::{Beat, ClipBeats};
use crate::time::{Time, TICKS_PER_SECOND};
use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;

/// The sample rate the editor decodes to for detection: plenty for onsets, and a quarter of the
/// mixing rate, so decimation is a plain average of four.
pub const ANALYSIS_RATE: u32 = 12_000;
/// Beats in a bar, for the downbeats.
pub const BEATS_PER_BAR: usize = 4;
/// The slowest and fastest tempo looked for, in beats per minute.
pub const MIN_TEMPO: f64 = 40.0;
pub const MAX_TEMPO_SEARCHED: f64 = 240.0;
/// The shortest sound that gets a tempo, in seconds. A tempo needs two of its periods in the
/// sound, so below `2 * 60 / MIN_TEMPO` seconds (3 s) the slowest tempo looked for rises to two
/// periods in the length: 60 BPM for 2 s.
pub const MIN_TEMPO_SECONDS: f64 = 2.0;
/// The tempo the search leans to, and how far it leans: the spread of the log-normal weight, in
/// octaves. Between a tempo and its double the one nearer this wins when both fit equally.
const TEMPO_CENTER: f64 = 120.0;
const TEMPO_SPREAD_OCTAVES: f64 = 1.0;
/// How strongly a gap between beats is held to the period (Ellis's alpha).
const TIGHTNESS: f64 = 400.0;
/// Length of the window, in seconds; rounded to a power of two of samples.
const WINDOW_SECONDS: f64 = 0.043;
/// Window over which the local mean of the envelope is taken away, in seconds.
const MEAN_SECONDS: f64 = 1.0;
/// A beat at either end weaker than this part of the median beat is silence, not music.
const EDGE_STRENGTH: f64 = 0.2;
/// Where in its window, as a part of the window, a frame's onset is when its flux peaks.
const ONSET_LAG: f64 = 0.8;
/// Multiples of the period that the tempo score sums.
const TEMPO_MULTIPLES: usize = 4;

/// What [`detect_beats`] found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BeatAnalysis {
    /// Beats per minute; 0 when no tempo was found (silence, or less than [`MIN_TEMPO_SECONDS`]).
    pub tempo: f64,
    /// Beat times in seconds from the first sample, increasing.
    pub beats: Vec<f64>,
    /// For each beat, whether it starts a bar.
    pub downbeats: Vec<bool>,
}

impl BeatAnalysis {
    /// The beats as a clip keeps them: in source time, which starts at the first sample decoded.
    pub fn clip_beats(&self) -> ClipBeats {
        let tick = |seconds: f64| Time::from_ticks((seconds * TICKS_PER_SECOND as f64).round() as i64);
        let mut beats: Vec<Beat> = Vec::with_capacity(self.beats.len());
        for (index, &seconds) in self.beats.iter().enumerate() {
            let at = tick(seconds);
            // Rounding to ticks cannot make two beats meet at any real tempo, but the list must stay strict.
            if beats.last().is_some_and(|last| last.at >= at) || at < Time::ZERO {
                continue;
            }
            beats.push(Beat { at, downbeat: self.downbeats.get(index).copied().unwrap_or(false) });
        }
        let tempo = if self.tempo.is_finite() { (self.tempo * 100.0).round() / 100.0 } else { 0.0 };
        ClipBeats { tempo: if beats.is_empty() { 0.0 } else { tempo }, beats }
    }
}

/// Finds the tempo and beats of `samples`, mono at `sample_rate`. `progress` is told how far the
/// work is, from 0 to 1, now and then; it returns false to stop, and then the result is `None`.
/// Silence or a sound too short for a tempo gives an analysis without beats.
pub fn detect_beats(samples: &[f32], sample_rate: u32, progress: &mut dyn FnMut(f32) -> bool) -> Option<BeatAnalysis> {
    let rate = f64::from(sample_rate.max(1));
    // The power of two at or below the length asked for: 512 samples at the analysis rate.
    let wanted = ((rate * WINDOW_SECONDS) as usize).max(16);
    let window = if wanted.is_power_of_two() { wanted } else { wanted.next_power_of_two() / 2 };
    let hop = window / 8;
    let frames_per_second = rate / hop as f64;
    let flux = spectral_flux(samples, window, hop, progress)?;
    // The log magnitude jumps as soon as an onset enters the window, while the Hann weight there is
    // still small: the flux of a sharp onset peaks in the frame that has it about four fifths of
    // the window in (measured on clicks). This puts the frame time on the onset itself.
    let lag = window as f64 * ONSET_LAG;
    let frame_seconds = |frame: f64| (frame * hop as f64 + lag) / rate;
    let envelope = onset_envelope(&flux, frames_per_second);
    if !progress(0.8) {
        return None;
    }
    let Some(period) = tempo_period(&envelope, frames_per_second) else {
        return Some(BeatAnalysis::default());
    };
    if !progress(0.9) {
        return None;
    }
    let mut frames = track_beats(&envelope, period);
    refine_beats(&envelope, &mut frames);
    let strengths: Vec<f64> = frames.iter().map(|&frame| strength_near(&envelope, frame)).collect();
    let (first, last) = trim_quiet_edges(&strengths);
    let frames = &frames[first..last];
    let strengths = &strengths[first..last];
    let downbeats = downbeat_flags(strengths);
    progress(1.0);
    Some(BeatAnalysis {
        tempo: if frames.is_empty() { 0.0 } else { 60.0 * frames_per_second / period },
        beats: frames.iter().map(|&frame| frame_seconds(frame as f64)).collect(),
        downbeats,
    })
}

/// The summed rise of the log magnitude of every bin, one value per frame of `window` samples
/// every `hop`. Empty when the sound is shorter than a window.
fn spectral_flux(samples: &[f32], window: usize, hop: usize, progress: &mut dyn FnMut(f32) -> bool) -> Option<Vec<f32>> {
    if samples.len() < window {
        return Some(Vec::new());
    }
    let count = (samples.len() - window) / hop + 1;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(window);
    let hann: Vec<f32> = (0..window).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / window as f32).cos()).collect();
    let bins = window / 2;
    let mut buffer = vec![Complex32::new(0.0, 0.0); window];
    let mut scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut previous = vec![0.0_f32; bins];
    let mut current = vec![0.0_f32; bins];
    let mut flux = Vec::with_capacity(count);
    for frame in 0..count {
        if frame % 2048 == 0 && !progress(0.8 * frame as f32 / count as f32) {
            return None;
        }
        let start = frame * hop;
        for (slot, (sample, weight)) in buffer.iter_mut().zip(samples[start..start + window].iter().zip(&hann)) {
            // A sample that is not a number is silence, so one bad value cannot spoil the whole sound.
            let sample = if sample.is_finite() { *sample } else { 0.0 };
            *slot = Complex32::new(sample * weight, 0.0);
        }
        fft.process_with_scratch(&mut buffer, &mut scratch);
        for (value, bin) in current.iter_mut().zip(&buffer[1..=bins]) {
            *value = (1.0 + 100.0 * bin.norm()).ln();
        }
        let rise = if frame == 0 { 0.0 } else { current.iter().zip(&previous).map(|(now, before)| (now - before).max(0.0)).sum() };
        flux.push(rise);
        std::mem::swap(&mut previous, &mut current);
    }
    Some(flux)
}

/// The flux without its local mean, half-wave rectified, lightly smoothed and scaled to unit
/// deviation.
fn onset_envelope(flux: &[f32], frames_per_second: f64) -> Vec<f64> {
    if flux.is_empty() {
        return Vec::new();
    }
    let half = ((MEAN_SECONDS * frames_per_second / 2.0) as usize).max(1);
    let mut prefix = Vec::with_capacity(flux.len() + 1);
    prefix.push(0.0_f64);
    for &value in flux {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + f64::from(value));
    }
    let rectified: Vec<f64> = (0..flux.len())
        .map(|i| {
            let (low, high) = (i.saturating_sub(half), (i + half + 1).min(flux.len()));
            let mean = (prefix[high] - prefix[low]) / (high - low) as f64;
            (f64::from(flux[i]) - mean).max(0.0)
        })
        .collect();
    // A three-tap smoothing keeps the peak where it is and calms single-frame noise.
    let smoothed: Vec<f64> = (0..rectified.len())
        .map(|i| {
            let before = if i > 0 { rectified[i - 1] } else { rectified[i] };
            let after = rectified.get(i + 1).copied().unwrap_or(rectified[i]);
            0.25 * before + 0.5 * rectified[i] + 0.25 * after
        })
        .collect();
    let mean = smoothed.iter().sum::<f64>() / smoothed.len() as f64;
    let deviation = (smoothed.iter().map(|value| (value - mean).powi(2)).sum::<f64>() / smoothed.len() as f64).sqrt();
    if deviation <= 1e-9 {
        return vec![0.0; smoothed.len()];
    }
    smoothed.into_iter().map(|value| value / deviation).collect()
}

/// The beat period in frames (fractional), or `None` when the envelope is too short or flat.
fn tempo_period(envelope: &[f64], frames_per_second: f64) -> Option<f64> {
    let shortest = (60.0 * frames_per_second / MAX_TEMPO_SEARCHED).floor().max(1.0) as usize;
    if (envelope.len() as f64) < MIN_TEMPO_SECONDS * frames_per_second {
        return None;
    }
    // Two periods of the slowest tempo looked for fit in the sound.
    let longest = ((60.0 * frames_per_second / MIN_TEMPO).ceil() as usize).min(envelope.len() / 2);
    if longest <= shortest {
        return None;
    }
    let reach = longest * TEMPO_MULTIPLES + 2;
    let mean = envelope.iter().sum::<f64>() / envelope.len() as f64;
    let centred: Vec<f64> = envelope.iter().map(|value| value - mean).collect();
    let reach = reach.min(centred.len() - 1);
    let correlation: Vec<f64> = (0..=reach)
        .map(|lag| centred[lag..].iter().zip(&centred).map(|(a, b)| a * b).sum::<f64>() / (centred.len() - lag) as f64)
        .collect();
    let at = |lag: f64| -> f64 {
        let below = lag.floor() as usize;
        if below + 1 >= correlation.len() {
            return 0.0;
        }
        let fraction = lag - below as f64;
        correlation[below] * (1.0 - fraction) + correlation[below + 1] * fraction
    };
    let score = |period: f64| -> f64 {
        let bpm = 60.0 * frames_per_second / period;
        let weight = (-0.5 * ((bpm / TEMPO_CENTER).log2() / TEMPO_SPREAD_OCTAVES).powi(2)).exp();
        // A short sound holds fewer multiples of a long period: the mean of those it holds, so a
        // slow tempo is not scored down for them.
        let held: Vec<f64> = (1..=TEMPO_MULTIPLES).map(|multiple| period * multiple as f64).filter(|lag| (lag.floor() as usize) + 1 < correlation.len()).collect();
        if held.is_empty() {
            return 0.0;
        }
        let summed: f64 = held.iter().map(|lag| at(*lag)).sum();
        weight * summed * TEMPO_MULTIPLES as f64 / held.len() as f64
    };
    let best = (shortest..=longest).max_by(|a, b| score(*a as f64).total_cmp(&score(*b as f64)))?;
    if score(best as f64) <= 0.0 {
        return None;
    }
    // Hundredths of a frame around the best whole lag.
    let mut period = best as f64;
    let mut best_score = score(period);
    let mut candidate = (best as f64 - 1.0).max(shortest as f64);
    while candidate <= (best as f64 + 1.0).min(longest as f64) {
        let value = score(candidate);
        if value > best_score {
            best_score = value;
            period = candidate;
        }
        candidate += 0.01;
    }
    Some(period)
}

/// Beat frames by dynamic programming on the envelope with the beat `period` in frames.
fn track_beats(envelope: &[f64], period: f64) -> Vec<usize> {
    let count = envelope.len();
    let mut score = vec![0.0_f64; count];
    let mut back: Vec<Option<usize>> = vec![None; count];
    let nearest = (period / 2.0).round().max(1.0) as usize;
    let farthest = (2.0 * period).round() as usize;
    // The penalty depends on the gap alone: worked out once per gap, not per frame and gap.
    let penalties: Vec<f64> = (0..=farthest).map(|gap| TIGHTNESS * (gap as f64 / period).ln().powi(2)).collect();
    for t in 0..count {
        let mut best: Option<(f64, usize)> = None;
        if t >= nearest {
            for gap in nearest..=farthest.min(t) {
                let previous = t - gap;
                let value = score[previous] - penalties[gap];
                if best.is_none_or(|(best_value, _)| value > best_value) {
                    best = Some((value, previous));
                }
            }
        }
        match best.filter(|(value, _)| *value > 0.0) {
            Some((value, previous)) => {
                score[t] = envelope[t] + value;
                back[t] = Some(previous);
            }
            None => score[t] = envelope[t],
        }
    }
    let tail = (period.round() as usize).min(count);
    let Some(mut frame) = (count - tail..count).max_by(|a, b| score[*a].total_cmp(&score[*b])) else { return Vec::new() };
    let mut beats = vec![frame];
    while let Some(previous) = back[frame] {
        beats.push(previous);
        frame = previous;
    }
    beats.reverse();
    beats
}

/// Moves each beat to the strongest onset within a frame of it, keeping them in order.
fn refine_beats(envelope: &[f64], beats: &mut [usize]) {
    for index in 0..beats.len() {
        let frame = beats[index];
        let low = frame.saturating_sub(1).max(if index > 0 { beats[index - 1] + 1 } else { 0 });
        let high = (frame + 1).min(envelope.len().saturating_sub(1));
        if let Some(best) = (low..=high).max_by(|a, b| envelope[*a].total_cmp(&envelope[*b])) {
            beats[index] = best;
        }
    }
}

/// The strongest envelope value within two frames of `frame`.
fn strength_near(envelope: &[f64], frame: usize) -> f64 {
    let (low, high) = (frame.saturating_sub(2), (frame + 3).min(envelope.len()));
    envelope[low..high].iter().copied().fold(0.0, f64::max)
}

/// The range of beats to keep: without the beats at either end that are quieter than
/// [`EDGE_STRENGTH`] of the median beat.
fn trim_quiet_edges(strengths: &[f64]) -> (usize, usize) {
    if strengths.is_empty() {
        return (0, 0);
    }
    let mut sorted = strengths.to_vec();
    sorted.sort_by(f64::total_cmp);
    let threshold = sorted[sorted.len() / 2] * EDGE_STRENGTH;
    let first = strengths.iter().position(|&value| value > threshold).unwrap_or(strengths.len());
    let last = strengths.iter().rposition(|&value| value > threshold).map_or(first, |last| last + 1);
    (first, last.max(first))
}

/// Which beats start a bar: every [`BEATS_PER_BAR`]th, from the phase whose beats are strongest
/// (the earlier phase on a tie).
fn downbeat_flags(strengths: &[f64]) -> Vec<bool> {
    let phase = (0..BEATS_PER_BAR)
        .map(|phase| (phase, strengths.iter().skip(phase).step_by(BEATS_PER_BAR).sum::<f64>()))
        .fold((0, f64::MIN), |best, candidate| if candidate.1 > best.1 { candidate } else { best })
        .0;
    (0..strengths.len()).map(|index| index % BEATS_PER_BAR == phase).collect()
}

/// Mono at [`ANALYSIS_RATE`] from interleaved stereo at four times that rate: the average of the
/// two channels over four frames, a sample that is not a finite number counting as silence.
/// Appends to `out`.
pub fn downmix_quarter(stereo: &[f32], out: &mut Vec<f32>) {
    for group in stereo.chunks_exact(8) {
        out.push(group.iter().map(|sample| if sample.is_finite() { *sample } else { 0.0 }).sum::<f32>() / 8.0);
    }
}

