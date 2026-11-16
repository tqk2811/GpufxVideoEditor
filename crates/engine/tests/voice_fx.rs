//! Per-clip noise reduction and voice change: what they do to the sound, and that a range mixed
//! after a seek is the same as the same range taken from a longer mix.

mod common;

use std::f64::consts::TAU;
use std::time::Instant;

use common::*;
use engine::render::{ClipKey, RenderError};
use engine::{
    mix_audio, mix_audio_with, set_denoise, set_voice, validate, AcceptAll, AudioOutput, AudioSource, ClipDenoise, ClipVoice, DenoiseMode, Output, Project, StretchStates, Track, TrackKind, ValidationError, VoiceEffect, SUPPORTED_VERSION,
};

const FRAME: i64 = 3000;
const SECOND: i64 = 90_000;
const RATE: usize = 48_000;

fn project_of(clips: Vec<engine::Clip>) -> Project {
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 16, height: 16, frame_duration: t(FRAME), audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 128_000 }) },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new(), locked: false, hidden: false, subtitle: None }],
        markers: Vec::new(), beats: Default::default(),
    }
}

fn key() -> ClipKey {
    ClipKey { track: 0, clip: 0 }
}

/// A clip of `seconds` at the start of the timeline, reading the source from `source_in` seconds.
fn clip_with(seconds: i64, source_in: i64, denoise: ClipDenoise, voice: ClipVoice, speed: f64) -> engine::Clip {
    let mut clip = clip("a", 0, source_in * SECOND, seconds * SECOND);
    clip.denoise = denoise;
    clip.voice = voice;
    clip.speed = speed;
    clip
}

fn denoise(mode: DenoiseMode, strength: f64) -> ClipDenoise {
    ClipDenoise { mode, strength }
}

fn voice(semitones: f64, effect: VoiceEffect) -> ClipVoice {
    ClipVoice { semitones, effect }
}

