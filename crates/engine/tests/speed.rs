//! Clip speed: the tick mapping, the edits that follow it and the pitch-preserving time stretch.

mod common;

use common::*;
use engine::render::{ClipKey, RenderError};
use engine::schedule::LayerContent;
use engine::{
    add_transition, audio_segments, layers_at_frame, mix_audio, mix_audio_with, set_speed, set_fades, split_clip, trim_end, trim_start, validate, AcceptAll, AudioSource, AudioOutput, EditError, EditHistory, Output, Project, StretchStates, Track, TrackKind, Transition, ValidationError, SUPPORTED_VERSION,
};

const FRAME: i64 = 3000;
const SECOND: i64 = 90_000;
const SPEEDS: [f64; 9] = [0.1, 0.25, 0.5, 0.75, 1.0, 1.25, 1.234, 2.0, 10.0];

fn project_of(clips: Vec<engine::Clip>) -> Project {
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 16, height: 16, frame_duration: t(FRAME), audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 128_000 }) },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new(), locked: false, hidden: false }],
    }
}

fn sped_clip(start: i64, source_in: i64, duration: i64, speed: f64) -> engine::Clip {
    let mut clip = clip("a", start, source_in, duration);
    clip.speed = speed;
    clip
}

fn key(clip: usize) -> ClipKey {
    ClipKey { track: 0, clip }
}

/// The source time of the clip shown at each frame of the project.
fn source_times(project: &Project) -> Vec<Option<i64>> {
    (0..engine::frame_count(project))
        .map(|frame| {
            layers_at_frame(project, frame).first().map(|layer| match layer.content {
                LayerContent::Clip(sample) => sample.source_time.ticks(),
                LayerContent::Transition { from, .. } => from.source_time.ticks(),
            })
        })
        .collect()
}

#[test]
fn a_clip_at_another_speed_reads_the_source_at_that_rate() {
    let fast = project_of(vec![sped_clip(0, 9_000, 300 * FRAME, 2.0)]);
    assert_eq!(source_times(&fast)[3], Some(9_000 + 6 * FRAME));
    let slow = project_of(vec![sped_clip(0, 0, 300 * FRAME, 0.5)]);
    assert_eq!(source_times(&slow)[3], Some(3 * FRAME / 2));
}

#[test]
fn the_mapping_does_not_drift_over_a_long_clip() {
    // Ten hours: a float mapping would be off by far more than a tick.
    let duration = t(10 * 3600 * SECOND);
    let clip = sped_clip(0, 0, duration.ticks(), 1.234);
    assert_eq!(clip.source_offset(duration), t(duration.ticks() * 1234 / 1000));
    assert_eq!(clip.source_end(), t(duration.ticks() * 1234 / 1000));
}

#[test]
fn a_speed_of_one_writes_no_field_and_an_old_file_round_trips_unchanged() {
    let old = r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":3000},"tracks":[{"clips":[{"source":"a","start":0,"source_in":10,"duration":90000,"effects":[]}]}]}"#;
    let parsed = Project::from_json(old).expect("old file");
    assert_eq!(parsed.tracks[0].clips[0].speed, 1.0);
    let saved = serde_json::to_string(&parsed).unwrap();
    assert!(!saved.contains("speed"), "{saved}");
    assert_eq!(Project::from_json(&saved).unwrap(), parsed);
    assert!(validate(&parsed, &AcceptAll).is_empty());
}

#[test]
fn a_speed_is_saved_and_loaded() {
    let project = project_of(vec![sped_clip(0, 0, SECOND, 2.0)]);
    let saved = serde_json::to_string(&project).unwrap();
    assert!(saved.contains("\"speed\":2.0"), "{saved}");
    assert_eq!(Project::from_json(&saved).unwrap(), project);
}

#[test]
fn a_speed_out_of_range_is_invalid() {
    for bad in [0.05, 10.5, 0.0, -1.0, f64::NAN, f64::INFINITY] {
        let errors = validate(&project_of(vec![sped_clip(0, 0, SECOND, bad)]), &AcceptAll);
        assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidSpeed { .. })), "{bad}: {errors:?}");
    }
    for good in [0.1, 1.0, 10.0] {
        assert!(validate(&project_of(vec![sped_clip(0, 0, SECOND, good)]), &AcceptAll).is_empty(), "{good}");
    }
}

