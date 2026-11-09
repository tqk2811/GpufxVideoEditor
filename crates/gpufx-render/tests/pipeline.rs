//! Renders in-process on the real GPU and decoder and checks the RGBA of the output frames.
//! Skips, with a message, when no Direct3D 12 hardware adapter is available.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::*;
use d3d12_backend::{AudioFiles, D3d12Compositor, Gpu, ProjectPaths, SequentialDecoder};
use engine::render::{render, render_with_audio, Decoder, FrameSink, RenderError};
use engine::{validate, Project, Time};
use sha2::{Digest, Sha256};

/// `None` (test skipped) only when there is no hardware D3D12 adapter: WARP cannot decode with
/// D3D12VA. Any other failure to open the GPU fails the test.
fn gpu() -> Option<Arc<Gpu>> {
    match Gpu::create_hardware() {
        Ok(Some(gpu)) => Some(Arc::new(gpu)),
        Ok(None) => {
            eprintln!("skipping: no D3D12 hardware adapter is available");
            None
        }
        Err(message) => panic!("opening the GPU failed: {message}"),
    }
}

/// Keeps RGBA of the frames in `wanted` and the hash of every frame.
struct Capture {
    wanted: Vec<i64>,
    frames: Vec<(i64, Vec<u8>)>,
    hashes: Vec<String>,
    audio: Sha256,
    audio_samples: usize,
    pcm: Vec<f32>,
}

impl Capture {
    fn new(wanted: &[i64]) -> Self {
        Self { wanted: wanted.to_vec(), frames: Vec::new(), hashes: Vec::new(), audio: Sha256::new(), audio_samples: 0, pcm: Vec::new() }
    }

    fn frame(&self, index: i64) -> &[u8] {
        &self.frames.iter().find(|(i, _)| *i == index).unwrap_or_else(|| panic!("frame {index} was not captured")).1
    }
}

impl FrameSink for Capture {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        self.hashes.push(Sha256::digest(rgba).iter().map(|b| format!("{b:02x}")).collect());
        if self.wanted.contains(&index) {
            self.frames.push((index, rgba.to_vec()));
        }
        Ok(())
    }

    fn write_audio(&mut self, samples: &[f32]) -> Result<(), RenderError> {
        for sample in samples {
            self.audio.update(sample.to_le_bytes());
        }
        self.audio_samples += samples.len();
        self.pcm.extend_from_slice(samples);
        Ok(())
    }
}

/// Validates and renders `json`; fails the test on a validation error.
fn render_project(gpu: &Arc<Gpu>, json: &str, capture: &mut Capture) -> Result<(), RenderError> {
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).map_err(RenderError)?;
    render(&project, &mut compositor, capture).map(|_| ())
}

fn clip_json(source: &Path, start: i64, source_in: i64, duration: i64) -> String {
    format!(r#"{{"source":"{}","start":{},"source_in":{},"duration":{},"effects":[]}}"#, json_path(source), start * FRAME, source_in * FRAME, duration * FRAME)
}

fn project_json(width: u32, height: u32, tracks: &str) -> String {
    format!(r#"{{"version":1,"output":{{"width":{width},"height":{height},"frame_duration":{FRAME}}},"tracks":[{tracks}]}}"#)
}

const W: u32 = CLIP_WIDTH;
const H: u32 = CLIP_HEIGHT;

/// Renders `json` with its audio through the real decoder; returns the mixed samples and their hash.
fn render_pcm(gpu: &Arc<Gpu>, json: &str) -> (usize, String) {
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut files = AudioFiles::new(paths.clone());
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).expect("compositor");
    let mut capture = Capture::new(&[]);
    render_with_audio(&project, &mut compositor, &mut capture, Some(&mut files)).expect("render");
    let digest: String = capture.audio.finalize().iter().map(|b| format!("{b:02x}")).collect();
    (capture.audio_samples, digest)
}

#[test]
fn the_mixed_pcm_repeats_exactly_between_renders() {
    let _lock = gpu_lock();
    let clip = tone_clip_with_video();
    let Some(gpu) = gpu() else { return };
    let source = json_path(&clip);
    let json = format!(
        r#"{{"version":2,"output":{{"width":{W},"height":{H},"frame_duration":{FRAME},"audio":{{"sample_rate":48000,"channels":2,"bitrate":128000}}}},"tracks":[
            {{"clips":[{{"source":"{source}","start":0,"source_in":90000,"duration":120000,"fade_in":9000}},
                       {{"source":"{source}","start":120000,"source_in":270000,"duration":120000,"gain_db":-3.0}}],
              "transitions":[{{"after_clip":0,"package":"mix","duration":30000}}]}}]}}"#
    );
    let first = render_pcm(&gpu, &json);
    let second = render_pcm(&gpu, &json);
    // 240000 ticks is 128000 stereo samples.
    assert_eq!(first.0, 128_000 * 2, "the mix covers the timeline exactly");
    assert_eq!(first, second, "the PCM before the encoder differs between two renders");
}