/// White noise from a hash of the position, so any position can be read at any time.
fn noise_at(position: i64, channel: u64) -> f32 {
    let mut x = (position as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ channel.wrapping_mul(0xD1B5_4A32_D192_ED03);
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
}

/// What the test source plays: a clean signal plus white noise, addressed by source sample.
#[derive(Clone, Copy)]
enum Clean {
    /// A sine of this frequency.
    Tone(f64),
    /// A voiced sound: harmonics of 140 Hz falling by 1/k, in syllables of a quarter second with a
    /// pause of a quarter second after each.
    Voiced,
}

impl Clean {
    fn at(self, position: i64) -> f32 {
        let time = position as f64 / RATE as f64;
        match self {
            Clean::Tone(hz) => (TAU * hz * time).sin() as f32 * 0.3,
            Clean::Voiced => {
                let harmonics: f64 = (1..=24).map(|k| (TAU * 140.0 * k as f64 * time).sin() / k as f64).sum();
                let syllable = (TAU * 2.0 * time).sin().max(0.0);
                (harmonics * syllable * 0.15) as f32
            }
        }
    }
}

struct Noisy {
    clean: Clean,
    noise: f32,
    /// Frames the source has; reads past it are short.
    length: i64,
}

impl AudioSource for Noisy {
    fn read(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        self.read_counted(key, source, position, out).map(|_| ())
    }

    fn read_counted(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<usize, RenderError> {
        let mut written = 0;
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let at = position + i as i64;
            if at >= self.length {
                break;
            }
            let clean = self.clean.at(at);
            frame[0] = clean + self.noise * noise_at(at, 0);
            frame[1] = clean + self.noise * noise_at(at, 1);
            written += 1;
        }
        Ok(written)
    }
}

fn source(clean: Clean, noise: f32) -> Noisy {
    Noisy { clean, noise, length: i64::MAX }
}

fn mix(project: &Project, source: &mut Noisy, start: i64, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    mix_audio(project, source, start, &mut out).expect("mix");
    out
}

/// How much of the clean signal `out` holds, and the level of the rest, both of the left channel:
/// `(gain, residual rms)`. The clean signal starts at source sample `from`.
fn split(out: &[f32], clean: Clean, from: i64) -> (f64, f64) {
    let left: Vec<f64> = out.chunks_exact(2).map(|frame| f64::from(frame[0])).collect();
    let reference: Vec<f64> = (0..left.len()).map(|i| f64::from(clean.at(from + i as i64))).collect();
    let dot: f64 = left.iter().zip(&reference).map(|(a, b)| a * b).sum();
    let energy: f64 = reference.iter().map(|b| b * b).sum();
    let gain = dot / energy;
    let residual: f64 = left.iter().zip(&reference).map(|(a, b)| (a - gain * b).powi(2)).sum();
    (gain, (residual / left.len() as f64).sqrt())
}

/// The level of the left channel during the pauses of [`Clean::Voiced`], away from their edges,
/// for audio starting at source sample `from`.
fn pause_noise(out: &[f32], from: i64) -> f64 {
    let in_pause = |i: usize| ((from + i as i64) as f64 / RATE as f64 * 2.0).fract();
    let samples: Vec<f64> = out.chunks_exact(2).enumerate().filter(|(i, _)| (0.56..0.94).contains(&in_pause(*i))).map(|(_, frame)| f64::from(frame[0])).collect();
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

#[test]
fn both_modes_lower_the_noise_and_keep_the_signal() {
    // The network keeps speech, so it gets a voiced sound; the gate gets a steady tone.
    for (mode, clean) in [(DenoiseMode::Voice, Clean::Voiced), (DenoiseMode::General, Clean::Tone(1000.0))] {
        let seconds = 4;
        let frames = seconds as usize * RATE;
        let dry = project_of(vec![clip_with(seconds, 10, ClipDenoise::default(), ClipVoice::default(), 1.0)]);
        let wet = project_of(vec![clip_with(seconds, 10, denoise(mode, 1.0), ClipVoice::default(), 1.0)]);
        // Leave out the first second: the comparison is about the settled sound.
        let from = 10 * RATE as i64 + RATE as i64;
        let before = &mix(&dry, &mut source(clean, 0.05), 0, frames)[2 * RATE..];
        let after = &mix(&wet, &mut source(clean, 0.05), 0, frames)[2 * RATE..];
        let (dry_gain, mut dry_noise) = split(before, clean, from);
        let (wet_gain, mut wet_noise) = split(after, clean, from);
        // What the network does to a voice counts as change too, so its noise is measured where
        // the voice pauses.
        if mode == DenoiseMode::Voice {
            (dry_noise, wet_noise) = (pause_noise(before, from), pause_noise(after, from));
        }
        let lowered = db(dry_noise / wet_noise);
        let kept = db(wet_gain / dry_gain);
        println!("{mode:?}: noise lowered by {lowered:.1} dB, signal changed by {kept:.2} dB");
        let least = if mode == DenoiseMode::Voice { 15.0 } else { 9.0 };
        assert!(lowered > least, "{mode:?}: noise lowered by only {lowered:.1} dB");
        assert!(kept.abs() < 1.5, "{mode:?}: signal changed by {kept:.2} dB");
    }
}

#[test]
fn half_strength_does_half_the_work() {
    for mode in [DenoiseMode::Voice, DenoiseMode::General] {
        let level = |strength: f64| {
            let project = project_of(vec![clip_with(3, 0, denoise(mode, strength), ClipVoice::default(), 1.0)]);
            let out = mix(&project, &mut source(Clean::Tone(1000.0), 0.0).with_noise_only(), 0, 3 * RATE);
            rms(&out[2 * RATE..])
        };
        let (full, half, none) = (level(1.0), level(0.5), level(0.0));
        assert!(full < half && half < none, "{mode:?}: {full} {half} {none}");
    }
}

impl Noisy {
    fn with_noise_only(mut self) -> Self {
        self.clean = Clean::Tone(0.0);
        self.noise = 0.05;
        self
    }
}

fn rms(stereo: &[f32]) -> f64 {
    (stereo.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / stereo.len() as f64).sqrt()
}

/// The frequency of the left channel, from its rising zero crossings with linear interpolation.
fn measured_hz(stereo: &[f32]) -> f64 {
    let left: Vec<f32> = stereo.chunks_exact(2).map(|frame| frame[0]).collect();
    let mut crossings = Vec::new();
    for i in 1..left.len() {
        if left[i - 1] < 0.0 && left[i] >= 0.0 {
            crossings.push(i as f64 - f64::from(left[i]) / f64::from(left[i] - left[i - 1]));
        }
    }
    let periods = crossings.len() - 1;
    periods as f64 * RATE as f64 / (crossings[periods] - crossings[0])
}

#[test]
fn a_pitch_shift_changes_the_frequency_and_keeps_the_length() {
    for speed in [1.0, 0.5, 2.0] {
        for semitones in [12.0, 7.0, -5.0, -12.0, 0.5] {
            let seconds = 4;
            let project = project_of(vec![clip_with(seconds, 3, ClipDenoise::default(), voice(semitones, VoiceEffect::None), speed)]);
            let frames = seconds as usize * RATE;
            let out = mix(&project, &mut source(Clean::Tone(440.0), 0.0), 0, frames);
            assert_eq!(out.len(), frames * 2);
            let middle = &out[2 * 12_000..2 * (frames - 12_000)];
            let expected = 440.0 * 2f64.powf(semitones / 12.0);
            let hz = measured_hz(middle);
            assert!((hz / expected - 1.0).abs() < 0.006, "{speed}x, {semitones} st: {hz} Hz, expected {expected}");
            let level = rms(middle);
            assert!((0.15..0.26).contains(&level), "{speed}x, {semitones} st: rms {level}");
            // The sound lasts as long as the clip: no silence at either end.
            assert!(rms(&out[..2 * 4800]) > 0.1 && rms(&out[2 * (frames - 4800)..]) > 0.1, "{speed}x, {semitones} st: the ends are silent");
        }
    }
}

#[test]
fn the_robot_effect_modulates_the_level_at_its_rate() {
    let project = project_of(vec![clip_with(2, 0, ClipDenoise::default(), voice(0.0, VoiceEffect::Robot), 1.0)]);
    let out = mix(&project, &mut source(Clean::Tone(1000.0), 0.0), 0, 2 * RATE);
    // A 1 kHz tone times a 50 Hz sine is 950 Hz plus 1050 Hz: no energy left at 1 kHz.
    let (gain, rest) = split(&out, Clean::Tone(1000.0), 0);
    assert!(gain.abs() < 0.01, "carrier left: {gain}");
    assert!((rest - 0.3 / 2.0).abs() < 0.01, "level {rest}");
}

/// Every effect and their combinations, to check against seeks.
fn variants() -> Vec<(&'static str, ClipDenoise, ClipVoice, f64)> {
    vec![
        ("voice denoise", denoise(DenoiseMode::Voice, 1.0), ClipVoice::default(), 1.0),
        ("general denoise", denoise(DenoiseMode::General, 0.7), ClipVoice::default(), 1.0),
        ("pitch up", ClipDenoise::default(), voice(7.0, VoiceEffect::None), 1.0),
        ("pitch down at speed", ClipDenoise::default(), voice(-5.0, VoiceEffect::None), 1.5),
        ("robot", ClipDenoise::default(), voice(0.0, VoiceEffect::Robot), 1.0),
        ("everything", denoise(DenoiseMode::General, 1.0), voice(9.0, VoiceEffect::Robot), 0.75),
        ("voice denoise and pitch", denoise(DenoiseMode::Voice, 0.5), voice(-7.0, VoiceEffect::None), 1.0),
    ]
}

#[test]
fn a_range_mixed_after_a_seek_equals_the_same_range_of_a_longer_mix() {
    let seconds = 6;
    let frames = seconds as usize * RATE;
    for (name, denoise, voice, speed) in variants() {
        let project = project_of(vec![clip_with(seconds, 2, denoise, voice, speed)]);
        // The whole clip in blocks of an odd size, as playback does.
        let mut whole = vec![0.0f32; frames * 2];
        let mut states = StretchStates::default();
        let mut noisy = source(Clean::Voiced, 0.05);
        for (n, block) in whole.chunks_mut(2 * 1111).enumerate() {
            mix_audio_with(&project, &mut noisy, &mut states, (n * 1111) as i64, block).unwrap();
        }
        // Ranges starting anywhere, across chunk and key grain boundaries, each from a fresh state.
        for (start, len) in [(0usize, 5000usize), (47_000, 3000), (95_999, 2), (131_072, 20_000), (200_001, 48_000)] {
            let mut part = vec![0.0f32; len * 2];
            mix_audio_with(&project, &mut source(Clean::Voiced, 0.05), &mut StretchStates::default(), start as i64, &mut part).unwrap();
            assert!(part == whole[start * 2..(start + len) * 2], "{name}: range {start} + {len} differs after a seek");
        }
    }
}

#[test]
fn denoise_reads_the_source_forward_only() {
    // A decoder seeks on every read that goes back.
    struct Forward {
        inner: Noisy,
        end: i64,
    }
    impl AudioSource for Forward {
        fn read(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
            self.read_counted(key, source, position, out).map(|_| ())
        }
        fn read_counted(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<usize, RenderError> {
            assert!(position >= self.end, "read at {position} goes back before {}", self.end);
            self.end = position + (out.len() / 2) as i64;
            self.inner.read_counted(key, source, position, out)
        }
    }
    for (name, denoise, voice, speed) in variants() {
        let project = project_of(vec![clip_with(5, 1, denoise, voice, speed)]);
        let mut forward = Forward { inner: source(Clean::Voiced, 0.05), end: 0 };
        let mut states = StretchStates::default();
        let mut block = vec![0.0f32; 2 * 960];
        for n in 0..5 * RATE / 960 {
            mix_audio_with(&project, &mut forward, &mut states, (n * 960) as i64, &mut block).unwrap_or_else(|_| panic!("{name}"));
        }
    }
}

#[test]
fn the_end_of_the_source_stays_silent_after_denoise() {
    for mode in [DenoiseMode::Voice, DenoiseMode::General] {
        let project = project_of(vec![clip_with(3, 0, denoise(mode, 1.0), ClipVoice::default(), 1.0)]);
        let mut short = Noisy { clean: Clean::Tone(440.0), noise: 0.05, length: 60_000 };
        let out = mix(&project, &mut short, 0, 3 * RATE);
        assert!(out[2 * 60_000..].iter().all(|sample| *sample == 0.0), "{mode:?}: sound past the end of the file");
        assert!(rms(&out[2 * 50_000..2 * 59_000]) > 0.1, "{mode:?}: the end of the file is lost");
    }
}

#[test]
fn a_file_ending_before_the_first_window_of_its_last_second_mixes() {
    // The second second starts at 48000 but its first gate window at 48128 (the hop is 512): a
    // file ending between them has a chunk with no window in it.
    for length in [48_001, 48_100, 48_128, 96_000 + 100] {
        let project = project_of(vec![clip_with(3, 0, denoise(DenoiseMode::General, 1.0), ClipVoice::default(), 1.0)]);
        let mut short = Noisy { clean: Clean::Tone(440.0), noise: 0.05, length };
        let out = mix(&project, &mut short, 0, 3 * RATE);
        assert!(out[2 * length as usize..].iter().all(|sample| *sample == 0.0), "{length}: sound past the end of the file");
    }
}

#[test]
fn defaults_are_left_out_of_the_file_and_settings_survive_a_round_trip() {
    let plain = project_of(vec![clip_with(1, 0, ClipDenoise::default(), ClipVoice::default(), 1.0)]);
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("denoise") && !json.contains("voice"), "{json}");
    // A strength with the mode off is not worth keeping either.
    let off = project_of(vec![clip_with(1, 0, denoise(DenoiseMode::Off, 0.3), ClipVoice::default(), 1.0)]);
    assert!(!serde_json::to_string(&off).unwrap().contains("denoise"));

    let set = project_of(vec![clip_with(1, 0, denoise(DenoiseMode::General, 0.4), voice(-3.5, VoiceEffect::Robot), 1.0)]);
    let json = serde_json::to_string(&set).unwrap();
    assert!(json.contains(r#""denoise":{"mode":"general","strength":0.4}"#), "{json}");
    assert!(json.contains(r#""voice":{"semitones":-3.5,"effect":"robot"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), set);
    // Full strength and no effect are the defaults inside.
    let full = project_of(vec![clip_with(1, 0, denoise(DenoiseMode::Voice, 1.0), voice(2.0, VoiceEffect::None), 1.0)]);
    let json = serde_json::to_string(&full).unwrap();
    assert!(json.contains(r#""denoise":{"mode":"voice"}"#) && json.contains(r#""voice":{"semitones":2.0}"#), "{json}");
    assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), full);
}

#[test]
fn out_of_range_settings_are_refused() {
    let project = project_of(vec![clip_with(1, 0, ClipDenoise::default(), ClipVoice::default(), 1.0)]);
    assert!(set_denoise(&project, key(), denoise(DenoiseMode::General, 0.5)).is_ok());
    assert!(set_voice(&project, key(), voice(-12.0, VoiceEffect::Robot)).is_ok());
    for strength in [-0.1, 1.5, f64::NAN] {
        let refused = set_denoise(&project, key(), denoise(DenoiseMode::General, strength));
        assert!(refused.is_err(), "strength {strength}");
    }
    for semitones in [12.5, -13.0, f64::INFINITY] {
        assert!(set_voice(&project, key(), voice(semitones, VoiceEffect::None)).is_err(), "{semitones} st");
    }
    let mut bad = project.clone();
    bad.tracks[0].clips[0].voice.semitones = 20.0;
    bad.tracks[0].clips[0].denoise.strength = 2.0;
    let errors = validate(&bad, &AcceptAll);
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidPitch { .. })), "{errors:?}");
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidDenoiseStrength { .. })), "{errors:?}");
}

