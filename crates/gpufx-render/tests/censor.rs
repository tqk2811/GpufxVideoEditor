//! Censor clips and effect regions through the real compositor on the real GPU: what the tracks
//! below show is hidden inside the box and kept outside it. Skips, with a message, when no
//! Direct3D 12 hardware adapter is available.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use d3d12_backend::{D3d12Compositor, Gpu, ProjectPaths};
use engine::render::{render, FrameSink, RenderError};
use engine::{validate, Project};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 36;

fn gpu() -> Option<Arc<Gpu>> {
    match Gpu::create_hardware() {
        Ok(Some(gpu)) => Some(Arc::new(gpu)),
        Ok(None) => {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but there is no hardware adapter");
            eprintln!("skipping: no D3D12 hardware adapter is available");
            None
        }
        Err(message) => panic!("opening the GPU failed: {message}"),
    }
}

/// Vertical stripes one pixel wide, white and black, as a PNG in a folder removed on drop.
struct Stripes(PathBuf);

impl Stripes {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gpufx-censor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("folder");
        let path = dir.join("stripes.png");
        let mut encoder = png::Encoder::new(std::fs::File::create(&path).expect("create"), WIDTH, HEIGHT);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let data: Vec<u8> = (0..WIDTH * HEIGHT).flat_map(|i| stripe(i % WIDTH)).collect();
        encoder.write_header().expect("header").write_image_data(&data).expect("data");
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("stripes.png")
    }
}

impl Drop for Stripes {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stripe(x: u32) -> [u8; 4] {
    if x % 2 == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] }
}

struct Capture(Vec<u8>);

impl FrameSink for Capture {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        if index == 0 {
            self.0 = rgba.to_vec();
        }
        Ok(())
    }
}

fn run(gpu: &Arc<Gpu>, tracks: &str) -> Vec<u8> {
    let json = format!(r#"{{"version":1,"output":{{"width":{WIDTH},"height":{HEIGHT},"frame_rate":{{"num":90000,"den":{FRAME}}}}},"tracks":[{tracks}]}}"#);
    let project = Project::from_json(&json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, WIDTH, HEIGHT).expect("a compositor");
    let mut capture = Capture(Vec::new());
    render(&project, &mut compositor, &mut capture).expect("rendered");
    capture.0
}

fn red(rgba: &[u8], x: u32, y: u32) -> u8 {
    rgba[((y * WIDTH + x) * 4) as usize]
}

/// The middle of the frame (x 16..48, y 9..27) is blocks of 4 pixels holding the mean of the
/// stripes, and the rest is the stripes.
fn assert_pixelated_middle(rgba: &[u8]) {
    for y in 10..26 {
        for x in 17..47 {
            let value = red(rgba, x, y);
            assert!((96..=160).contains(&value), "inside, the mean of the stripes at ({x}, {y}): {value}");
        }
    }
    for (x, y) in [(2, 18), (3, 18), (60, 18), (61, 18), (30, 2), (31, 2), (30, 33), (31, 33)] {
        assert_eq!(red(rgba, x, y), stripe(x)[0], "outside, the stripes at ({x}, {y})");
    }
}

#[test]
fn a_censor_clip_pixelates_the_track_below_inside_its_box() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let stripes = Stripes::new("clip");
    let image = format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}}}]}}"#, json_path(&stripes.path()), 3 * FRAME);
    // 120 pixels of a 1080-line frame are 4 at 36 lines.
    let censor = format!(r#"{{"kind":"sticker","clips":[{{"source":"censor:","start":0,"source_in":0,"duration":{},"placement":{{"x":0.25,"y":0.25,"width":0.5}},"censor":{{"aspect":1.7777777777777777,"mode":{{"kind":"pixelate","block":120}}}}}}]}}"#, 3 * FRAME);
    let rgba = run(&gpu, &format!("{image},{censor}"));
    assert_pixelated_middle(&rgba);
}

#[test]
fn a_pixelate_effect_limited_to_a_region_hides_only_that_part_of_its_clip() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let stripes = Stripes::new("region");
    let image = format!(
        r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{},"effects":[{{"package":"packages/gpufx/ffmpeg/pixelate","params":{{"block":4}},"region":{{"cx":0.5,"cy":0.5,"w":0.5,"h":0.5}}}}]}}]}}"#,
        json_path(&stripes.path()),
        3 * FRAME
    );
    let rgba = run(&gpu, &image);
    assert_pixelated_middle(&rgba);
}
