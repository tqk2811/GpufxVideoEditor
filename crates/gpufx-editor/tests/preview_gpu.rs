//! The preview producer on a real GPU and real decoder. Skips on a machine without a hardware
//! adapter.

#![cfg(windows)]

use std::path::Path;
use std::sync::Arc;

use d3d12_backend::Gpu;
use engine::{Clip, Output, Project, Time, Track, TrackKind, SUPPORTED_VERSION};
use gpufx_editor::preview::backend::{D3d12Producer, PreviewRequest};
use gpufx_editor::preview::worker::FrameProducer;

fn clip(name: &str, duration: Time) -> Clip {
    let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-clips")).join(name);
    let source = std::path::absolute(source).expect("absolute path").to_string_lossy().into_owned();
    Clip { source, start: Time::ZERO, source_in: Time::ZERO, duration, effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO }
}

fn project(clips: Vec<Clip>) -> Arc<Project> {
    Arc::new(Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 320, height: 180, frame_duration: Time::from_ticks(3000), audio: None },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new() }],
    })
}

fn request(project: &Arc<Project>, frame: i64) -> PreviewRequest {
    PreviewRequest { project: project.clone(), frame }
}

#[test]
fn frames_errors_edits_and_blanks_through_one_producer() {
    let Some(gpu) = Gpu::create_hardware().expect("device") else {
        eprintln!("no hardware adapter: skipped");
        return;
    };
    if !Path::new(&clip("clip-a.mp4", Time::ZERO).source).is_file() {
        eprintln!("test-clips is missing (scripts/setup-poc.ps1 makes it): skipped");
        return;
    }
    let gpu = Arc::new(gpu);
    let mut producer = D3d12Producer::new(gpu.clone());
    let seconds = Time::from_seconds(3).unwrap();

    let first = project(vec![clip("clip-a.mp4", seconds)]);
    let frame = producer.produce(&request(&first, 30)).expect("frame").expect("a picture");
    assert_eq!((frame.image.width(), frame.image.height()), (320, 180));
    let picture_a = frame.image.read_rgba(&gpu).expect("read");
    assert_eq!(picture_a.len(), 320 * 180 * 4);
    producer.recycle(frame);

    // The same clip key now names another file: the decoder must not serve the old one.
    let edited = project(vec![clip("clip-b.mp4", seconds)]);
    let frame = producer.produce(&request(&edited, 30)).expect("frame").expect("a picture");
    let picture_b = frame.image.read_rgba(&gpu).expect("read");
    assert_ne!(picture_a, picture_b, "the edited project must show the other source");
    producer.recycle(frame);

    let missing = project(vec![clip("no-such-file.mp4", seconds)]);
    let error = producer.produce(&request(&missing, 30)).err().expect("a missing file fails");
    assert!(error.contains("no-such-file"), "{error}");

    let frame = producer.produce(&request(&first, 30)).expect("the next request works").expect("a picture");
    assert_eq!(frame.image.read_rgba(&gpu).expect("read"), picture_a, "the same frame gives the same picture");
    producer.recycle(frame);

    assert!(producer.produce(&request(&first, 9_000)).expect("past the end is not an error").is_none());
    let empty = project(Vec::new());
    assert!(producer.produce(&request(&empty, 0)).expect("an empty timeline is not an error").is_none());
}
