//! Still image clips through the real compositor on the real GPU: alpha over what is below, a clip
//! longer than a still has any "length", and no re-read of the file. Skips, with a message, when no
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

/// A folder for the pictures of a test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gpufx-image-clip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("folder");
        Self(dir)
    }

    /// Writes an 8-bit RGBA PNG, `(x, y) -> [r, g, b, a]`.
    fn png(&self, name: &str, size: (u32, u32), pixel: impl Fn(u32, u32) -> [u8; 4]) -> PathBuf {
        let path = self.0.join(name);
        let mut encoder = png::Encoder::new(std::fs::File::create(&path).expect("create"), size.0, size.1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        let mut data = Vec::with_capacity((size.0 * size.1 * 4) as usize);
        for y in 0..size.1 {
            for x in 0..size.0 {
                data.extend_from_slice(&pixel(x, y));
            }
        }
        writer.write_image_data(&data).expect("data");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
        &self.frames.iter().find(|(frame, _)| *frame == index).unwrap_or_else(|| panic!("frame {index}")).1
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

fn run(gpu: &Arc<Gpu>, json: &str, capture: &mut Capture) -> Result<(), RenderError> {
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).map_err(RenderError)?;
    render(&project, &mut compositor, capture).map(|_| ())
}

fn pixel(rgba: &[u8], x: u32, y: u32) -> [i32; 3] {
    let at = (y * WIDTH + x) as usize * 4;
    [i32::from(rgba[at]), i32::from(rgba[at + 1]), i32::from(rgba[at + 2])]
}

#[track_caller]
fn assert_near(actual: [i32; 3], expected: [i32; 3], tolerance: i32, what: &str) {
    assert!(actual.iter().zip(expected).all(|(a, e)| (a - e).abs() <= tolerance), "{what}: {actual:?}, expected {expected:?} within {tolerance}");
}

fn project_json(tracks: &str) -> String {
    format!(r#"{{"version":1,"output":{{"width":{WIDTH},"height":{HEIGHT},"frame_duration":{FRAME}}},"tracks":[{tracks}]}}"#)
}

fn image_track(path: &Path, duration: i64, placement: &str) -> String {
    format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}{placement}}}]}}"#, json_path(path), duration * FRAME)
}

#[test]
fn half_transparent_red_over_a_real_video_mixes_with_the_video() {
    let _lock = gpu_lock();
    let (clip_a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("over-video");
    // The right half is fully transparent and paints an obvious colour under the alpha: it must not show.
    let sticker = scratch.png("red.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [255, 0, 0, 128] } else { [0, 255, 0, 0] });
    let video_track = format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}}}]}}"#, json_path(&clip_a), 10 * FRAME);

    let mut below = Capture::new(&[3]);
    run(&gpu, &project_json(&video_track), &mut below).expect("the video alone");
    let mut both = Capture::new(&[3]);
    run(&gpu, &project_json(&format!("{video_track},{}", image_track(&sticker, 10, ""))), &mut both).expect("the image on top");

    let base = pixel(below.frame(3), WIDTH / 4, HEIGHT - 4);
    assert_near(base, [GREY_A, GREY_A, GREY_A], 6, "the grey of the clip");
    let mixed = pixel(both.frame(3), WIDTH / 4, HEIGHT - 4);
    let expected = [(255 * 128 + base[0] * 127) / 255, base[1] * 127 / 255, base[2] * 127 / 255];
    assert_near(mixed, expected, 8, "red at half alpha over the video");
    assert!(mixed[0] > base[0] + 40, "premultiplied colours mixed again would come out dark: {mixed:?}");
    assert_near(pixel(both.frame(3), WIDTH - 8, HEIGHT - 4), base, 4, "where the picture is transparent the video shows, with no green from under the alpha");
}

/// A root of test packages, removed on drop: `solid-alpha` writes an alpha of one over its input,
/// `move-right` moves it half the frame to the right, `solid-mix` mixes two inputs with an alpha of one.
struct AlphaPackages(PathBuf);

