//! Measures the silence a seek and an edit cause with the real audio decoder and a simulated device.
//! Run by hand: `GPUFX_MEASURE_AUDIO=<file with audio>`, `--ignored --nocapture`. No sound is played.

#![cfg(windows)]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use d3d12_backend::{AudioFiles, ProjectPaths};
use engine::{AudioConsumer, AudioOutput, Clip, Output, Project, Time, Track, TrackKind, SUPPORTED_VERSION};
use gpufx_editor::playback::plan::frames_to_ms;
use gpufx_editor::playback::thread::{fill_device_buffer, PreparingThread};

fn track(kind: TrackKind, source: &str, seconds: i64) -> Track {
    let duration = Time::from_seconds(seconds).expect("a valid length");
    let clip = Clip { source: source.to_owned(), start: Time::ZERO, source_in: Time::ZERO, duration, effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None };
    Track { kind, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false }
}

fn project(audio: &str, seconds: i64) -> Project {
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 192_000 }) },
        tracks: vec![track(TrackKind::Video, audio, seconds), track(TrackKind::Audio, audio, seconds)],
    }
}

fn device(consumer: &mut AudioConsumer, thread: &PreparingThread, duration: Duration) {
    let mut out = vec![0.0f32; 960];
    let start = Instant::now();
    let mut next = start;
    while start.elapsed() < duration {
        fill_device_buffer(consumer, thread.shared(), &mut out, Some(Duration::ZERO));
        next += Duration::from_millis(10);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
    }
}

#[test]
#[ignore = "real time; needs GPUFX_MEASURE_AUDIO"]
fn measure_silence_with_the_real_decoder() {
    let audio = std::env::var("GPUFX_MEASURE_AUDIO").expect("GPUFX_MEASURE_AUDIO names a file with audio");
    let paths = Arc::new(ProjectPaths::new(std::path::Path::new("unsaved.json"), Vec::new()));
    let (thread, mut consumer) = PreparingThread::spawn(&project(&audio, 18), move || AudioFiles::new(paths)).expect("the thread starts");
    let silence = |thread: &PreparingThread, before: u64| frames_to_ms(thread.shared().stats().underrun - before);

    thread.play(Time::ZERO);
    let start = Instant::now();
    while thread.shared().stats().played == 0 && start.elapsed() < Duration::from_secs(5) {
        device(&mut consumer, &thread, Duration::from_millis(10));
    }
    device(&mut consumer, &thread, Duration::from_millis(500));
    eprintln!("start: {} ms", silence(&thread, 0));

    for seconds in [12, 3, 15] {
        let before = thread.shared().stats().underrun;
        thread.seek(Time::from_seconds(seconds).unwrap());
        device(&mut consumer, &thread, Duration::from_millis(500));
        eprintln!("seek to {seconds} s: {} ms of silence", silence(&thread, before));
    }
    for _ in 0..3 {
        let before = thread.shared().stats().underrun;
        thread.set_project(Arc::new(project(&audio, 18)));
        device(&mut consumer, &thread, Duration::from_millis(500));
        eprintln!("edit: {} ms of silence", silence(&thread, before));
    }
    assert_eq!(thread.take_error(), None);
}
