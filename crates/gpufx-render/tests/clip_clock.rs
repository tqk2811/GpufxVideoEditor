//! `clip_time` and `clip_duration` through the real compositor, on the real GPU and decoder. Skips,
//! with a message, when no Direct3D 12 hardware adapter is available.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use d3d12_backend::{D3d12Compositor, Gpu, ProjectPaths};
use engine::render::{render, FrameSink, RenderError};
use engine::{validate, Project};

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

/// Keeps the RGBA of the frames in `wanted`.
struct Capture {
    wanted: Vec<i64>,
    frames: Vec<(i64, Vec<u8>)>,
}

impl Capture {
    fn new(wanted: &[i64]) -> Self {
        Self { wanted: wanted.to_vec(), frames: Vec::new() }
    }

    fn frame(&self, index: i64) -> &[u8] {
        &self.frames.iter().find(|(i, _)| *i == index).unwrap_or_else(|| panic!("frame {index} was not captured")).1
    }
}

impl FrameSink for Capture {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        if self.wanted.contains(&index) {
            self.frames.push((index, rgba.to_vec()));
        }
        Ok(())
    }
}

fn render_with_root(gpu: &Arc<Gpu>, json: &str, root: &Path, capture: &mut Capture) -> Result<(), RenderError> {
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root(), root.to_path_buf()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).map_err(RenderError)?;
    render(&project, &mut compositor, capture).map(|_| ())
}

/// A folder with the `clock-probe` package, removed on drop. The package paints clip_time / 4 into
/// red and clip_duration / 4 into green, so a frame tells which clock the compositor set.
struct ProbeRoot(PathBuf);

impl ProbeRoot {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("gpufx-clip-clock-{tag}-{}", std::process::id()));
        let package = root.join("packages/gpufx/test/clock-probe/shaders");
        std::fs::create_dir_all(&package).expect("folder");
        let manifest = r#"{"api":1,"id":"test.clock-probe","name":"Probe","used_for":["gpufx"],"inputs":[{"name":"src"}],
            "params":[{"name":"clip_time","type":"float","default":0},{"name":"clip_duration","type":"float","default":0}],
            "groups":[{"passes":[{"shader":"shaders/p.wgsl"}]}]}"#;
        let shader = "@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3u) {\n    if (!gpufx_in_bounds(id)) { return; }\n    gpufx_write(id, vec4f(params.clip_time / 4.0, params.clip_duration / 4.0, 0.0, 1.0));\n}\n";
        std::fs::write(root.join("packages/gpufx/test/clock-probe/manifest.json"), manifest).expect("manifest");
        std::fs::write(package.join("p.wgsl"), shader).expect("shader");
        Self(root)
    }
}

impl Drop for ProbeRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn red_green(rgba: &[u8]) -> (i32, i32) {
    (i32::from(rgba[0]), i32::from(rgba[1]))
}

#[test]
fn a_clip_gets_its_own_clock_whatever_its_speed_or_a_stored_value() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new("clock");
    // Clip 0 is 2 s long at speed 2; clip 1 starts at 2 s. Hand-written stored values must be ignored.
    let effect = r#"{"package":"packages/gpufx/test/clock-probe","params":{"clip_time":99,"clip_duration":99}}"#;
    let json = format!(
        r#"{{"version":1,"output":{{"width":64,"height":36,"frame_duration":{FRAME}}},"tracks":[{{"clips":[
            {{"source":"{}","start":0,"source_in":0,"duration":{},"speed":2.0,"effects":[{effect}]}},
            {{"source":"{}","start":{},"source_in":0,"duration":{},"effects":[{effect}]}}]}}]}}"#,
        json_path(&a), 60 * FRAME, json_path(&b), 60 * FRAME, 30 * FRAME
    );
    let mut capture = Capture::new(&[0, 30, 59, 60, 75]);
    render_with_root(&gpu, &json, &root.0, &mut capture).expect("render");
    // One byte of slack: the GPU rounds 127.5 either way.
    let near = |frame: i64, expected: (i32, i32), what: &str| {
        let got = red_green(capture.frame(frame));
        assert!((got.0 - expected.0).abs() <= 1 && (got.1 - expected.1).abs() <= 1, "{what}: got {got:?}, expected {expected:?}");
    };
    near(0, (0, 128), "start of a 2 s clip");
    near(30, (64, 128), "middle: 1 s on the timeline, the 2x source ran 2 s");
    near(59, (125, 128), "last frame, one frame before the end");
    near(60, (0, 64), "the next clip starts its own 1 s clock");
    near(75, (32, 64), "0.5 s into the second clip");
}

