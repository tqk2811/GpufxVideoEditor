//! Beat detection of real files through the FFmpeg decoder: a click track written as a WAV file.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use engine::{add_clip, add_track, ClipKey, Time, TrackKind};
use gpufx_editor::beats::{analyze_file, BeatDetector, BeatStatus};
use gpufx_editor::editor::{new_clip, EditorState};

const RATE: u32 = 48_000;

/// Writes `seconds` of stereo 16-bit clicks at `bpm` from `offset` seconds and returns the click times.
pub fn write_click_wav(path: &Path, bpm: f64, seconds: f64, offset: f64) -> Vec<f64> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut mono = vec![0.0_f64; frames];
    let mut clicks = Vec::new();
    let mut index = 0;
    loop {
        let at = offset + index as f64 * 60.0 / bpm;
        if at + 0.05 >= seconds {
            break;
        }
        clicks.push(at);
        let first = (at * f64::from(RATE)).round() as usize;
        for n in 0..(0.015 * f64::from(RATE)) as usize {
            let time = n as f64 / f64::from(RATE);
            mono[first + n] += 0.6 * (-time / 0.004).exp() * (std::f64::consts::TAU * 1500.0 * time).sin();
        }
        index += 1;
    }
    let data_len = (frames * 4) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in mono {
        let value = ((sample.clamp(-1.0, 1.0)) * 32767.0) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write the wav");
    clicks
}

fn scratch(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("gpufx-beats-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    folder.join(name)
}

#[test]
fn a_click_track_file_gives_its_tempo_and_beats() {
    let path = scratch("clicks-120.wav");
    let clicks = write_click_wav(&path, 120.0, 20.0, 0.3);
    let mut last = 0.0;
    let analysis = analyze_file(&path, &AtomicBool::new(false), &mut |progress| last = progress).expect("not cancelled").expect("decoded");
    assert!((analysis.tempo - 120.0).abs() < 1.0, "tempo {}", analysis.tempo);
    let near = clicks.iter().filter(|click| analysis.beats.iter().any(|beat| (beat - **click).abs() <= 0.02)).count();
    assert!(near as f64 >= 0.9 * clicks.len() as f64, "{near} of {}", clicks.len());
    assert!(last > 0.99, "progress reached {last}");
}

#[test]
fn the_detector_caches_per_version_of_the_file_and_can_cancel() {
    let path = scratch("clicks-100.wav");
    write_click_wav(&path, 100.0, 12.0, 0.2);
    let mut detector = BeatDetector::default();
    assert_eq!(detector.status(&path), BeatStatus::Idle);
    assert!(matches!(detector.start(&path), BeatStatus::Running(_)));
    let BeatStatus::Ready(first) = detector.wait(&path) else { panic!("finished") };
    assert!((first.tempo - 100.0).abs() < 1.0, "{}", first.tempo);
    let BeatStatus::Ready(again) = detector.start(&path) else { panic!("cached") };
    assert!(std::sync::Arc::ptr_eq(&first, &again), "no second run");
    // Another file under the same name is analysed again.
    write_click_wav(&path, 100.0, 13.0, 0.2);
    assert_eq!(detector.status(&path), BeatStatus::Idle);
    assert!(matches!(detector.start(&path), BeatStatus::Running(_)));
    detector.cancel(&path);
    assert_eq!(detector.status(&path), BeatStatus::Idle);
    assert!(!detector.running());
}

#[test]
fn a_file_without_sound_fails() {
    let missing = scratch("nothing-here.wav");
    let mut detector = BeatDetector::default();
    assert!(matches!(detector.wait(&missing), BeatStatus::Failed(_)));
}

#[test]
fn the_editor_writes_the_beats_to_the_clip_as_one_undo_step() {
    let path = scratch("clicks-140.wav");
    write_click_wav(&path, 140.0, 10.0, 0.1);
    let source = path.to_string_lossy().into_owned();
    let empty = EditorState::default().project().clone();
    let audio = add_track(&empty, TrackKind::Audio).unwrap();
    let track = audio.tracks.len() - 1;
    let project = add_clip(&audio, track, new_clip(&source, Time::ZERO, Time::from_seconds(10).unwrap())).unwrap();
    let mut state = EditorState::new(project);
    let key = ClipKey { track, clip: 0 };
    state.detect_beats(key);
    let started = Instant::now();
    while state.poll_beats() {
        assert!(started.elapsed() < Duration::from_secs(60), "detection hangs");
        std::thread::sleep(Duration::from_millis(20));
    }
    state.poll_beats();
    let beats = state.project().tracks[track].clips[0].beats.clone();
    assert!((beats.tempo - 140.0).abs() < 1.0 && beats.beats.len() > 15, "{} beats at {}", beats.beats.len(), beats.tempo);
    assert!(state.can_undo());
    state.undo();
    assert!(state.project().tracks[track].clips[0].beats.is_empty());
    // A second detection of the same file is instant, from the cache.
    state.detect_beats(key);
    assert_eq!(state.project().tracks[track].clips[0].beats, beats);
    state.clear_beats(key);
    assert!(state.project().tracks[track].clips[0].beats.is_empty());
}
