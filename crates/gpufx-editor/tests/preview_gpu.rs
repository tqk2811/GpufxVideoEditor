//! The preview producer on a real GPU and real decoder. Skips on a machine without a hardware
//! adapter.

#![cfg(windows)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use d3d12_backend::{is_cancelled, Gpu, ProjectPaths, SequentialDecoder};
use engine::render::Decoder;
use engine::{Clip, Output, Project, Time, Track, TrackKind, SUPPORTED_VERSION};
use gpufx_editor::preview::backend::{D3d12Producer, PreviewRequest};
use gpufx_editor::preview::worker::{FrameProducer, ProduceError};
use gpufx_editor::project_file::{package_roots, project_paths};

fn clip(name: &str, duration: Time) -> Clip {
    let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-clips")).join(name);
    let source = std::path::absolute(source).expect("absolute path").to_string_lossy().into_owned();
    Clip { source, start: Time::ZERO, source_in: Time::ZERO, duration, effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO }
}

fn project(clips: Vec<Clip>) -> Arc<Project> {
    Arc::new(Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 320, height: 180, frame_duration: Time::from_ticks(3000), audio: None },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new(), locked: false, hidden: false }],
    })
}

/// Reports why a test cannot run. Fails instead when the environment says the GPU must be there.
fn skip(reason: &str) {
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {reason}");
    eprintln!("preview_gpu: skipped: {reason}");
}

/// The hardware device, or `None` after reporting a skip.
fn hardware_gpu() -> Option<Arc<Gpu>> {
    let gpu = Gpu::create_hardware().expect("device");
    if gpu.is_none() {
        skip("no hardware adapter");
    }
    if !Path::new(&clip("clip-a.mp4", Time::ZERO).source).is_file() {
        skip("test-clips is missing (scripts/setup-poc.ps1 makes it)");
        return None;
    }
    gpu.map(Arc::new)
}

fn request(project: &Arc<Project>, frame: i64) -> PreviewRequest {
    static PATHS: std::sync::OnceLock<Arc<ProjectPaths>> = std::sync::OnceLock::new();
    let paths = PATHS.get_or_init(|| Arc::new(project_paths(None, &package_roots()))).clone();
    PreviewRequest { revision: 0, project: project.clone(), paths, frame }
}

/// One test at a time on the GPU.
fn gpu_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// 30 fps, as every clip of test-clips: the time of `frame` in the source.
fn at_frame(frame: i64) -> Time {
    Time::from_ticks(frame * 3000)
}

/// Frames in each of the test clips.
const FRAMES: i64 = 600;

/// The timestamp of each frame, from decoding forward without a jump.
fn sequential_pts(source: &Path, gpu: &Gpu) -> Vec<i64> {
    let mut decoder = SequentialDecoder::open(source, gpu.raw_device()).expect("open the clip");
    (0..FRAMES).map(|frame| decoder.frame_at(at_frame(frame)).expect("frame").pts).collect()
}

fn source_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(clip(name, Time::ZERO).source)
}

#[test]
fn a_decoder_gives_the_same_frames_after_backward_and_far_forward_jumps() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    // gop250-a has keyframes 8.3 s apart, more than the forward seek threshold.
    for name in ["clip-a.mp4", "gop250-a.mp4"] {
        let source = source_path(name);
        let expected = sequential_pts(&source, &gpu);
        let mut decoder = SequentialDecoder::open(&source, gpu.raw_device()).expect("open the clip");
        for frame in [0, 10, 300, 120, 5, 400, 405, 340, 599, 200, 280, FRAMES + 5000, 0, 590] {
            let pts = decoder.frame_at(at_frame(frame)).expect("frame").pts;
            let wanted = expected[frame.min(FRAMES - 1) as usize];
            assert_eq!(pts, wanted, "{name}: frame {frame} after the jumps before it");
        }
    }
}

#[test]
fn a_cancelled_decode_leaves_the_decoder_usable() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let source = source_path("gop250-a.mp4");
    let expected = sequential_pts(&source, &gpu);
    let mut decoder = SequentialDecoder::open(&source, gpu.raw_device()).expect("open the clip");
    let abort = Arc::new(AtomicBool::new(false));
    decoder.set_abort(abort.clone(), Duration::ZERO);

    let frame = |decoder: &mut SequentialDecoder, frame: i64| decoder.frame_at(at_frame(frame)).map(|f| f.pts);
    assert_eq!(frame(&mut decoder, 30).expect("frame 30"), expected[30]);

    abort.store(true, Ordering::SeqCst);
    let error = frame(&mut decoder, 400).expect_err("the flag stops the decode");
    assert!(is_cancelled(&error.0), "{}", error.0);
    // The flag has cleared again: a backward request after the cancelled forward jump.
    abort.store(false, Ordering::SeqCst);
    assert_eq!(frame(&mut decoder, 5).expect("frame 5"), expected[5]);
    assert_eq!(frame(&mut decoder, 400).expect("frame 400"), expected[400]);
}