/// A deterministic spread of cut positions over a clip.
fn offsets(duration: i64) -> Vec<i64> {
    (1..40).map(|i| 1 + (i * 7919 * 97) % (duration - 2)).collect()
}

#[test]
fn a_split_anywhere_keeps_every_source_position() {
    for speed in SPEEDS {
        let base = project_of(vec![sped_clip(1000, 555, 10 * SECOND, speed)]);
        let before = source_times(&base);
        for cut in offsets(10 * SECOND) {
            let at = t(1000 + cut);
            let Ok(split) = split_clip(&base, key(0), at) else { panic!("{speed}: split at {cut} refused") };
            assert_eq!(split.tracks[0].clips.len(), 2);
            let first = &split.tracks[0].clips[0];
            let second = &split.tracks[0].clips[1];
            assert_eq!(second.start.ticks() - first.start.ticks(), first.duration.ticks(), "{speed} {cut}");
            assert!((second.start.ticks() - at.ticks()).abs() < 1000, "{speed} {cut}: {}", second.start.ticks());
            assert_eq!(source_times(&split), before, "speed {speed}, cut {cut}");
        }
    }
}

#[test]
fn a_start_trim_keeps_the_frames_under_the_end_and_undoes() {
    for speed in SPEEDS {
        let project = project_of(vec![sped_clip(SECOND, 20 * SECOND, 8 * SECOND, speed)]);
        let original = source_times(&project);
        for offset in offsets(4 * SECOND) {
            let mut history = EditHistory::new(project.clone(), 8);
            let new_start = t(SECOND + offset);
            let edited = history.apply(|p| trim_start(p, key(0), new_start)).expect("trim").clone();
            let clip = &edited.tracks[0].clips[0];
            assert!(clip.start.ticks() >= new_start.ticks() && clip.start.ticks() - new_start.ticks() < 1000, "{speed}");
            assert_eq!(clip.end(), Some(t(9 * SECOND)), "the end edge does not move");
            let after = source_times(&edited);
            let first_full = clip.start.ticks().div_euclid(FRAME) as usize + 1;
            assert_eq!(after[first_full..], original[first_full..], "speed {speed}, offset {offset}");
            assert!(history.undo());
            assert_eq!(history.current(), &project, "one undo step");
        }
    }
}

#[test]
fn trimming_the_start_toward_the_source_start_stops_at_zero() {
    let project = project_of(vec![sped_clip(10 * SECOND, 2 * SECOND, SECOND, 2.0)]);
    // Two source seconds at 2x are one timeline second of slack.
    assert!(trim_start(&project, key(0), t(9 * SECOND)).is_ok());
    assert!(matches!(trim_start(&project, key(0), t(9 * SECOND - 2000)), Err(EditError::BeforeSourceStart { .. })));
}

#[test]
fn an_end_trim_does_not_move_the_start_of_the_clip_in_the_source() {
    let project = project_of(vec![sped_clip(0, 500, 2 * SECOND, 2.0)]);
    let edited = trim_end(&project, key(0), t(SECOND)).unwrap();
    assert_eq!(edited.tracks[0].clips[0].source_in, t(500));
    assert_eq!(edited.tracks[0].clips[0].source_end(), t(500 + 2 * SECOND));
}

#[test]
fn set_speed_keeps_the_source_range_and_is_one_undo_step() {
    let project = project_of(vec![sped_clip(SECOND, 500, 4 * SECOND, 1.0)]);
    let mut history = EditHistory::new(project.clone(), 8);
    history.apply(|p| set_speed(p, key(0), 2.0)).expect("set speed");
    let clip = &history.current().tracks[0].clips[0];
    assert_eq!((clip.speed, clip.duration, clip.source_in, clip.start), (2.0, t(2 * SECOND), t(500), t(SECOND)));
    assert_eq!(clip.source_end(), t(500 + 4 * SECOND));
    assert!(history.undo());
    assert_eq!(history.current(), &project);
    // Half speed from there doubles it again.
    let slowed = set_speed(&project, key(0), 0.5).unwrap();
    assert_eq!(slowed.tracks[0].clips[0].duration, t(8 * SECOND));
    assert_eq!(slowed.tracks[0].clips[0].source_end(), t(500 + 4 * SECOND));
}