#[test]
fn rendering_twice_gives_identical_frames() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    let transition = r#""transitions":[{"after_clip":0,"package":"mix","duration":30000}]"#;
    let tracks = format!(
        r#"{{"clips":[{},{}],{transition}}},{{"clips":[{}]}}"#,
        clip_json(&a, 0, 10, 40),
        clip_json(&b, 40, 0, 40),
        clip_json(&b, 70, 20, 20)
    );
    let json = project_json(W, H, &tracks);
    let mut first = Capture::new(&[]);
    render_project(&gpu, &json, &mut first).expect("first render");
    let mut second = Capture::new(&[]);
    render_project(&gpu, &json, &mut second).expect("second render");
    assert_eq!(first.hashes.len(), 90);
    assert_eq!(first.hashes, second.hashes, "the two renders differ");
    assert_ne!(first.hashes[0], first.hashes[1], "frames should differ from each other");
    assert_ne!(first.hashes[0], first.hashes[80], "frames should differ from each other");
}

#[test]
fn trims_clips_exactly_and_cuts_between_them() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{},{}]}}"#, clip_json(&a, 0, 30, 30), clip_json(&b, 30, 100, 30));
    let mut capture = Capture::new(&[0, 29, 30, 59]);
    render_project(&gpu, &project_json(W, H, &tracks), &mut capture).expect("render");
    assert_eq!(capture.hashes.len(), 60);
    assert_eq!(read_frame_number(capture.frame(0), W), 30, "first frame of the trimmed clip");
    assert_eq!(read_frame_number(capture.frame(29), W), 59, "last frame of the trimmed clip");
    assert_eq!(read_frame_number(capture.frame(30), W), 100, "first frame after the cut");
    assert_eq!(read_frame_number(capture.frame(59), W), 129, "last frame");
    assert!((sample(capture.frame(29), W, W / 2, H / 2) - GREY_A).abs() <= 4);
    assert!((sample(capture.frame(30), W, W / 2, H / 2) - GREY_B).abs() <= 4);
}

/// A version 2 project: version 1 files are frozen and have no `speed`.
fn speed_project_json(tracks: &str) -> String {
    project_json(W, H, tracks).replace(r#""version":1"#, r#""version":2"#)
}

/// A clip at `speed`, in frames of the output.
fn sped_clip_json(source: &Path, start: i64, source_in: i64, duration: i64, speed: f64) -> String {
    format!(r#"{{"source":"{}","start":{},"source_in":{},"duration":{},"speed":{speed}}}"#, json_path(source), start * FRAME, source_in * FRAME, duration * FRAME)
}

#[test]
fn double_speed_shows_every_second_source_frame_and_keeps_the_timeline_length() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}}"#, sped_clip_json(&a, 0, 10, 40, 2.0));
    let wanted = [0, 1, 7, 39];
    let mut capture = Capture::new(&wanted);
    render_project(&gpu, &speed_project_json(&tracks), &mut capture).expect("render");
    assert_eq!(capture.hashes.len(), 40, "the clip lasts its timeline length");
    for frame in wanted {
        assert_eq!(read_frame_number(capture.frame(frame), W), 10 + 2 * frame, "output frame {frame}");
    }
}