#[test]
fn a_producer_shows_the_right_picture_after_scrubbing_and_jumping() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let seconds = Time::from_seconds(15).unwrap();
    let project = project(vec![clip("gop250-a.mp4", seconds)]);

    let mut walking = D3d12Producer::new(gpu.clone());
    let mut wanted = std::collections::HashMap::new();
    for frame in 0..=320 {
        let image = walking.produce(&request(&project, frame)).expect("frame").expect("a picture");
        if matches!(frame, 200 | 320) {
            wanted.insert(frame, image.image.read_rgba(&gpu).expect("read"));
        }
        walking.recycle(image);
    }

    let mut jumping = D3d12Producer::new(gpu.clone());
    for frame in [5, 320, 200] {
        let image = jumping.produce(&request(&project, frame)).expect("frame").expect("a picture");
        if let Some(rgba) = wanted.get(&frame) {
            assert_eq!(&image.image.read_rgba(&gpu).expect("read"), rgba, "frame {frame} after jumping");
        }
        jumping.recycle(image);
    }
}

#[test]
fn frames_errors_edits_and_blanks_through_one_producer() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
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
    match producer.produce(&request(&missing, 30)) {
        Err(ProduceError::Failed(error)) => assert!(error.contains("no-such-file"), "{error}"),
        Err(ProduceError::Cancelled) => panic!("nothing cancelled this frame"),
        Ok(_) => panic!("a missing file fails"),
    }

    let frame = producer.produce(&request(&first, 30)).expect("the next request works").expect("a picture");
    assert_eq!(frame.image.read_rgba(&gpu).expect("read"), picture_a, "the same frame gives the same picture");
    producer.recycle(frame);

    assert!(producer.produce(&request(&first, 9_000)).expect("past the end is not an error").is_none());
    let empty = project(Vec::new());
    assert!(producer.produce(&request(&empty, 0)).expect("an empty timeline is not an error").is_none());
}

fn scale_effect(width: i64, height: i64) -> engine::Effect {
    let params = serde_json::json!({ "w": width, "h": height });
    engine::Effect { package: "packages/gpufx/ffmpeg/scale".to_owned(), params: params.as_object().expect("an object").clone() }
}

/// A size parameter fixes the sizes gpufx computed, so changing one must rebuild the graph and not
/// leave the preview stuck on the refusal.
#[test]
fn changing_a_size_parameter_of_a_clip_effect_keeps_the_preview_working() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let mut producer = D3d12Producer::new(gpu.clone());
    let seconds = Time::from_seconds(3).unwrap();
    let with_scale = |width, height| {
        let mut scaled = clip("clip-a.mp4", seconds);
        scaled.effects.push(scale_effect(width, height));
        project(vec![scaled])
    };
    // The rule gpufx applies changes while the size written stays that of the project image.
    for (width, height) in [(320, 180), (-1, 180), (320, -1), (320, 180)] {
        let frame = producer.produce(&request(&with_scale(width, height), 30)).unwrap_or_else(|e| panic!("{width}x{height}: {}", describe(&e))).expect("a picture");
        producer.recycle(frame);
    }
}

/// A size parameter changed in the same update as a later effect's bad value must not leave the
/// graph refusing every following frame.
#[test]
fn a_failed_update_after_a_size_change_does_not_leave_the_preview_stuck() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let mut producer = D3d12Producer::new(gpu.clone());
    let seconds = Time::from_seconds(3).unwrap();
    let with_effects = |first: engine::Effect, second: engine::Effect| {
        let mut scaled = clip("clip-a.mp4", seconds);
        scaled.effects.extend([first, second]);
        project(vec![scaled])
    };
    let good = with_effects(scale_effect(320, 180), scale_effect(320, 180));
    let frame = producer_frame(&mut producer, &good);
    producer.recycle(frame);
    let bad_params = serde_json::json!({ "w": { "not": "a number" }, "h": 180 });
    let bad = engine::Effect { package: "packages/gpufx/ffmpeg/scale".to_owned(), params: bad_params.as_object().expect("an object").clone() };
    let refused = producer.produce(&request(&with_effects(scale_effect(-1, 180), bad), 30));
    assert!(refused.is_err(), "a value that is not a number is an error");
    let fixed = with_effects(scale_effect(-1, 180), scale_effect(320, 180));
    let frame = producer_frame(&mut producer, &fixed);
    producer.recycle(frame);
}