#[test]
fn a_new_speed_that_runs_into_the_next_clip_is_refused() {
    let project = project_of(vec![sped_clip(0, 0, 4 * SECOND, 1.0), sped_clip(4 * SECOND, 0, SECOND, 1.0)]);
    let error = set_speed(&project, key(0), 0.5).expect_err("needs eight seconds");
    assert_eq!(error, EditError::SpeedOverlap { key: key(0), end: t(8 * SECOND), next_start: t(4 * SECOND) });
    // Faster leaves a gap and moves nothing.
    let faster = set_speed(&project, key(0), 2.0).unwrap();
    assert_eq!(faster.tracks[0].clips[0].duration, t(2 * SECOND));
    assert_eq!(faster.tracks[0].clips[1].start, t(4 * SECOND));
}

#[test]
fn a_locked_track_refuses_a_speed() {
    let mut project = project_of(vec![sped_clip(0, 0, SECOND, 1.0)]);
    project.tracks[0].locked = true;
    assert_eq!(set_speed(&project, key(0), 2.0).unwrap_err(), EditError::TrackLocked { track: 0 });
}

#[test]
fn a_speed_out_of_range_is_refused_as_invalid() {
    let project = project_of(vec![sped_clip(0, 0, SECOND, 1.0)]);
    assert!(matches!(set_speed(&project, key(0), 20.0), Err(EditError::Invalid(_))));
    assert!(matches!(set_speed(&project, key(0), f64::NAN), Err(EditError::Invalid(_))));
}

#[test]
fn a_transition_on_the_end_or_fades_that_no_longer_fit_refuse_a_speed() {
    let two = project_of(vec![sped_clip(0, 0, 4 * SECOND, 1.0), sped_clip(4 * SECOND, 0, 4 * SECOND, 1.0)]);
    let joined = add_transition(&two, 0, Transition { after_clip: 0, package: "t".into(), duration: t(SECOND), params: Default::default() }).unwrap();
    assert!(matches!(set_speed(&joined, key(0), 2.0), Err(EditError::TransitionOnEdge { edge: "end", .. })));

    let faded = set_fades(&project_of(vec![sped_clip(0, 0, 4 * SECOND, 1.0)]), key(0), t(2 * SECOND), t(SECOND)).unwrap();
    assert!(matches!(set_speed(&faded, key(0), 4.0), Err(EditError::Invalid(_))));
}

#[test]
fn asking_for_the_current_speed_changes_nothing() {
    let project = project_of(vec![sped_clip(0, 0, SECOND, 1.5)]);
    assert_eq!(set_speed(&project, key(0), 1.5).unwrap(), project);
}

/// A sine wave addressed by source sample.
struct Tone {
    hz: f64,
}

impl AudioSource for Tone {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let sample = (2.0 * std::f64::consts::PI * self.hz * (position + i as i64) as f64 / 48_000.0).sin() as f32 * 0.5;
            frame[0] = sample;
            frame[1] = sample;
        }
        Ok(())
    }
}

fn mix_tone(project: &Project, hz: f64, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    mix_audio(project, &mut Tone { hz }, 0, &mut out).expect("mix");
    out
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
    periods as f64 * 48_000.0 / (crossings[periods] - crossings[0])
}

fn rms(stereo: &[f32]) -> f32 {
    (stereo.iter().map(|s| s * s).sum::<f32>() / stereo.len() as f32).sqrt()
}

#[test]
fn the_stretch_keeps_the_pitch_and_the_length() {
    for speed in [0.1, 0.25, 0.5, 2.0, 4.0, 10.0] {
        let project = project_of(vec![sped_clip(0, 3 * SECOND, 4 * SECOND, speed)]);
        let out = mix_tone(&project, 440.0, 4 * 48_000);
        assert_eq!(out.len(), 4 * 48_000 * 2, "the mix has the timeline length");
        let middle = &out[2 * 12_000..2 * (4 * 48_000 - 12_000)];
        let hz = measured_hz(middle);
        assert!((hz - 440.0).abs() < 2.5, "{speed}x: {hz} Hz");
        let level = rms(middle);
        assert!((0.26..0.40).contains(&level), "{speed}x: rms {level}");
    }
}

/// Which source region each stretched block asked for, as `[(position, frames)]`.
#[derive(Default)]
struct Recording {
    reads: Vec<(i64, usize)>,
}

impl AudioSource for Recording {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        self.reads.push((position, out.len() / 2));
        Ok(())
    }
}

