//! Opacity, rotation and keyframes through the real compositor on the real GPU. Skips, with a
//! message, when no Direct3D 12 hardware adapter is available.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::*;
use d3d12_backend::{D3d12Compositor, Gpu, ProjectPaths};
use engine::render::{render, Compositor, FrameRenderer, FrameSink, RenderError};
use engine::{trim_start, validate, ClipKey, Project, Time};

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

/// A folder for the files of a test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gpufx-motion-{tag}-{}", std::process::id()));
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

    /// A test package whose picture is a grey of the float parameter `level`, as a package root.
    fn level_package(&self) -> PathBuf {
        let folder = self.0.join("packages/gpufx/test/level");
        std::fs::create_dir_all(folder.join("shaders")).expect("folder");
        let manifest = r#"{"api":1,"id":"test.level","name":"T","used_for":["gpufx"],"inputs":[{"name":"src"}],"params":[{"name":"level","type":"float","default":0,"min":0,"max":1}],"groups":[{"passes":[{"shader":"shaders/p.wgsl"}]}]}"#;
        std::fs::write(folder.join("manifest.json"), manifest).expect("manifest");
        let shader = "@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3u) {\n    if (!gpufx_in_bounds(id)) { return; }\n    gpufx_write(id, vec4f(vec3f(params.level), 1.0));\n}\n";
        std::fs::write(folder.join("shaders/p.wgsl"), shader).expect("shader");
        self.0.clone()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Keeps the RGBA of every frame.
struct Capture(Vec<Vec<u8>>);

impl Capture {
    fn frame(&self, index: i64) -> &[u8] {
        &self.0[usize::try_from(index).expect("a frame index")]
    }
}

impl FrameSink for Capture {
    fn write_frame(&mut self, _index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        self.0.push(rgba.to_vec());
        Ok(())
    }
}

fn run(gpu: &Arc<Gpu>, project: &Project, root: Option<&Path>) -> Capture {
    let mut roots = vec![packages_root()];
    roots.extend(root.map(Path::to_path_buf));
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), roots));
    let errors = validate(project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, project.output.width, project.output.height).expect("compositor");
    let mut capture = Capture(Vec::new());
    render(project, &mut compositor, &mut capture).expect("render");
    capture
}

fn parse(json: &str) -> Project {
    Project::from_json(json).expect("project parses")
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
    format!(r#"{{"version":2,"output":{{"width":{WIDTH},"height":{HEIGHT},"frame_duration":{FRAME}}},"tracks":[{tracks}]}}"#)
}

fn image_track(path: &Path, frames: i64, extra: &str) -> String {
    format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}{extra}}}]}}"#, json_path(path), frames * FRAME)
}