fn producer_frame(producer: &mut D3d12Producer, timeline: &Arc<engine::Project>) -> gpufx_editor::preview::backend::PreviewImage {
    producer.produce(&request(timeline, 30)).unwrap_or_else(|e| panic!("{}", describe(&e))).expect("a picture")
}

/// A transition number changing between frames is updated in place and still shows a picture.
#[test]
fn changing_a_transition_number_keeps_the_preview_working() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let mut producer = D3d12Producer::new(gpu.clone());
    let seconds = Time::from_seconds(3).unwrap();
    let with_size = |max_size: f64| {
        let mut second = clip("clip-b.mp4", seconds);
        second.start = seconds;
        let mut timeline = (*project(vec![clip("clip-a.mp4", seconds), second])).clone();
        let params = serde_json::json!({ "max_size": max_size });
        timeline.tracks[0].transitions.push(engine::Transition {
            after_clip: 0,
            package: "packages/gpufx/ffmpeg/pixelize-transition".to_owned(),
            duration: Time::from_seconds(1).unwrap(),
            params: params.as_object().expect("an object").clone(),
        });
        Arc::new(timeline)
    };
    let frame_inside = seconds.ticks() / 3000;
    for max_size in [8.0, 32.0, 8.0] {
        let frame = producer.produce(&request(&with_size(max_size), frame_inside)).unwrap_or_else(|e| panic!("max_size {max_size}: {}", describe(&e))).expect("a picture");
        producer.recycle(frame);
    }
}

fn describe(error: &ProduceError) -> String {
    match error {
        ProduceError::Failed(message) => message.clone(),
        ProduceError::Cancelled => "cancelled".to_owned(),
    }
}

#[test]
fn a_relative_source_follows_the_paths_of_the_request() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let mut relative = clip("clip-a.mp4", Time::from_seconds(1).unwrap());
    relative.source = "clip-a.mp4".to_owned();
    let project = project(vec![relative]);
    let clips = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-clips"));
    let roots = package_roots();
    let mut producer = D3d12Producer::new(gpu);

    let beside = Arc::new(project_paths(Some(&clips.join("project.json")), &roots));
    let found = PreviewRequest { revision: 0, project: project.clone(), paths: beside, frame: 3 };
    producer.produce(&found).expect("found next to the project").expect("a picture");

    let elsewhere = Arc::new(project_paths(Some(Path::new(r"C:\no-such-folder\project.json")), &roots));
    let missing = PreviewRequest { revision: 1, project, paths: elsewhere, frame: 3 };
    assert!(matches!(producer.produce(&missing), Err(ProduceError::Failed(_))), "the old decoder must not serve the moved project");
}

/// Covers `Gpu::try_lock_submits` only: a held gate is refused and a free one is taken. It does not
/// run the presenter, so the skipped frame of `Presenter::render` is not tested here.
#[test]
fn try_lock_submits_refuses_a_held_gate_and_takes_a_free_one() {
    let _exclusive = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let held = gpu.lock_submits();
    let refused = std::thread::scope(|scope| scope.spawn(|| gpu.try_lock_submits().is_none()).join().expect("the thread ends"));
    assert!(refused, "a held gate must be refused");
    drop(held);
    assert!(gpu.try_lock_submits().is_some(), "a free gate must be taken");
}

/// The first frame on a device builds the pipelines of GPU frames, about a second. The UI must be
/// able to present meanwhile, so the submit gate is only taken for short moments. Whether the fences
/// stay safe under the gate is tested in gpufx-core, not here.
#[test]
fn the_first_frame_on_a_device_leaves_the_submit_gate_free_while_it_builds_pipelines() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let project = project(vec![clip("clip-a.mp4", Time::from_seconds(2).unwrap())]);
    let stop = AtomicBool::new(false);
    let longest = std::thread::scope(|scope| {
        // Submits as a frame of the UI does, between short sleeps. Each measure is one wait for
        // one release of the gate, so separate short holds never add up.
        let ui = scope.spawn(|| {
            let mut longest = Duration::ZERO;
            while !stop.load(Ordering::SeqCst) {
                let waiting = std::time::Instant::now();
                let gate = gpu.lock_submits();
                longest = longest.max(waiting.elapsed());
                gpu.queue.submit([]);
                drop(gate);
                std::thread::sleep(Duration::from_millis(1));
            }
            longest
        });
        let mut producer = D3d12Producer::new(gpu.clone());
        producer.produce(&request(&project, 0)).expect("frame").expect("a picture");
        stop.store(true, Ordering::SeqCst);
        ui.join().expect("the thread ends")
    });
    assert!(longest < Duration::from_millis(300), "a frame of the UI waited {longest:?} for the submit gate");
}