#[test]
fn the_stretch_reads_forward_and_stays_with_the_picture() {
    let project = project_of(vec![sped_clip(0, 0, 8 * SECOND, 2.0)]);
    let mut source = Recording::default();
    let mut out = vec![0.0f32; 96_000 * 2];
    let mut states = StretchStates::default();
    for block in 0..4 {
        mix_audio_with(&project, &mut source, &mut states, block * 96_000, &mut out).unwrap();
    }
    let mut end = 0;
    for (position, frames) in &source.reads {
        assert_eq!(*position, end, "reads are contiguous forward: {:?}", source.reads);
        end += *frames as i64;
    }
    // Eight timeline seconds at 2x consume sixteen source seconds, give or take a grain.
    assert!((end - 16 * 48_000).abs() < 16_000, "read to {end}");
}

/// A tone that records which source regions were asked for.
struct RecordedTone {
    tone: Tone,
    reads: Vec<(i64, usize)>,
}

impl AudioSource for RecordedTone {
    fn read(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        self.reads.push((position, out.len() / 2));
        self.tone.read(key, source, position, out)
    }
}

/// A real decoder seeks on any read that goes backwards, which is what makes playback at a high
/// speed fall behind. Key grains used to throw away the audio of the grain before them, and a
/// block whose first grain is that one read it again.
#[test]
fn small_blocks_across_key_grains_never_read_backwards() {
    for speed in [10.0, 4.0, 0.5] {
        for block in [1600, 800] {
            let project = project_of(vec![sped_clip(0, 0, 6 * SECOND, speed)]);
            let frames = 6 * 48_000;
            let mut source = RecordedTone { tone: Tone { hz: 440.0 }, reads: Vec::new() };
            let mut states = StretchStates::default();
            let mut out = vec![0.0f32; block * 2];
            let mut start = 0i64;
            while start < frames {
                mix_audio_with(&project, &mut source, &mut states, start, &mut out).unwrap();
                start += block as i64;
            }
            let mut end = 0;
            for (position, length) in &source.reads {
                assert!(*position >= end, "{speed}x, block {block}: read {position} goes back before {end}");
                end = position + *length as i64;
            }
            let decoded: i64 = source.reads.iter().map(|(_, length)| *length as i64).sum();
            let span = source.reads.last().map_or(0, |(position, length)| position + *length as i64);
            assert_eq!(decoded, span, "{speed}x, block {block}: nothing is decoded twice");
        }
    }
}

/// A tone of `length` samples that says where it ends, as a real file does.
struct EndingTone {
    hz: f64,
    length: i64,
    level: f32,
}

impl AudioSource for EndingTone {
    fn read(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        self.read_counted(key, source, position, out).map(|_| ())
    }