/// Milliseconds of work per second of audio for each effect, mixing in blocks as playback does.
/// Run it in release: `cargo test -p engine --release --test voice_fx -- --ignored --nocapture`.
#[test]
#[ignore]
fn cost_per_second_of_audio() {
    let seconds = 20;
    let frames = seconds as usize * RATE;
    let mut cases = vec![("plain", ClipDenoise::default(), ClipVoice::default(), 1.0)];
    cases.extend(variants());
    for (name, denoise, voice, speed) in cases {
        let project = project_of(vec![clip_with(seconds, 0, denoise, voice, speed)]);
        let mut states = StretchStates::default();
        let mut noisy = source(Clean::Voiced, 0.05);
        let mut block = vec![0.0f32; 2 * 1024];
        let started = Instant::now();
        let mut at = 0;
        while at < frames {
            mix_audio_with(&project, &mut noisy, &mut states, at as i64, &mut block).unwrap();
            at += 1024;
        }
        let per_second = started.elapsed().as_secs_f64() * 1000.0 / seconds as f64;
        // A seek: the first block after it pays for two runs.
        let started = Instant::now();
        mix_audio_with(&project, &mut noisy, &mut StretchStates::default(), 7 * RATE as i64 + 12_345, &mut block).unwrap();
        let seek = started.elapsed().as_secs_f64() * 1000.0;
        println!("{name:>24}: {per_second:6.2} ms per second of audio, first block after a seek {seek:6.2} ms");
    }
}