#[test]
fn half_speed_repeats_source_frames() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}}"#, sped_clip_json(&a, 0, 10, 40, 0.5));
    let wanted = [0, 1, 2, 3, 39];
    let mut capture = Capture::new(&wanted);
    render_project(&gpu, &speed_project_json(&tracks), &mut capture).expect("render");
    assert_eq!(capture.hashes.len(), 40);
    for (frame, expected) in [(0, 10), (1, 10), (2, 11), (3, 11), (39, 29)] {
        assert_eq!(read_frame_number(capture.frame(frame), W), expected, "output frame {frame}");
    }
    assert_eq!(capture.hashes[0], capture.hashes[1], "a repeated frame is the same picture");
}

#[test]
fn a_speed_of_one_renders_exactly_as_a_project_without_the_field() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let plain = format!(r#"{{"clips":[{}]}}"#, clip_json(&a, 0, 5, 30));
    let explicit = format!(r#"{{"clips":[{}]}}"#, sped_clip_json(&a, 0, 5, 30, 1.0));
    let mut first = Capture::new(&[]);
    render_project(&gpu, &speed_project_json(&plain), &mut first).expect("render");
    let mut second = Capture::new(&[]);
    render_project(&gpu, &speed_project_json(&explicit), &mut second).expect("render");
    assert_eq!(first.hashes, second.hashes);

    let tone = tone_clip_with_video();
    let source = json_path(&tone);
    let audio = |speed: &str| format!(r#"{{"version":2,"output":{{"width":{W},"height":{H},"frame_duration":{FRAME},"audio":{{"sample_rate":48000,"channels":2,"bitrate":128000}}}},"tracks":[{{"clips":[{{"source":"{source}","start":0,"source_in":90000,"duration":90000{speed}}}]}}]}}"#);
    assert_eq!(render_pcm(&gpu, &audio("")), render_pcm(&gpu, &audio(r#","speed":1.0"#)), "the audio of 1x does not go through the stretcher");
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

/// The exported samples of one clip of the stepped tone clip at `speed`.
fn exported_pcm(gpu: &Arc<Gpu>, source_in: i64, duration: i64, speed: f64) -> Vec<f32> {
    let clip = tone_clip_with_video();
    let source = json_path(&clip);
    let json = format!(r#"{{"version":2,"output":{{"width":{W},"height":{H},"frame_duration":{FRAME},"audio":{{"sample_rate":48000,"channels":2,"bitrate":128000}}}},"tracks":[{{"clips":[{{"source":"{source}","start":0,"source_in":{source_in},"duration":{duration},"speed":{speed}}}]}}]}}"#);
    let project = Project::from_json(&json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    assert!(validate(&project, paths.as_ref()).is_empty());
    let mut files = AudioFiles::new(paths.clone());
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).expect("compositor");
    let mut capture = Capture::new(&[]);
    render_with_audio(&project, &mut compositor, &mut capture, Some(&mut files)).expect("render");
    capture.pcm
}

#[test]
fn an_exported_tone_keeps_its_frequency_and_the_timeline_length_at_2x_and_half() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    // Source second 1 holds 500 Hz; at 2x half a timeline second plays it.
    let fast = exported_pcm(&gpu, 90_000, 45_000, 2.0);
    assert_eq!(fast.len(), 24_000 * 2, "half a second of the timeline");
    let hz = measured_hz(&fast[2 * 2_400..fast.len() - 2 * 2_400]);
    assert!((hz - 500.0).abs() < 4.0, "2x: {hz}");

    // Source second 2 holds 700 Hz; at 0.5x two timeline seconds play it.
    let slow = exported_pcm(&gpu, 180_000, 180_000, 0.5);
    assert_eq!(slow.len(), 96_000 * 2, "two seconds of the timeline");
    let hz = measured_hz(&slow[2 * 4_800..slow.len() - 2 * 4_800]);
    assert!((hz - 700.0).abs() < 5.0, "0.5x: {hz}");
}

/// `json` rendered by a compositor that already holds the graphs from `first`.
fn render_after(gpu: &Arc<Gpu>, first: &str, json: &str, frames: &[i64]) -> Capture {
    let earlier = Project::from_json(first).expect("project parses");
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).expect("compositor");
    render(&earlier, &mut compositor, &mut Capture::new(&[])).expect("first render");
    let mut capture = Capture::new(frames);
    render(&project, &mut compositor, &mut capture).expect("second render");
    capture
}

#[test]
fn a_changed_effect_parameter_reaches_a_graph_already_built() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = |brightness: f64| format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{},"effects":[{{"package":"packages/gpufx/ffmpeg/color-adjust","params":{{"brightness":{brightness}}}}}]}}]}}"#, json_path(&a), 3 * FRAME);
    let (dark, bright) = (project_json(W, H, &tracks(-0.2)), project_json(W, H, &tracks(0.3)));
    let mut direct = Capture::new(&[1]);
    render_project(&gpu, &bright, &mut direct).expect("render");
    let reused = render_after(&gpu, &dark, &bright, &[1]);
    assert_eq!(reused.hashes, direct.hashes, "a built graph must show the new value");
    let mut unchanged = Capture::new(&[1]);
    render_project(&gpu, &dark, &mut unchanged).expect("render");
    assert_ne!(unchanged.hashes, direct.hashes, "the parameter must change the picture");
}

