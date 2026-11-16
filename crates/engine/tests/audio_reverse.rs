//! The sound of a reversed clip: the samples of its source range from the last to the first, with
//! noise reduction, stretch, pitch and voice applied to the reversed sound, and a range mixed after
//! a seek bit for bit what a sequential mix gives.

mod common;

use common::*;
use engine::render::{ClipKey, RenderError};
use engine::{mix_audio, mix_audio_with, sample_position, AudioOutput, AudioSource, ClipDenoise, ClipVoice, DenoiseMode, Output, Project, StretchStates, Track, TrackKind, VoiceEffect, SUPPORTED_VERSION};

const FRAME: i64 = 3000;
const SECOND: i64 = 90_000;
/// Samples in the test file: 10 seconds.
const LENGTH: i64 = 480_000;

fn project_of(clips: Vec<engine::Clip>) -> Project {
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 16, height: 16, frame_rate: engine::FrameRate::from_frame_duration(t(FRAME)).unwrap(), audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 128_000 }) },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new(), locked: false, hidden: false, subtitle: None }],
        markers: Vec::new(),
        beats: Default::default(),
    }
}

/// A file whose sample says where it is: left `position / LENGTH`, right its negative, plus a
/// tone so the stretcher and the noise reduction have something to work on.
fn value(position: i64) -> [f32; 2] {
    let ramp = position as f32 / LENGTH as f32 * 0.5;
    let tone = ((position as f64) * std::f64::consts::TAU * 220.0 / 48_000.0).sin() as f32 * 0.25;
    [ramp + tone, -ramp + tone]
}

struct File;

impl AudioSource for File {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let at = position + i as i64;
            if (0..LENGTH).contains(&at) {
                frame.copy_from_slice(&value(at));
            }
        }
        Ok(())
    }
}

/// A reversed clip of two seconds at timeline 1 s, from source 3 s.
fn reversed_clip() -> engine::Clip {
    let mut c = clip("a", SECOND, 3 * SECOND, 2 * SECOND);
    c.reverse = true;
    c
}

fn mix(project: &Project, start: i64, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * 2];
    mix_audio(project, &mut File, start, &mut out).unwrap();
    out
}

#[test]
fn a_reversed_clip_plays_its_source_samples_from_the_last_to_the_first() {
    let p = project_of(vec![reversed_clip()]);
    let clip_start = sample_position(t(SECOND));
    let (first, end) = (sample_position(t(3 * SECOND)), sample_position(t(5 * SECOND)));
    let out = mix(&p, clip_start, (end - first) as usize);
    for (i, frame) in out.chunks_exact(2).enumerate() {
        assert_eq!(frame, value(end - 1 - i as i64), "sample {i}");
    }
    // Nothing before the clip, nor after it.
    assert!(mix(&p, 0, clip_start as usize).iter().all(|s| *s == 0.0));
}

#[test]
fn a_reversed_clip_mixed_after_a_seek_matches_a_sequential_mix_with_every_effect() {
    let mut c = reversed_clip();
    c.speed = 1.5;
    c.denoise = ClipDenoise { mode: DenoiseMode::General, strength: 0.6 };
    c.voice = ClipVoice { semitones: 3.0, effect: VoiceEffect::Robot };
    c.duration = t(SECOND);
    let p = project_of(vec![c]);
    let total = sample_position(t(2 * SECOND)) as usize;
    // Sequential, in blocks of 1024 as playback does.
    let mut states = StretchStates::default();
    let mut sequential = vec![0.0; total * 2];
    for (n, block) in sequential.chunks_mut(2048).enumerate() {
        mix_audio_with(&p, &mut File, &mut states, (n * 1024) as i64, block).unwrap();
    }
    assert!(sequential.iter().any(|s| s.abs() > 0.01), "the clip sounds");
    for start in [60_000usize, 48_000 + 777, 90_000] {
        let alone = mix(&p, start as i64, 4096);
        assert_eq!(alone, sequential[start * 2..(start + 4096) * 2], "from {start}");
    }
}

#[test]
fn reversing_twice_is_the_same_sound_as_forward_reversed_in_time() {
    // Forward and reversed over the same source range are mirror images sample by sample.
    let reversed = project_of(vec![reversed_clip()]);
    let mut forward_clip = reversed_clip();
    forward_clip.reverse = false;
    let forward = project_of(vec![forward_clip]);
    let start = sample_position(t(SECOND));
    let frames = (sample_position(t(3 * SECOND)) - start) as usize;
    let (r, f) = (mix(&reversed, start, frames), mix(&forward, start, frames));
    for i in 0..frames {
        assert_eq!(r[i * 2..i * 2 + 2], f[(frames - 1 - i) * 2..(frames - i) * 2], "sample {i}");
    }
}
