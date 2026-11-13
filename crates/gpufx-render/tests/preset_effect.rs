//! The gpufx effect of a pack preset through the real compositor, on the real GPU and decoder:
//! inside the in and out windows the effect paints the frame, outside them the clip is untouched.
//! Skips, with a message, when no Direct3D 12 hardware adapter is available.

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

struct Capture {
    wanted: Vec<i64>,
    frames: Vec<(i64, Vec<u8>)>,
}

impl FrameSink for Capture {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        if self.wanted.contains(&index) {
            self.frames.push((index, rgba.to_vec()));
        }
        Ok(())
    }
}

impl Capture {
    fn frame(&self, index: i64) -> &[u8] {
        &self.frames.iter().find(|(i, _)| *i == index).unwrap_or_else(|| panic!("frame {index} was not captured")).1
    }
}

/// A root with the gpufx package `test.amount-probe` (red = `amount`, green = `level`) and the
/// clip animation pack `fx` whose in and out items run it with `amount` as their progress.
struct ProbeRoot(PathBuf);

impl ProbeRoot {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("gpufx-preset-effect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let package = root.join("packages/gpufx/test/amount-probe");
        std::fs::create_dir_all(package.join("shaders")).expect("folder");
        let manifest = r#"{"api":1,"id":"test.amount-probe","name":"Probe","used_for":["gpufx"],"inputs":[{"name":"src"}],
            "params":[{"name":"amount","type":"float","default":0},{"name":"level","type":"float","default":0}],
            "groups":[{"passes":[{"shader":"shaders/p.wgsl"}]}]}"#;
        let shader = "@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3u) {\n    if (!gpufx_in_bounds(id)) { return; }\n    gpufx_write(id, vec4f(params.amount, params.level, 0.0, 1.0));\n}\n";
        std::fs::write(package.join("manifest.json"), manifest).expect("manifest");
        std::fs::write(package.join("shaders/p.wgsl"), shader).expect("shader");
        let pack = root.join("packages/clip-animation/fx");
        std::fs::create_dir_all(&pack).expect("folder");
        let effect = r#"{"package":"test.amount-probe","progress":"amount","params":{"level":0.25}}"#;
        let pack_manifest = format!(
            r#"{{"format":1,"type":"clip-animation","id":"fx","name":"Fx","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[
                {{"id":"in","name":"In","kind":"in","default_duration":1,"effect":{effect}}},
                {{"id":"out","name":"Out","kind":"out","default_duration":1,"effect":{effect}}},
                {{"id":"gone","name":"Gone","kind":"in","default_duration":1,"effect":{{"package":"test.not-installed"}},"curve":{{"tracks":{{"opacity":[{{"t":0,"value":1}}]}}}}}}]}}"#
        );
        std::fs::write(pack.join("manifest.json"), pack_manifest).expect("pack");
        Self(root)
    }
}

impl Drop for ProbeRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn render_frames(gpu: &Arc<Gpu>, json: &str, root: &Path, wanted: &[i64]) -> Capture {
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root(), root.to_path_buf()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).expect("compositor");
    let mut capture = Capture { wanted: wanted.to_vec(), frames: Vec::new() };
    render(&project, &mut compositor, &mut capture).expect("render");
    capture
}

/// The byte of the middle pixel of a 64 x 36 frame, away from the bars of the test clip.
const CENTER: usize = (18 * 64 + 32) * 4;

fn red_green_blue(rgba: &[u8]) -> (i32, i32, i32) {
    (i32::from(rgba[CENTER]), i32::from(rgba[CENTER + 1]), i32::from(rgba[CENTER + 2]))
}

#[test]
fn a_preset_effect_paints_only_inside_its_windows() {
    let _lock = gpu_lock();
    let (a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = ProbeRoot::new();
    engine::set_pack_presets(d3d12_backend::read_pack_presets(&[packages_root(), root.0.clone()]));

    // A 60-frame clip with a 10-frame in and a 10-frame out.
    let json = format!(
        r#"{{"version":2,"output":{{"width":64,"height":36,"frame_duration":{FRAME}}},"tracks":[{{"clips":[
            {{"source":"{}","start":0,"source_in":0,"duration":{},"animation":{{"in":{{"preset":"fx/in","duration":{}}},"out":{{"preset":"fx/out","duration":{}}}}}}}]}}]}}"#,
        json_path(&a), 60 * FRAME, 10 * FRAME, 10 * FRAME
    );
    let capture = render_frames(&gpu, &json, &root.0, &[0, 5, 10, 30, 50, 55, 59]);
    let near = |frame: i64, red: i32| {
        let got = red_green_blue(capture.frame(frame));
        assert!((got.0 - red).abs() <= 1 && (got.1 - 64).abs() <= 1 && got.2 == 0, "frame {frame}: got {got:?}, expected red {red} and the static green 64");
    };
    near(0, 0);
    near(5, 128);
    near(55, 128);
    near(59, 26);
    for frame in [10, 30, 50] {
        assert_grey(capture.frame(frame), frame);
    }

    // The item whose package is not installed plays without an effect.
    let gone = json.replace("fx/in", "fx/gone");
    let capture = render_frames(&gpu, &gone, &root.0, &[5]);
    assert_grey(capture.frame(5), 5);
}

/// The middle pixel shows the grey clip, not the probe.
fn assert_grey(rgba: &[u8], frame: i64) {
    let got = red_green_blue(rgba);
    assert!((got.0 - got.1).abs() <= 2 && (got.1 - got.2).abs() <= 2 && got.2 > 8, "frame {frame} must show the clip untouched, got {got:?}");
}