#[test]
fn holds_the_last_frame_when_the_source_runs_out() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    // The source has 180 frames; this clip asks for frames 175 to 194.
    let tracks = format!(r#"{{"clips":[{}]}}"#, clip_json(&a, 0, 175, 20));
    let mut capture = Capture::new(&[4, 19]);
    render_project(&gpu, &project_json(W, H, &tracks), &mut capture).expect("render");
    assert_eq!(read_frame_number(capture.frame(4), W), 179);
    assert_eq!(read_frame_number(capture.frame(19), W), 179, "held after the source ended");
}

#[test]
fn transition_frames_follow_the_schedule() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    // Clips cut at frame 40; the transition lasts 20 frames, so frames 30..50 blend.
    let transition = format!(r#""transitions":[{{"after_clip":0,"package":"mix","duration":{}}}]"#, 20 * FRAME);
    let tracks = format!(r#"{{"clips":[{},{}],{transition}}}"#, clip_json(&a, 0, 100, 40), clip_json(&b, 40, 10, 40));
    let window: Vec<i64> = (30..50).collect();
    let mut wanted = window.clone();
    wanted.extend([0, 29, 50, 79]);
    let mut capture = Capture::new(&wanted);
    render_project(&gpu, &project_json(W, H, &tracks), &mut capture).expect("render");
    assert_eq!(read_frame_number(capture.frame(29), W), 129, "last frame before the window");
    assert_eq!(read_frame_number(capture.frame(50), W), 20, "first frame after the window");
    assert_eq!(read_frame_number(capture.frame(79), W), 49, "last frame");
    let cell_value = |number: i64, cell: u32| -> i32 {
        if (number >> (15 - cell)) & 1 == 1 {
            255
        } else {
            0
        }
    };
    for &frame in &window {
        let progress = (frame - 30) as f64 / 20.0;
        let from_number = 100 + frame;
        // Clip B runs before its in point inside the window, by the scheduler's formula.
        let to_number = 10 + (frame - 40);
        let rgba = capture.frame(frame);
        for cell in 0..16u32 {
            let expected = f64::from(cell_value(from_number, cell)) * (1.0 - progress) + f64::from(cell_value(to_number, cell)) * progress;
            let actual = f64::from(sample(rgba, W, cell * CELL + CELL / 2, 12));
            assert!((actual - expected).abs() <= 16.0, "frame {frame} cell {cell}: {actual} vs {expected} at progress {progress}");
        }
        let grey = f64::from(GREY_A) * (1.0 - progress) + f64::from(GREY_B) * progress;
        let actual = f64::from(sample(rgba, W, W / 2, H / 2));
        assert!((actual - grey).abs() <= 4.0, "frame {frame} background: {actual} vs {grey}");
    }
}

#[test]
fn top_track_covers_the_bottom_one() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}},{{"clips":[{}]}}"#, clip_json(&a, 0, 0, 30), clip_json(&b, 10, 50, 10));
    let mut capture = Capture::new(&[5, 15, 25]);
    render_project(&gpu, &project_json(W, H, &tracks), &mut capture).expect("render");
    assert_eq!(read_frame_number(capture.frame(5), W), 5, "only the bottom track");
    assert_eq!(read_frame_number(capture.frame(15), W), 55, "the top track, opaque, wins");
    assert!((sample(capture.frame(15), W, W / 2, H / 2) - GREY_B).abs() <= 4);
    assert_eq!(read_frame_number(capture.frame(25), W), 25, "the bottom track again");
}