    fn read_counted(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<usize, RenderError> {
        let written = (self.length - position).clamp(0, out.len() as i64 / 2) as usize;
        for (i, frame) in out.chunks_exact_mut(2).take(written).enumerate() {
            let sample = (2.0 * std::f64::consts::PI * self.hz * (position + i as i64) as f64 / 48_000.0).sin() as f32 * self.level;
            frame[0] = sample;
            frame[1] = sample;
        }
        Ok(written)
    }
}

fn timeline_samples(ticks: i64) -> usize {
    engine::sample_position(t(ticks)) as usize
}

/// The lowest RMS of the consecutive windows of `size` frames over `samples`.
fn lowest_window(samples: &[f32], size: usize) -> f32 {
    samples.chunks(size * 2).map(rms).fold(f32::MAX, f32::min)
}

/// A whole file slowed down is the default case: the level must reach the first and last sample,
/// where grains read outside the file.
#[test]
fn the_level_holds_up_to_the_very_first_and_last_sample_of_the_file() {
    for speed in [0.1f64, 0.5] {
        for hz in [235.0, 440.0, 1000.0] {
            let length = 2 * 48_000;
            let duration = (2.0 * SECOND as f64 / speed).round() as i64;
            let project = project_of(vec![sped_clip(0, 0, duration, speed)]);
            let frames = timeline_samples(duration);
            let mut out = vec![0.0f32; frames * 2];
            mix_audio(&project, &mut EndingTone { hz, length, level: 0.5 }, 0, &mut out).unwrap();
            let head = lowest_window(&out[..4096 * 2], 256);
            let tail = lowest_window(&out[(frames - 4096) * 2..], 256);
            assert!(head > 0.2, "{speed}x {hz} Hz: start of the file, rms {head}");
            assert!(tail > 0.2, "{speed}x {hz} Hz: end of the file, rms {tail}");
        }
    }
}

#[test]
fn past_the_end_of_the_file_is_silent_and_a_silent_file_stays_silent() {
    let length = 48_000;
    // One source second at half speed, but a clip twice as long: its second half has no audio.
    let project = project_of(vec![sped_clip(0, 0, 4 * SECOND, 0.5)]);
    let frames = 4 * 48_000;
    let mut out = vec![0.0f32; frames * 2];
    mix_audio(&project, &mut EndingTone { hz: 440.0, length, level: 0.5 }, 0, &mut out).unwrap();
    assert!(rms(&out[(2 * 48_000 + 4096) * 2..]) == 0.0, "nothing is boosted out of the void");
    let mut silent = vec![1.0f32; frames * 2];
    mix_audio(&project, &mut EndingTone { hz: 440.0, length: 4 * 48_000, level: 0.0 }, 0, &mut silent).unwrap();
    assert!(silent.iter().all(|sample| *sample == 0.0), "digital silence is not boosted");
}

/// How far a windowed RMS falls below the steady level, in dB, at the worst key grain.
fn worst_key_dip(speed: f64, hz: f64) -> f64 {
    const GRAIN_HOP: usize = 1024;
    const KEY: usize = 64;
    let frames = 12 * 48_000;
    let project = project_of(vec![sped_clip(0, 20 * SECOND, 12 * SECOND, speed)]);
    let out = mix_tone(&project, hz, frames);
    let window = |from: usize| rms(&out[from * 2..(from + 256) * 2]);
    let mut worst = 0.0f64;
    let mut key = KEY * GRAIN_HOP;
    while key + 8192 < frames {
        let steady = (0..8).map(|i| window(key + 4096 + i * 512)).fold(0.0, f32::max);
        let lowest = (0..3 * 2048).step_by(32).map(|step| window(key - 3072 + step)).fold(f32::MAX, f32::min);
        worst = worst.max(20.0 * f64::from(steady / lowest).log10());
        key += KEY * GRAIN_HOP;
    }
    worst
}

/// Key grains are not aligned, so that a seek replays little; two grains out of phase used to
/// cancel by up to 18 dB in their overlap. The join gain is capped at 2 so that it cannot burst what
/// did not cancel, which leaves a worst measured dip of 4.7 dB here (0.1x, 3 kHz).
#[test]
fn a_key_grain_does_not_dip_the_level() {
    for speed in [0.5, 0.1, 2.0] {
        for hz in [110.0, 233.0, 440.0, 1000.0, 3000.0] {
            let dip = worst_key_dip(speed, hz);
            assert!(dip < 6.0, "{speed}x {hz} Hz: {dip:.2} dB at a key");
        }
    }
}

fn long_mix(project: &Project, hz: f64, frames: usize, block: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    let mut states = StretchStates::default();
    let mut source = Tone { hz };
    let mut start = 0i64;
    for chunk in out.chunks_mut(block * 2) {
        mix_audio_with(project, &mut source, &mut states, start, chunk).unwrap();
        start += (chunk.len() / 2) as i64;
    }
    out
}

/// A tone whose frequency sweeps, so a grain placed wrongly really differs.
struct Sweep;

impl AudioSource for Sweep {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let time = (position + i as i64) as f64 / 48_000.0;
            let sample = ((220.0 + 200.0 * time) * time * 2.0 * std::f64::consts::PI).sin() as f32 * 0.4;
            frame[0] = sample;
            // Both channels alike: a mono of zero would skip the alignment this test is about.
            frame[1] = sample * 0.5;
        }
        Ok(())
    }
}

#[test]
fn the_output_does_not_depend_on_the_block_size_or_on_seeking() {
    let project = project_of(vec![sped_clip(0, SECOND, 8 * SECOND, 1.75)]);
    let frames = 8 * 48_000;
    let mut whole = vec![0.0f32; frames * 2];
    mix_audio(&project, &mut Sweep, 0, &mut whole).expect("mix");
    for block in [37, 512, 1024, 1601] {
        let mut split = vec![0.0f32; frames * 2];
        let mut states = StretchStates::default();
        let mut start = 0i64;
        for chunk in split.chunks_mut(block * 2) {
            mix_audio_with(&project, &mut Sweep, &mut states, start, chunk).unwrap();
            start += (chunk.len() / 2) as i64;
        }
        assert_eq!(split, whole, "block {block}");
    }
    // Reading a block out of order, as a seek does, matches the sequential mix.
    let mut seek = StretchStates::default();
    for start in [300_000i64, 40_000, 200_000, 120_000, 300_000] {
        let mut block = vec![0.0f32; 2048 * 2];
        mix_audio_with(&project, &mut Sweep, &mut seek, start, &mut block).unwrap();
        assert_eq!(&block[..], &whole[start as usize * 2..(start as usize + 2048) * 2], "seek to {start}");
    }
}