#[test]
fn a_package_without_the_clock_still_refuses_an_unknown_parameter() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new("undeclared");
    let json = format!(
        r#"{{"version":1,"output":{{"width":64,"height":36,"frame_duration":{FRAME}}},"tracks":[{{"clips":[
            {{"source":"{}","start":0,"source_in":0,"duration":{},"effects":[{{"package":"packages/gpufx/ffmpeg/invert","params":{{"clip_time":1}}}}]}}]}}]}}"#,
        json_path(&a),
        30 * FRAME
    );
    let result = render_with_root(&gpu, &json, &root.0, &mut Capture::new(&[]));
    let message = result.expect_err("the parameter does not exist on this package").0;
    assert!(message.contains("clip_time"), "{message}");
}

/// The `avs-effect` pack folder `name` of the GpufxVideoEditor-Assets submodule, or `None` (skip)
/// when the submodule is not checked out.
fn asset_pack(name: &str) -> Option<PathBuf> {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/GpufxVideoEditor-Assets/packages/gpufx/avs-effect").join(name);
    if folder.join("manifest.json").is_file() {
        Some(folder)
    } else {
        eprintln!("skipping: {} is empty, run `git submodule update --init external/GpufxVideoEditor-Assets`", folder.display());
        None
    }
}

fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("folder");
    for entry in std::fs::read_dir(from).expect("read the pack folder") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy");
        }
    }
}

/// Copies the pack `name` into `root` and returns its package path, or `None` (skip).
fn install_pack(root: &ProbeRoot, name: &str) -> Option<String> {
    let folder = asset_pack(name)?;
    copy_folder(&folder, &root.0.join("packages/gpufx/test").join(name));
    Some(format!("packages/gpufx/test/{name}"))
}

/// One clip of `frames` frames at the start of the timeline, with `effects` (JSON list items).
fn single_clip_project(source: &Path, width: u32, height: u32, frames: i64, effects: &str) -> String {
    format!(
        r#"{{"version":1,"output":{{"width":{width},"height":{height},"frame_duration":{FRAME}}},"tracks":[{{"clips":[
            {{"source":"{}","start":0,"source_in":0,"duration":{},"effects":[{effects}]}}]}}]}}"#,
        json_path(source),
        frames * FRAME
    )
}

#[test]
fn a_real_punch_pack_starts_zoomed_and_settles_to_the_unmodified_frame() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new("punch");
    let Some(package) = install_pack(&root, "punch") else { return };
    let frames = [0, 2, 8];
    let mut plain = Capture::new(&frames);
    render_with_root(&gpu, &single_clip_project(&a, CLIP_WIDTH, CLIP_HEIGHT, 30, ""), &root.0, &mut plain).expect("plain render");
    let mut punched = Capture::new(&frames);
    let effect = format!(r#"{{"package":"{package}","params":{{}}}}"#);
    render_with_root(&gpu, &single_clip_project(&a, CLIP_WIDTH, CLIP_HEIGHT, 30, &effect), &root.0, &mut punched).expect("punch render");
    assert_ne!(punched.frame(0), plain.frame(0), "frame 0 is zoomed in");
    assert_ne!(punched.frame(2), plain.frame(2), "0.067 s is still moving");
    assert_eq!(punched.frame(8), plain.frame(8), "after 0.25 s the frame is exactly the unmodified one");
}

#[test]
fn every_clip_effect_pack_renders_on_a_landscape_and_a_portrait_output() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    // (pack, frames that must differ from the clip without the effect). Zoom in, whip and push
    // start on the unmodified frame, so they are judged in the middle of their motion.
    let packs: [(&str, &[i64]); 5] = [("punch", &[0, 2]), ("zoom-in", &[9]), ("whip", &[4]), ("blur-in", &[0, 4]), ("push", &[8])];
    let root = ProbeRoot::new("packs");
    let mut ran = 0;
    for (width, height) in [(CLIP_WIDTH, CLIP_HEIGHT), (180, 320)] {
        let mut plain = Capture::new(&[0, 2, 4, 8, 9]);
        render_with_root(&gpu, &single_clip_project(&a, width, height, 30, ""), &root.0, &mut plain).expect("plain render");
        for (name, moving) in packs {
            let Some(package) = install_pack(&root, name) else { continue };
            let effect = format!(r#"{{"package":"{package}","params":{{}}}}"#);
            let mut with = Capture::new(&[0, 2, 4, 8, 9]);
            render_with_root(&gpu, &single_clip_project(&a, width, height, 30, &effect), &root.0, &mut with)
                .unwrap_or_else(|e| panic!("{name} at {width}x{height}: {}", e.0));
            for &frame in moving {
                assert_ne!(with.frame(frame), plain.frame(frame), "{name} at {width}x{height}: frame {frame} moves");
            }
            ran += 1;
        }
    }
    eprintln!("rendered {ran} pack/orientation pairs");
}