/// A clip placed by `placement` (a JSON object), needing a version 2 project.
fn placed_clip_json(source: &Path, start: i64, source_in: i64, duration: i64, placement: &str) -> String {
    format!(r#"{{"source":"{}","start":{},"source_in":{},"duration":{},"effects":[],"placement":{}}}"#, json_path(source), start * FRAME, source_in * FRAME, duration * FRAME, placement)
}

fn project_json_v2(width: u32, height: u32, tracks: &str) -> String {
    project_json(width, height, tracks).replacen(r#""version":1"#, r#""version":2"#, 1)
}

const RIGHT_TOP_QUARTER: &str = r#"{"x":0.5,"y":0,"width":0.5}"#;

#[test]
fn a_placed_clip_fills_only_its_rectangle_and_the_rest_is_black() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}}"#, placed_clip_json(&a, 0, 0, 3, RIGHT_TOP_QUARTER));
    let mut capture = Capture::new(&[1]);
    render_project(&gpu, &project_json_v2(W, H, &tracks), &mut capture).expect("render");
    let frame = capture.frame(1);
    for (x, y) in [(40, 40), (160, 90), (160, 300), (300, 340)] {
        assert!(sample(frame, W, x, y) <= 4, "({x}, {y}) is outside the rectangle: {}", sample(frame, W, x, y));
    }
    for (x, y) in [(400, 60), (480, 90), (600, 150)] {
        assert!((sample(frame, W, x, y) - GREY_A).abs() <= 6, "({x}, {y}) is inside the rectangle: {}", sample(frame, W, x, y));
    }
    assert!(sample(frame, W, 480, 270) <= 4, "below the rectangle is black");
}

#[test]
fn a_lower_track_shows_through_where_a_placed_clip_does_not_cover() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}},{{"clips":[{}]}}"#, clip_json(&a, 0, 0, 3), placed_clip_json(&b, 0, 0, 3, RIGHT_TOP_QUARTER));
    let mut capture = Capture::new(&[1]);
    render_project(&gpu, &project_json_v2(W, H, &tracks), &mut capture).expect("render");
    let frame = capture.frame(1);
    for (x, y) in [(40, 100), (160, 300), (480, 270), (600, 340)] {
        assert!((sample(frame, W, x, y) - GREY_A).abs() <= 6, "({x}, {y}) shows the lower track: {}", sample(frame, W, x, y));
    }
    for (x, y) in [(400, 60), (480, 90), (600, 150)] {
        assert!((sample(frame, W, x, y) - GREY_B).abs() <= 6, "({x}, {y}) shows the placed clip: {}", sample(frame, W, x, y));
    }
}