impl AlphaPackages {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("gpufx-image-alpha-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |name: &str, inputs: &str, params: &str, body: &str| {
            let folder = root.join("packages/gpufx/test").join(name);
            std::fs::create_dir_all(folder.join("shaders")).expect("folder");
            let manifest = format!(r#"{{"api":1,"id":"test.{name}","name":"T","used_for":["gpufx"],"inputs":[{inputs}],"params":[{params}],"groups":[{{"passes":[{{"shader":"shaders/p.wgsl"}}]}}]}}"#);
            std::fs::write(folder.join("manifest.json"), manifest).expect("manifest");
            let shader = format!("@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3u) {{\n    if (!gpufx_in_bounds(id)) {{ return; }}\n    let p = vec2i(id.xy);\n{body}\n}}\n");
            std::fs::write(folder.join("shaders/p.wgsl"), shader).expect("shader");
        };
        write("solid-alpha", r#"{"name":"src"}"#, "", "    gpufx_write(id, vec4f(textureLoad(src, p, 0).rgb, 1.0));");
        write("move-right", r#"{"name":"src"}"#, "", "    let half = i32(textureDimensions(src).x) / 2;\n    if (p.x < half) { gpufx_write(id, vec4f(0.0)); return; }\n    gpufx_write(id, textureLoad(src, p - vec2i(half, 0), 0));");
        write(
            "solid-mix",
            r#"{"name":"first"},{"name":"second","same_size":true}"#,
            r#"{"name":"progress","type":"float","default":0}"#,
            "    let a = textureLoad(first, p, 0).rgb;\n    let b = textureLoad(second, p, 0).rgb;\n    gpufx_write(id, vec4f(mix(a, b, params.progress), 1.0));",
        );
        Self(root)
    }
}

impl Drop for AlphaPackages {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_with_root(gpu: &Arc<Gpu>, json: &str, root: &Path, capture: &mut Capture) -> Result<(), RenderError> {
    let project = Project::from_json(json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root(), root.to_path_buf()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).map_err(RenderError)?;
    render(&project, &mut compositor, capture).map(|_| ())
}

fn effect_track(path: &Path, duration: i64, package: &str) -> String {
    image_track(path, duration, &format!(r#","effects":[{{"package":"{package}","params":{{}}}}]"#))
}

/// A sticker over a video: `(left of the picture, its right half, the video alone)` at frame 3.
fn sticker_with_effect(gpu: &Arc<Gpu>, root: &AlphaPackages, sticker: &Path, package: &str) -> ([i32; 3], [i32; 3], [i32; 3]) {
    let (clip_a, _) = test_clips();
    let video = format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}}}]}}"#, json_path(&clip_a), 10 * FRAME);
    let mut capture = Capture::new(&[3]);
    run_with_root(gpu, &project_json(&format!("{video},{}", effect_track(sticker, 10, package))), &root.0, &mut capture).expect(package);
    let mut below = Capture::new(&[3]);
    run_with_root(gpu, &project_json(&video), &root.0, &mut below).expect("the video alone");
    let (x_left, x_right, y) = (WIDTH / 4, WIDTH - 8, HEIGHT - 4);
    (pixel(capture.frame(3), x_left, y), pixel(capture.frame(3), x_right, y), pixel(below.frame(3), x_right, y))
}

#[test]
fn an_effect_that_writes_alpha_does_not_turn_see_through_parts_into_a_block() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let root = AlphaPackages::new("solid");
    let scratch = Scratch::new("effect-alpha");
    // Left half opaque, right half transparent with a colour the alpha hides.
    let sticker = scratch.png("s.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [255, 0, 0, 255] } else { [0, 255, 0, 0] });
    for package in ["packages/gpufx/test/solid-alpha", "packages/gpufx/ffmpeg/alpha-extract", "packages/gpufx/ffmpeg/edgedetect"] {
        let (_, right, video) = sticker_with_effect(&gpu, &root, &sticker, package);
        assert_near(right, video, 6, package);
    }
}

#[test]
fn an_effect_that_keeps_alpha_stays_as_it_was_even_when_it_moves_the_picture() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let root = AlphaPackages::new("keep");
    let scratch = Scratch::new("keep-alpha");
    let sticker = scratch.png("s.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [200, 0, 0, 255] } else { [0, 200, 0, 0] });
    let (left, right, video) = sticker_with_effect(&gpu, &root, &sticker, "packages/gpufx/ffmpeg/invert");
    assert_near(left, [55, 255, 255], 8, "the picture is inverted");
    assert_near(right, video, 6, "still see-through");
    let (left, right, video) = sticker_with_effect(&gpu, &root, &sticker, "packages/gpufx/test/move-right");
    assert_near(left, video, 6, "the picture moved out of here");
    assert_near(right, [200, 0, 0], 8, "the picture moved here: its alpha moved with it, it was not cut by the source's");
}