/// A strong bass in both channels and a weak tone only in their difference.
struct BassAndSide;

impl AudioSource for BassAndSide {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let time = (position + i as i64) as f64 / 48_000.0;
            let bass = (80.0 * time * 2.0 * std::f64::consts::PI).sin() as f32 * 0.6;
            let side = (1300.0 * time * 2.0 * std::f64::consts::PI).sin() as f32 * 0.15;
            frame[0] = bass + side;
            frame[1] = bass - side;
        }
        Ok(())
    }
}

#[test]
fn a_key_join_on_cancelling_bass_does_not_burst_what_did_not_cancel() {
    for speed in [1.5, 3.0] {
        let project = project_of(vec![sped_clip(0, SECOND, 12 * SECOND, speed)]);
        let frames = 12 * 48_000;
        let mut out = vec![0.0f32; frames * 2];
        mix_audio(&project, &mut BassAndSide, 0, &mut out).expect("mix");
        let peak = out[8192..(frames - 4096) * 2].chunks_exact(2).map(|frame| ((frame[0] - frame[1]) * 0.5).abs()).fold(0.0f32, f32::max);
        assert!(peak <= 0.15 * 2.6, "the side tone of 0.15 reaches {peak} at {speed}x");
    }
}

#[test]
fn speed_one_leaves_the_mix_untouched() {
    let plain = project_of(vec![sped_clip(0, 500, SECOND, 1.0)]);
    let segments = audio_segments(&plain, 0, 48_000);
    assert!(segments.iter().all(|segment| segment.stretch.is_none()), "no stretcher at 1x");
    let out = long_mix(&plain, 440.0, 48_000, 1024);
    let mut direct = vec![0.0f32; 48_000 * 2];
    Tone { hz: 440.0 }.read(key(0), "a", engine::sample_position(t(500)), &mut direct).unwrap();
    assert_eq!(out, direct, "consecutive source samples");
}

#[test]
fn the_sound_of_a_clip_is_gated_by_its_timeline_range() {
    let project = project_of(vec![sped_clip(0, 0, SECOND, 2.0)]);
    let out = mix_tone(&project, 440.0, 2 * 48_000);
    assert!(rms(&out[48_100 * 2..]) < 1e-6, "silent after the clip");
    assert!(rms(&out[..47_000 * 2]) > 0.1, "sound inside it");
}

#[test]
fn neighbouring_clips_at_different_speeds_stretch_independently() {
    let project = project_of(vec![sped_clip(0, 0, 2 * SECOND, 2.0), sped_clip(2 * SECOND, 0, 2 * SECOND, 0.5)]);
    let out = long_mix(&project, 440.0, 4 * 48_000, 1024);
    for side in [&out[2 * 12_000..2 * 84_000], &out[2 * 108_000..2 * 180_000]] {
        let hz = measured_hz(side);
        assert!((hz - 440.0).abs() < 2.5, "{hz}");
        let level = rms(side);
        assert!((0.26..0.40).contains(&level), "{level}");
    }
}

#[test]
fn a_transition_stretches_both_clips_through_its_window() {
    let two = project_of(vec![sped_clip(0, 0, 4 * SECOND, 2.0), sped_clip(4 * SECOND, 0, 4 * SECOND, 2.0)]);
    let joined = add_transition(&two, 0, Transition { after_clip: 0, package: "t".into(), duration: t(SECOND), params: Default::default() }).unwrap();
    let segments = audio_segments(&joined, 4 * 48_000 - 100, 200);
    assert_eq!(segments.len(), 2, "both clips sound in the window");
    assert!(segments.iter().all(|s| s.stretch.is_some()));
    // Equal-power crossfade of the same tone: level stays close to the tone itself.
    let out = long_mix(&joined, 440.0, 8 * 48_000, 1024);
    let window = &out[2 * (4 * 48_000 - 12_000)..2 * (4 * 48_000 + 12_000)];
    assert!(rms(window) > 0.2, "{}", rms(window));
}