#[test]
fn a_clip_of_another_shape_is_letterboxed_in_the_frame() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}}"#, clip_json(&a, 0, 0, 3));
    let mut capture = Capture::new(&[1]);
    render_project(&gpu, &project_json(W, W, &tracks), &mut capture).expect("render");
    let frame = capture.frame(1);
    for (x, y) in [(320, 320), (100, 200), (560, 440)] {
        assert!((sample(frame, W, x, y) - GREY_A).abs() <= 6, "({x}, {y}) is in the picture: {}", sample(frame, W, x, y));
    }
    for (x, y) in [(320, 40), (50, 100), (600, 600), (320, 560)] {
        assert!(sample(frame, W, x, y) <= 4, "({x}, {y}) is in a bar: {}", sample(frame, W, x, y));
    }
}

#[test]
fn clips_are_resized_to_the_project_size() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}}"#, clip_json(&a, 0, 7, 3));
    let mut capture = Capture::new(&[0, 2]);
    render_project(&gpu, &project_json(W * 2, H * 2, &tracks), &mut capture).expect("render");
    assert_eq!(read_frame_number(capture.frame(0), W * 2), 7);
    assert_eq!(read_frame_number(capture.frame(2), W * 2), 9);
    assert_eq!(capture.frame(0).len(), (W * 2 * H * 2 * 4) as usize, "the output has the project size");
}

#[test]
fn clip_effects_are_applied() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let source = json_path(&a);
    let plain = format!(r#"{{"clips":[{{"source":"{source}","start":0,"source_in":0,"duration":{FRAME},"effects":[]}}]}}"#);
    let with_effect = format!(r#"{{"clips":[{{"source":"{source}","start":0,"source_in":0,"duration":{FRAME},"effects":[{{"package":"invert","params":{{}}}}]}}]}}"#);
    let mut base = Capture::new(&[0]);
    render_project(&gpu, &project_json(W, H, &plain), &mut base).expect("plain render");
    let mut inverted = Capture::new(&[0]);
    render_project(&gpu, &project_json(W, H, &with_effect), &mut inverted).expect("effect render");
    let before = sample(base.frame(0), W, W / 2, H / 2);
    let after = sample(inverted.frame(0), W, W / 2, H / 2);
    assert!((after - (255 - before)).abs() <= 6, "invert gave {after} from {before}");
}

#[test]
fn streams_with_a_crop_offset_are_rejected() {
    let _lock = gpu_lock();
    let clip = cropped_clip();
    let Some(gpu) = gpu() else { return };
    let tracks = format!(r#"{{"clips":[{}]}}"#, clip_json(&clip, 0, 0, 3));
    let mut capture = Capture::new(&[]);
    let error = render_project(&gpu, &project_json(W, H, &tracks), &mut capture).expect_err("the crop offset must be rejected");
    assert!(error.0.contains("crop offset"), "{}", error.0);
    assert!(error.0.contains("cropped-clip-a"), "the message names the file: {}", error.0);
}

#[test]
fn the_decoder_seeks_only_to_go_backward() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let mut decoder = SequentialDecoder::open(&a, gpu.raw_device()).expect("open the clip");
    let at = |n: i64| Time::from_ticks(n * FRAME);
    let zero = decoder.frame_at(at(0)).expect("frame 0").pts;
    let ten = decoder.frame_at(at(10)).expect("frame 10").pts;
    let forward = decoder.frame_at(at(50)).expect("frame 50").pts;
    let back = decoder.frame_at(at(10)).expect("frame 10 again").pts;
    assert!(ten > zero);
    assert_eq!((forward - zero) * 10, (ten - zero) * 50, "constant frame rate");
    assert_eq!(back, ten, "the backward request returns the same frame");
    let last = decoder.frame_at(at(10_000)).expect("past the end").pts;
    assert_eq!((last - zero) / (ten - zero), CLIP_FRAMES / 10 - 1, "past the end holds the last frame: frame {}", (last - zero) * 10 / (ten - zero));
}