/// Both runs either side of a chunk boundary process the same audio, so their blend must keep the
/// level: an equal-power blend of correlated runs rises 3 dB in its middle, once per second.
#[test]
fn chunk_boundaries_keep_the_level_of_a_steady_tone() {
    for mode in [DenoiseMode::General, DenoiseMode::Voice] {
        let seconds = 4;
        let project = project_of(vec![clip_with(seconds, 0, denoise(mode, 1.0), ClipVoice::default(), 1.0)]);
        let out = mix(&project, &mut source(Clean::Tone(1000.0), 0.0), 0, seconds as usize * RATE);
        // The peak of each 10 ms window from the second second on: the tone fills every window.
        let peaks: Vec<f32> = out[2 * RATE..].chunks_exact(2 * 480).map(|window| window.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))).collect();
        let mut sorted = peaks.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        let highest = sorted[sorted.len() - 1];
        let bump = db(f64::from(highest / median));
        println!("{mode:?}: highest window {bump:.2} dB above the median");
        assert!(bump < 1.0, "{mode:?}: a window stands {bump:.2} dB above the median");
    }
}

/// The runs hold only the mode's processing at full strength, so a new strength reuses them and
/// sounds exactly as a fresh mix at that strength.
#[test]
fn a_new_strength_keeps_the_runs_and_equals_a_fresh_mix() {
    struct Counting {
        inner: Noisy,
        frames: usize,
    }
    impl AudioSource for Counting {
        fn read(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
            self.read_counted(key, source, position, out).map(|_| ())
        }
        fn read_counted(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<usize, RenderError> {
            self.frames += out.len() / 2;
            self.inner.read_counted(key, source, position, out)
        }
    }
    for mode in [DenoiseMode::Voice, DenoiseMode::General] {
        let at = |strength: f64| project_of(vec![clip_with(3, 0, denoise(mode, strength), ClipVoice::default(), 1.0)]);
        let start = RATE as i64 + 4_321;
        let mut states = StretchStates::default();
        let mut counting = Counting { inner: source(Clean::Voiced, 0.05), frames: 0 };
        let mut block = vec![0.0f32; 2 * 1024];
        mix_audio_with(&at(1.0), &mut counting, &mut states, start, &mut block).unwrap();
        let read = counting.frames;
        mix_audio_with(&at(0.35), &mut counting, &mut states, start, &mut block).unwrap();
        assert_eq!(counting.frames, read, "{mode:?}: the new strength read the source again");
        let fresh = mix(&at(0.35), &mut source(Clean::Voiced, 0.05), start, 1024);
        assert!(block == fresh, "{mode:?}: the reused runs sound different from a fresh mix");
    }
}

/// The robot effect follows the timeline, so a clip split anywhere sounds as it did whole.
#[test]
fn a_split_robot_clip_keeps_its_phase_across_the_cut() {
    let whole = project_of(vec![clip_with(2, 0, ClipDenoise::default(), voice(0.0, VoiceEffect::Robot), 1.0)]);
    // 1 s and one frame: 49 600 samples, not a whole number of 50 Hz periods.
    let cut = engine::split_clip(&whole, key(), t(SECOND + FRAME)).unwrap();
    let before = mix(&whole, &mut source(Clean::Tone(1000.0), 0.0), 0, 2 * RATE);
    let after = mix(&cut, &mut source(Clean::Tone(1000.0), 0.0), 0, 2 * RATE);
    let worst = before.iter().zip(&after).fold(0.0f32, |worst, (a, b)| worst.max((a - b).abs()));
    assert!(worst < 1e-6, "the split clip differs by {worst}");
}

/// The residual noise of steady white noise must stay level over time: a noise floor tracked
/// afresh in every chunk lets it rise and fall once per second, which sounds like breathing.
#[test]
fn steady_noise_stays_level_after_general_denoise() {
    for mode in [DenoiseMode::General, DenoiseMode::Voice] {
        let seconds = 10;
        let project = project_of(vec![clip_with(seconds, 3, denoise(mode, 1.0), ClipVoice::default(), 1.0)]);
        let out = mix(&project, &mut source(Clean::Tone(1000.0), 0.0).with_noise_only(), 0, seconds as usize * RATE);
        // The level of each tenth of a second from the second second on, averaged over the
        // seconds by its place in the second: the chunks are one second long.
        let slot = RATE / 10;
        let mut by_phase = [0.0f64; 10];
        let mut counts = [0usize; 10];
        for (n, block) in out[2 * RATE..].chunks_exact(2 * slot).enumerate() {
            by_phase[n % 10] += rms(block).powi(2);
            counts[n % 10] += 1;
        }
        let levels: Vec<f64> = by_phase.iter().zip(&counts).map(|(power, count)| 10.0 * (power / *count as f64).log10()).collect();
        let highest = levels.iter().cloned().fold(f64::MIN, f64::max);
        let lowest = levels.iter().cloned().fold(f64::MAX, f64::min);
        let swing = highest - lowest;
        println!("{mode:?}: residual noise swings {swing:.2} dB within a second: {levels:.1?}");
        if mode == DenoiseMode::General {
            assert!(swing < 1.0, "{mode:?}: the residual noise swings {swing:.2} dB within each second");
        }
    }
}