#[test]
fn a_transition_that_writes_alpha_between_stickers_keeps_their_see_through_parts() {
    let _lock = gpu_lock();
    let (clip_a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let root = AlphaPackages::new("mix");
    let scratch = Scratch::new("transition-alpha");
    let first = scratch.png("a.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [255, 0, 0, 255] } else { [0, 0, 0, 0] });
    let second = scratch.png("b.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [0, 0, 255, 255] } else { [0, 0, 0, 0] });
    let video = format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}}}]}}"#, json_path(&clip_a), 30 * FRAME);
    let top = format!(
        r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{d}}},{{"source":"{}","start":{d},"source_in":0,"duration":{d}}}],"transitions":[{{"after_clip":0,"package":"packages/gpufx/test/solid-mix","duration":{t}}}]}}"#,
        json_path(&first),
        json_path(&second),
        d = 15 * FRAME,
        t = 10 * FRAME
    );
    let mut capture = Capture::new(&[15]);
    run_with_root(&gpu, &project_json(&format!("{video},{top}")), &root.0, &mut capture).expect("render");
    let mut below = Capture::new(&[15]);
    run_with_root(&gpu, &project_json(&video), &root.0, &mut below).expect("the video alone");
    let (x_right, x_left, y) = (WIDTH - 8, WIDTH / 4, HEIGHT - 4);
    assert_near(pixel(capture.frame(15), x_right, y), pixel(below.frame(15), x_right, y), 6, "a transparent part of both stickers");
    let middle = pixel(capture.frame(15), x_left, y);
    assert!(middle[0] > 60 && middle[2] > 60 && middle[1] < 40, "the two pictures mixed: {middle:?}");
}

#[test]
fn a_placed_image_mixes_correctly_and_only_inside_its_rectangle() {
    let _lock = gpu_lock();
    let (clip_a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("placed");
    let white = scratch.png("white.png", (32, 32), |_, _| [255, 255, 255, 64]);
    let placed = r#","placement":{"x":0.25,"y":0.25,"width":0.5}"#;
    let video_track = format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}}}]}}"#, json_path(&clip_a), 5 * FRAME);
    let mut both = Capture::new(&[2]);
    run(&gpu, &project_json(&format!("{video_track},{}", image_track(&white, 5, placed))), &mut both).expect("render");
    let mut below = Capture::new(&[2]);
    run(&gpu, &project_json(&video_track), &mut below).expect("the video alone");
    let base = pixel(below.frame(2), WIDTH / 2, HEIGHT / 2);
    let expected = (255 * 64 + base[0] * 191) / 255;
    assert_near(pixel(both.frame(2), WIDTH / 2, HEIGHT / 2), [expected; 3], 8, "white at quarter alpha inside the rectangle");
    assert_near(pixel(both.frame(2), 2, HEIGHT / 2), pixel(below.frame(2), 2, HEIGHT / 2), 4, "outside the rectangle only the video");
}

#[test]
fn a_lone_transparent_image_comes_out_as_over_black() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("alone");
    let red = scratch.png("red.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [255, 0, 0, 128] } else { [9, 200, 9, 0] });
    let mut capture = Capture::new(&[0]);
    run(&gpu, &project_json(&image_track(&red, 2, "")), &mut capture).expect("render");
    assert_near(pixel(capture.frame(0), WIDTH / 4, HEIGHT / 2), [128, 0, 0], 8, "half red over black");
    assert_near(pixel(capture.frame(0), WIDTH - 8, HEIGHT / 2), [0, 0, 0], 6, "transparent, whatever colour is under the alpha");
}

#[test]
fn a_clip_longer_than_five_seconds_keeps_showing_the_picture_and_the_file_is_read_once() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("long");
    let picture = scratch.png("blue.png", (WIDTH, HEIGHT), |_, _| [10, 20, 200, 255]);
    // 8 seconds at 30 fps: 240 frames, with the last frames past five seconds.
    let project = project_json(&image_track(&picture, 240, ""));
    let parsed = Project::from_json(&project).unwrap();
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths.clone(), WIDTH, HEIGHT).expect("compositor");
    let mut renderer = engine::render::FrameRenderer::default();
    let mut first = Vec::new();
    renderer.render_frame(&parsed, &mut compositor, 0, |compositor, image| engine::render::Compositor::read(compositor, image, &mut first)).expect("frame 0");
    assert_near(pixel(&first, WIDTH / 2, HEIGHT / 2), [10, 20, 200], 3, "the first frame");
    std::fs::write(&picture, b"rubbish, had it been read again").expect("overwrite");
    let mut late = Vec::new();
    renderer.render_frame(&parsed, &mut compositor, 239, |compositor, image| engine::render::Compositor::read(compositor, image, &mut late)).expect("frame 239");
    assert_eq!(first, late, "the same picture past five seconds, the file not read again");
}
