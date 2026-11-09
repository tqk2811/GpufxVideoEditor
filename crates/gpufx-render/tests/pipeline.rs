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
}

impl Capture {
    fn new(wanted: &[i64]) -> Self {
        Self { wanted: wanted.to_vec(), frames: Vec::new(), hashes: Vec::new(), audio: Sha256::new(), audio_samples: 0 }
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