fn video_track(path: &Path, frames: i64) -> String {
    format!(r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":{}}}]}}"#, json_path(path), frames * FRAME)
}

fn white(scratch: &Scratch) -> PathBuf {
    scratch.png("white.png", (WIDTH, HEIGHT), |_, _| [255, 255, 255, 255])
}

#[test]
fn half_opacity_mixes_a_full_frame_picture_with_what_is_below() {
    let _lock = gpu_lock();
    let (clip_a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("opacity");
    let base = video_track(&clip_a, 5);
    let image = white(&scratch);
    let top = image_track(&image, 5, r#","motion":{"opacity":0.5}"#);
    let below = run(&gpu, &parse(&project_json(&base)), None);
    let mixed = run(&gpu, &parse(&project_json(&format!("{base},{top}"))), None);
    let grey = pixel(below.frame(2), WIDTH / 2, HEIGHT / 2)[0];
    let expected = (255 + grey) / 2;
    assert_near(pixel(mixed.frame(2), WIDTH / 2, HEIGHT - 4), [expected; 3], 8, "white at half opacity over the video");
}

#[test]
fn a_lone_half_opaque_picture_comes_out_as_over_black() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("opacity-alone");
    let image = white(&scratch);
    let frames = run(&gpu, &parse(&project_json(&image_track(&image, 2, r#","motion":{"opacity":0.25}"#))), None);
    assert_near(pixel(frames.frame(0), WIDTH / 2, HEIGHT / 2), [64; 3], 6, "a quarter of white");
}

/// A landscape picture, as wide as twice its height, placed in the middle of the frame.
fn rotated_landscape(scratch: &Scratch) -> (PathBuf, &'static str) {
    let image = scratch.png("wide.png", (32, 16), |_, _| [255, 255, 255, 255]);
    (image, r#","placement":{"x":0.25,"y":0.2777777777777778,"width":0.5}"#)
}

#[test]
fn a_quarter_turn_keeps_the_shape_of_the_picture() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("turn");
    let (image, placement) = rotated_landscape(&scratch);
    let upright = run(&gpu, &parse(&project_json(&image_track(&image, 1, placement))), None);
    assert_near(pixel(upright.frame(0), 20, HEIGHT / 2), [255; 3], 8, "the upright picture reaches 20");
    assert_near(pixel(upright.frame(0), WIDTH / 2, 8), [0; 3], 8, "and not as high as 8");

    let turned = run(&gpu, &parse(&project_json(&image_track(&image, 1, &format!(r#"{placement},"motion":{{"rotation":90}}"#)))), None);
    // In output pixels 32 wide and 16 tall around the middle, so 16 wide and 32 tall after a quarter turn.
    assert_near(pixel(turned.frame(0), WIDTH / 2, 4), [255; 3], 8, "the turned picture is as tall as it was wide");
    assert_near(pixel(turned.frame(0), WIDTH / 2, HEIGHT - 5), [255; 3], 8, "both ways");
    assert_near(pixel(turned.frame(0), 20, HEIGHT / 2), [0; 3], 8, "and no longer as wide");
}

#[test]
fn a_turn_on_a_frame_of_another_shape_is_not_skewed() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("skew");
    // A square image placed as a square of the frame's height.
    let square = scratch.png("square.png", (16, 16), |_, _| [255, 255, 255, 255]);
    let placed = r#","placement":{"x":0.21875,"y":0.0,"width":0.5625}"#;
    let turned = run(&gpu, &parse(&project_json(&image_track(&square, 1, &format!(r#"{placed},"motion":{{"rotation":90}}"#)))), None);
    // 36 pixels wide and tall on a frame of 64 by 36: a quarter turn of a square gives the same square.
    assert_near(pixel(turned.frame(0), WIDTH / 2, 1), [255; 3], 8, "the top edge is where it was");
    assert_near(pixel(turned.frame(0), 16, HEIGHT / 2), [255; 3], 8, "the left edge");
    assert_near(pixel(turned.frame(0), 10, HEIGHT / 2), [0; 3], 8, "and nothing past it");
}

#[test]
fn a_turned_see_through_part_stays_see_through_and_leaves_no_fringe() {
    let _lock = gpu_lock();
    let (clip_a, _) = test_clips();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("turn-alpha");
    // Left half opaque red, right half transparent over a colour that must never show.
    let sticker = scratch.png("s.png", (WIDTH, HEIGHT), |x, _| if x < WIDTH / 2 { [255, 0, 0, 255] } else { [0, 255, 0, 0] });
    let base = video_track(&clip_a, 3);
    let top = image_track(&sticker, 3, r#","motion":{"rotation":180}"#);
    let below = run(&gpu, &parse(&project_json(&base)), None);
    let turned = run(&gpu, &parse(&project_json(&format!("{base},{top}"))), None);
    let y = HEIGHT - 4;
    assert_near(pixel(turned.frame(1), WIDTH - 12, y), [255, 0, 0], 12, "half a turn puts the red on the right");
    assert_near(pixel(turned.frame(1), 12, y), pixel(below.frame(1), 12, y), 8, "and the video shows on the left");
    for x in 0..WIDTH {
        assert!(pixel(turned.frame(1), x, y)[1] < 200, "no green from under the alpha at {x}: {:?}", pixel(turned.frame(1), x, y));
    }
}

#[test]
fn the_edge_of_a_turned_picture_is_smooth() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("edge");
    let (image, placement) = rotated_landscape(&scratch);
    let turned = run(&gpu, &parse(&project_json(&image_track(&image, 1, &format!(r#"{placement},"motion":{{"rotation":30}}"#)))), None);
    let partial = (0..WIDTH).flat_map(|x| (0..HEIGHT).map(move |y| (x, y))).filter(|&(x, y)| {
        let value = pixel(turned.frame(0), x, y)[0];
        (24..232).contains(&value)
    });
    assert!(partial.count() > 8, "a smooth edge has pixels in between");
}

#[test]
fn opacity_keyframes_step_exactly_with_the_frames() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("opacity-keys");
    let image = white(&scratch);
    let end = 8 * FRAME;
    let motion = format!(r#","motion":{{"keyframes":{{"opacity":[{{"at":0,"value":0}},{{"at":{end},"value":1}}]}}}}"#);
    let frames = run(&gpu, &parse(&project_json(&image_track(&image, 9, &motion))), None);
    let middle = |index: i64| pixel(frames.frame(index), WIDTH / 2, HEIGHT / 2)[0];
    assert!(middle(0) <= 4, "at the first keyframe: {}", middle(0));
    assert!((middle(8) - 255).abs() <= 2, "at the last keyframe: {}", middle(8));
    assert!((middle(4) - 128).abs() <= 8, "halfway: {}", middle(4));
    let rising: Vec<i32> = (0..=8).map(middle).collect();
    assert!(rising.windows(2).all(|pair| pair[1] > pair[0]), "{rising:?}");
}

#[test]
fn a_keyframed_place_moves_the_picture_across_the_frame() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("move");
    let image = scratch.png("small.png", (16, 9), |_, _| [255, 255, 255, 255]);
    let end = 4 * FRAME;
    let motion = format!(r#","placement":{{"x":0.0,"y":0.25,"width":0.25}},"motion":{{"keyframes":{{"x":[{{"at":0,"value":0}},{{"at":{end},"value":0.75}}]}}}}"#);
    let frames = run(&gpu, &parse(&project_json(&image_track(&image, 5, &motion))), None);
    // The picture is 16 by 9 pixels from row 9 to row 18.
    let row = 13;
    assert_near(pixel(frames.frame(0), 4, row), [255; 3], 8, "the left of the frame at first");
    assert_near(pixel(frames.frame(0), WIDTH - 4, row), [0; 3], 8, "with nothing on the right");
    assert_near(pixel(frames.frame(4), WIDTH - 4, row), [255; 3], 8, "the right of the frame at last");
    assert_near(pixel(frames.frame(4), 4, row), [0; 3], 8, "with nothing on the left");
}

#[test]
fn a_keyframed_turn_differs_frame_by_frame() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("spin");
    let (image, placement) = rotated_landscape(&scratch);
    let end = 2 * FRAME;
    let motion = format!(r#"{placement},"motion":{{"keyframes":{{"rotation":[{{"at":0,"value":0}},{{"at":{end},"value":90}}]}}}}"#);
    let frames = run(&gpu, &parse(&project_json(&image_track(&image, 3, &motion))), None);
    assert_near(pixel(frames.frame(0), 20, HEIGHT / 2), [255; 3], 8, "frame 0 has no turn");
    assert_near(pixel(frames.frame(2), WIDTH / 2, 4), [255; 3], 8, "frame 2 has the whole quarter turn");
    assert_ne!(frames.frame(1), frames.frame(0), "the turn in between");
    assert_ne!(frames.frame(1), frames.frame(2));
}

#[test]
fn a_keyframed_effect_parameter_reaches_the_shader_every_frame() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("effect-keys");
    let root = scratch.level_package();
    let image = scratch.png("image.png", (WIDTH, HEIGHT), |_, _| [10, 10, 10, 255]);
    let end = 4 * FRAME;
    let effects = format!(r#","effects":[{{"package":"packages/gpufx/test/level","params":{{"level":0.5}},"keyframes":{{"level":[{{"at":0,"value":0}},{{"at":{end},"value":1}}]}}}}]"#);
    let frames = run(&gpu, &parse(&project_json(&image_track(&image, 5, &effects))), Some(&root));
    let value = |index: i64| pixel(frames.frame(index), WIDTH / 2, HEIGHT / 2)[0];
    assert!(value(0) <= 2, "keyframed over the static value: {}", value(0));
    assert!((value(4) - 255).abs() <= 2, "{}", value(4));
    assert!((value(2) - 128).abs() <= 3, "halfway: {}", value(2));
}

#[test]
fn a_trim_keeps_what_each_frame_of_the_timeline_looks_like() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let scratch = Scratch::new("trim");
    let image = white(&scratch);
    let end = 8 * FRAME;
    let motion = format!(r#","motion":{{"keyframes":{{"opacity":[{{"at":0,"value":0,"easing":"ease_in_out"}},{{"at":{end},"value":1}}]}}}}"#);
    let original = parse(&project_json(&image_track(&image, 9, &motion)));
    let trimmed = trim_start(&original, ClipKey { track: 0, clip: 0 }, Time::from_ticks(3 * FRAME)).expect("trim");
    let whole = run(&gpu, &original, None);
    let cut = run(&gpu, &trimmed, None);
    assert_near(pixel(cut.frame(1), WIDTH / 2, HEIGHT / 2), [0; 3], 2, "before the clip now");
    for index in 3..9 {
        assert_eq!(pixel(cut.frame(index), WIDTH / 2, HEIGHT / 2), pixel(whole.frame(index), WIDTH / 2, HEIGHT / 2), "frame {index}");
    }
}

fn text_track(frames: i64, color: &str) -> String {
    format!(r#"{{"clips":[{{"source":"text:","start":0,"source_in":0,"duration":{},"text":{{"content":"HI","size":0.5,"color":{color}}}}}]}}"#, frames * FRAME)
}

/// A compositor that draws text with the system fonts, so `HI` comes out in the default font.
fn text_compositor(gpu: &Arc<Gpu>) -> D3d12Compositor {
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, WIDTH, HEIGHT).expect("compositor");
    compositor.set_text_rasterizer(Arc::new(Mutex::new(gpufx_text::TextRasterizer::new())));
    compositor
}

fn text_frame(compositor: &mut D3d12Compositor, project: &Project) -> Vec<u8> {
    let errors = validate(project, &engine::AcceptAll);
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut renderer = FrameRenderer::default();
    renderer
        .render_frame(project, compositor, 0, |compositor, image| {
            let mut rgba = Vec::new();
            compositor.read(image, &mut rgba)?;
            Ok(rgba)
        })
        .expect("frame")
}

#[test]
fn a_text_clip_draws_its_text_in_its_colour() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let mut compositor = text_compositor(&gpu);
    let frame = text_frame(&mut compositor, &parse(&project_json(&text_track(2, "[255,0,0,255]"))));
    let lit: Vec<[i32; 3]> = (0..HEIGHT).flat_map(|y| (0..WIDTH).map(move |x| (x, y))).map(|(x, y)| pixel(&frame, x, y)).filter(|p| p.iter().any(|&c| c > 32)).collect();
    assert!(lit.len() > 40, "the text lights up pixels: {}", lit.len());
    assert!(lit.len() < (WIDTH * HEIGHT) as usize * 3 / 4, "and not the whole frame");
    assert!(lit.iter().all(|p| p[0] >= p[1] && p[0] >= p[2]), "red text: {:?}", &lit[..lit.len().min(8)]);
    assert!(lit.iter().any(|p| p[0] > 200 && p[1] < 40), "some pixel is fully red");
    assert_near(pixel(&frame, 0, 0), [0; 3], 4, "a corner stays black");
}

#[test]
fn the_same_text_is_rasterized_once_and_a_new_colour_again() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let mut compositor = text_compositor(&gpu);
    let red = parse(&project_json(&text_track(2, "[255,0,0,255]")));
    let first = text_frame(&mut compositor, &red);
    let after_first = compositor.text_rasterizations();
    // The scale it needs comes from measuring, so the text is drawn once, at that scale.
    assert_eq!(after_first, 1, "rasterized once");
    // A new renderer opens the clip again: the compositor still has the picture.
    let again = text_frame(&mut compositor, &red);
    assert_eq!(compositor.text_rasterizations(), after_first, "the same text is not rasterized again");
    assert_eq!(first, again);
    let green = text_frame(&mut compositor, &parse(&project_json(&text_track(2, "[0,255,0,255]"))));
    assert!(compositor.text_rasterizations() > after_first, "a new colour is rasterized again");
    assert_ne!(first, green);
}

#[test]
fn a_text_fading_in_is_part_way_clear_halfway_and_drawn_once() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let mut compositor = text_compositor(&gpu);
    let fade = 4 * FRAME;
    let track = format!(r#"{{"clips":[{{"source":"text:","start":0,"source_in":0,"duration":{},"text":{{"content":"HI","size":0.5,"color":[255,255,255,255]}},"animation":{{"in":{{"preset":"fade","duration":{fade}}}}}}}]}}"#, 8 * FRAME);
    let project = parse(&project_json(&track));
    let errors = validate(&project, &engine::AcceptAll);
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut renderer = FrameRenderer::default();
    let brightest: Vec<i32> = (0..=4)
        .map(|index| {
            let rgba = renderer
                .render_frame(&project, &mut compositor, index, |compositor, image| {
                    let mut rgba = Vec::new();
                    compositor.read(image, &mut rgba)?;
                    Ok(rgba)
                })
                .expect("frame");
            (0..HEIGHT).flat_map(|y| (0..WIDTH).map(move |x| (x, y))).map(|(x, y)| pixel(&rgba, x, y)[0]).max().unwrap()
        })
        .collect();
    assert!(brightest[0] <= 4, "clear at the start: {brightest:?}");
    assert!(brightest[4] >= 250, "opaque once the fade is over: {brightest:?}");
    assert!((brightest[2] - 128).abs() <= 10, "half way at the middle of the fade: {brightest:?}");
    assert!(brightest.windows(2).all(|pair| pair[1] > pair[0]), "{brightest:?}");
    assert_eq!(compositor.text_rasterizations(), 1, "the fade only changes how the picture is drawn");
}

/// A text clip zooming, spinning or popping in has no size at its first frame, and zooming out none
/// at the end: the frame stays black there, with no garbage from a zero-sized rectangle.
#[test]
fn a_text_with_no_size_yet_draws_nothing() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let mut compositor = text_compositor(&gpu);
    let mut renderer = FrameRenderer::default();
    for preset in ["zoom", "spin", "pop"] {
        let span = 4 * FRAME;
        let track = format!(r#"{{"clips":[{{"source":"text:","start":0,"source_in":0,"duration":{},"text":{{"content":"HI","size":0.5,"color":[255,255,255,255]}},"animation":{{"in":{{"preset":"{preset}","duration":{span}}},"out":{{"preset":"{preset}","duration":{span}}}}}}}]}}"#, 8 * FRAME);
        let project = parse(&project_json(&track));
        let errors = validate(&project, &engine::AcceptAll);
        assert!(errors.is_empty(), "invalid project: {errors:?}");
        let rgba = renderer
            .render_frame(&project, &mut compositor, 0, |compositor, image| {
                let mut rgba = Vec::new();
                compositor.read(image, &mut rgba)?;
                Ok(rgba)
            })
            .expect("frame");
        let brightest = (0..HEIGHT).flat_map(|y| (0..WIDTH).map(move |x| (x, y))).map(|(x, y)| pixel(&rgba, x, y).into_iter().max().unwrap()).max().unwrap();
        assert!(brightest <= 4, "{preset}: nothing drawn at the first frame, brightest {brightest}");
    }
}

#[test]
fn a_karaoke_text_reds_the_word_being_spoken_with_one_picture_per_step() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let mut compositor = text_compositor(&gpu);
    let karaoke = r#""karaoke":{"mode":"color","active":{"color":[255,0,0,255]}}"#;
    let track = format!(r#"{{"clips":[{{"source":"text:","start":0,"source_in":0,"duration":{},"text":{{"content":"HH HH","size":0.4,"color":[255,255,255,255],{karaoke}}}}}]}}"#, 8 * FRAME);
    let project = parse(&project_json(&track));
    let errors = validate(&project, &engine::AcceptAll);
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut renderer = FrameRenderer::default();
    // The columns with red ink at a frame: the first word speaks over frames 0 to 3, the second
    // over 4 to 7, as both have two characters.
    let mut red_columns = |index| -> Vec<u32> {
        let rgba = renderer
            .render_frame(&project, &mut compositor, index, |compositor, image| {
                let mut rgba = Vec::new();
                compositor.read(image, &mut rgba)?;
                Ok(rgba)
            })
            .expect("frame");
        (0..WIDTH).filter(|&x| (0..HEIGHT).any(|y| { let p = pixel(&rgba, x, y); p[0] > 150 && p[1] < 60 })).collect()
    };
    let first = red_columns(1);
    let second = red_columns(6);
    assert!(!first.is_empty() && !second.is_empty(), "{first:?} {second:?}");
    assert!(first.iter().max() < second.iter().min(), "the red moves right: {first:?} {second:?}");
    assert!(compositor.text_rasterizations() <= 4, "one picture per step: {}", compositor.text_rasterizations());
}