/// Two seconds of a left-to-right red ramp, made once per run.
fn ramp_clip() -> PathBuf {
    audio_test_file(
        "clock-ramp.mp4",
        &[
            "-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=2", "-vf", "format=gbrp,geq=r='X*255/W':g=128:b=128,format=yuv420p",
            "-c:v", "libopenh264", "-pix_fmt", "yuv420p", "-b:v", "4M", "-g", "30",
        ],
    )
}

#[test]
fn whip_pans_a_portrait_frame_by_a_fraction_of_its_width() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new("whip-pan");
    let Some(package) = install_pack(&root, "whip") else { return };
    let ramp = ramp_clip();
    // Frame 5 is just after the half of the 0.3 s motion, where the frame is 420 design pixels aside
    // and scaled up. With the pan taken as a fraction of the width the left edge still shows the
    // ramp; with the pan scaled by the long side over the width it is pushed past the edge and
    // ClampToEdge makes the left columns one colour.
    let effect = format!(r#"{{"package":"{package}","params":{{"blur":0}}}}"#);
    let mut capture = Capture::new(&[5]);
    render_with_root(&gpu, &single_clip_project(&ramp, 180, 320, 30, &effect), &root.0, &mut capture).expect("whip render");
    let row: Vec<i32> = (0..17).map(|x| sample(capture.frame(5), 180, x, 160)).collect();
    eprintln!("whip left edge, red of columns 0..16: {row:?}");
    assert!(row.windows(2).all(|w| w[1] >= w[0] - 1), "the ramp rises towards the right: {row:?}");
    assert!(row[16] - row[0] >= 8, "the left columns are not one colour: {row:?}");
}

#[test]
fn a_zoom_in_clip_shorter_than_its_duration_keeps_the_zoom_in_its_transition() {
    let _lock = gpu_lock();
    let (a, b) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new("zoom-hold");
    let Some(package) = install_pack(&root, "zoom-in") else { return };
    // Clip 0 lasts 12 frames and a transition out of 10 frames starts at frame 7 and ends at frame
    // 17, so the clip is visible for 17 frames (0.567 s), shorter than the default 0.6 s: its clock
    // never reaches the end of the zoom and the zoom must not jump back, as with `hold`.
    let project = |params: &str| {
        format!(
            r#"{{"version":1,"output":{{"width":{CLIP_WIDTH},"height":{CLIP_HEIGHT},"frame_duration":{FRAME}}},"tracks":[{{"clips":[
                {{"source":"{}","start":0,"source_in":0,"duration":{},"effects":[{{"package":"{package}","params":{params}}}]}},
                {{"source":"{}","start":{},"source_in":0,"duration":{}}}],
                "transitions":[{{"after_clip":0,"package":"packages/gpufx/ffmpeg/fade-grays","duration":{}}}]}}]}}"#,
            json_path(&a), 12 * FRAME, json_path(&b), 12 * FRAME, 30 * FRAME, 10 * FRAME
        )
    };
    let frames = [9, 12, 14, 16];
    let mut zoomed = Capture::new(&frames);
    render_with_root(&gpu, &project("{}"), &root.0, &mut zoomed).expect("render");
    let mut held = Capture::new(&frames);
    render_with_root(&gpu, &project(r#"{"hold":true}"#), &root.0, &mut held).expect("render with hold");
    let mut plain = Capture::new(&frames);
    let plain_json = project("{}").replace(&format!(r#"{{"package":"{package}","params":{{}}}}"#), "");
    render_with_root(&gpu, &plain_json, &root.0, &mut plain).expect("plain render");
    for frame in frames {
        assert_eq!(zoomed.frame(frame), held.frame(frame), "frame {frame}: the zoom stays at its end, it does not jump back to scale 1");
    }
    assert_ne!(zoomed.frame(14), plain.frame(14), "the zoom is visible past the end of the clip");
}

#[test]
fn a_zoom_in_clip_longer_than_its_duration_returns_to_the_unmodified_frame() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new("zoom-end");
    let Some(package) = install_pack(&root, "zoom-in") else { return };
    let frames = [3, 25];
    let mut plain = Capture::new(&frames);
    render_with_root(&gpu, &single_clip_project(&a, CLIP_WIDTH, CLIP_HEIGHT, 30, ""), &root.0, &mut plain).expect("plain render");
    let mut zoomed = Capture::new(&frames);
    let effect = format!(r#"{{"package":"{package}","params":{{}}}}"#);
    render_with_root(&gpu, &single_clip_project(&a, CLIP_WIDTH, CLIP_HEIGHT, 30, &effect), &root.0, &mut zoomed).expect("zoom render");
    assert_ne!(zoomed.frame(3), plain.frame(3), "zooming during the first 0.6 s");
    assert_eq!(zoomed.frame(25), plain.frame(25), "after 0.6 s the frame is the unmodified one");
}
