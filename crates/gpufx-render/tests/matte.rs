//! AI background removal through the real decoder, model and compositor: the preview (frames
//! asked for one by one, in any order, on a fresh compositor) gives the export's pixels, with and
//! without temporal smoothing. Skips when the matting models are not built
//! (`external/FFmpegGpuFilter/scripts/convert-matting-models.py` into its `build/packages`).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use d3d12_backend::{D3d12Compositor, Gpu, ProjectPaths};
use engine::render::{render, FrameRenderer, FrameSink, RenderError};
use engine::{validate, MatteModel, Project};

const W: u32 = 160;
const H: u32 = 96;
const FRAMES: i64 = 8;

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

/// The FFmpegGpuFilter build folder whose `packages/gpufx_ai_matting` holds the built models.
fn models_root() -> PathBuf {
    packages_root().join("build")
}

fn model_built(model: MatteModel) -> bool {
    let built = d3d12_backend::find_matting_model(model, &[models_root()]).is_some();
    if !built {
        eprintln!("skipping: the {} model is not built", model.package_id().unwrap());
    }
    built
}

struct All(Vec<Vec<u8>>);

impl FrameSink for All {
    fn write_frame(&mut self, _index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        self.0.push(rgba.to_vec());
        Ok(())
    }
}

fn project(top: &Path, bottom: &Path, matte: &str) -> (Project, Arc<ProjectPaths>) {
    let clip = |source: &Path, extra: &str| format!(r#"{{"source":"{}","start":0,"source_in":{},"duration":{}{extra}}}"#, json_path(source), 10 * FRAME, FRAMES * FRAME);
    let json = format!(
        r#"{{"version":1,"output":{{"width":{W},"height":{H},"frame_duration":{FRAME}}},"tracks":[{{"clips":[{}]}},{{"clips":[{}]}}]}}"#,
        clip(bottom, ""),
        clip(top, &format!(r#","matte":{matte}"#))
    );
    let project = Project::from_json(&json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root(), models_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    (project, paths)
}

fn export(gpu: &Arc<Gpu>, project: &Project, paths: &Arc<ProjectPaths>) -> Vec<Vec<u8>> {
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths.clone(), W, H).expect("compositor");
    let mut all = All(Vec::new());
    render(project, &mut compositor, &mut all).unwrap_or_else(|e| panic!("export failed: {}", e.0));
    assert!(compositor.matte_problems().is_empty(), "{:?}", compositor.matte_problems());
    all.0
}

/// Frames in the order `order`, each through a fresh renderer as a seek in the preview would.
fn preview(gpu: &Arc<Gpu>, project: &Project, paths: &Arc<ProjectPaths>, order: &[i64]) -> Vec<(i64, Vec<u8>)> {
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths.clone(), W, H).expect("compositor");
    let mut renderer = FrameRenderer::default();
    order
        .iter()
        .map(|&index| {
            let rgba = renderer
                .render_frame(project, &mut compositor, index, |compositor, image| {
                    let mut rgba = Vec::new();
                    engine::render::Compositor::read(compositor, image, &mut rgba)?;
                    Ok(rgba)
                })
                .unwrap_or_else(|e| panic!("preview frame {index}: {}", e.0));
            (index, rgba)
        })
        .collect()
}

fn check(model: MatteModel, matte: &str) {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    if !model_built(model) {
        return;
    }
    let (top, bottom) = test_clips();
    let (project, paths) = project(&top, &bottom, matte);
    let exported = export(&gpu, &project, &paths);
    assert_eq!(exported.len(), FRAMES as usize);
    let (plain, _) = self::project(&top, &bottom, r#"{"model":"off"}"#);
    let unmatted = export(&gpu, &plain, &paths);
    assert_ne!(exported, unmatted, "the matte changes the picture");
    for (index, rgba) in preview(&gpu, &project, &paths, &[5, 2, 7, 0, 3, 6, 1, 4, 5]) {
        assert!(rgba == exported[index as usize], "preview frame {index} differs from the export");
    }
}

#[test]
fn a_fast_matte_previews_as_it_exports() {
    check(MatteModel::Fast, r#"{"model":"fast","feather":3.0,"edge":-2.0}"#);
}

#[test]
fn a_quality_matte_with_threshold_and_invert_previews_as_it_exports() {
    check(MatteModel::Quality, r#"{"model":"quality","threshold":0.5,"invert":true}"#);
}

#[test]
fn a_temporal_matte_does_not_depend_on_the_seek_order() {
    check(MatteModel::Fast, r#"{"model":"fast","temporal":3,"feather":1.0}"#);
}

#[test]
fn a_missing_model_draws_the_clip_without_its_matte() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let (top, bottom) = test_clips();
    let (project, _) = project(&top, &bottom, r#"{"model":"fast"}"#);
    let empty = std::env::temp_dir().join(format!("gpufx-no-models-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    if d3d12_backend::find_matting_model(MatteModel::Fast, &[packages_root()]).is_some() {
        eprintln!("skipping: a Fast model is installed outside the test folders");
        return;
    }
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths.clone(), W, H).expect("compositor");
    let mut all = All(Vec::new());
    render(&project, &mut compositor, &mut all).expect("renders without the matte");
    assert_eq!(compositor.matte_problems().len(), 1, "{:?}", compositor.matte_problems());
    let (plain, _) = self::project(&top, &bottom, r#"{"model":"off"}"#);
    assert_eq!(all.0, export(&gpu, &plain, &paths));
}

/// A 64x48 PNG of one colour at half alpha.
fn half_alpha_png() -> PathBuf {
    let path = clips_dir().join("matte-half-alpha.png");
    std::fs::create_dir_all(clips_dir()).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 64, 48);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let pixels: Vec<u8> = (0..64 * 48).flat_map(|_| [200u8, 120, 40, 128]).collect();
    encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
    path
}

/// A still with alpha whose matte keeps everything (threshold 1, inverted: a raw matte below 1
/// becomes 1) looks as it does unmatted: the cut-out keeps straight colours, nothing darkens.
/// Its matte is computed once for all frames (the still is cached by file).
#[test]
fn a_still_with_alpha_keeps_its_colours_and_its_matte_is_cached() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    if !model_built(MatteModel::Fast) {
        return;
    }
    let (_, bottom) = test_clips();
    let still = half_alpha_png();
    let (matted, paths) = project(&still, &bottom, r#"{"model":"fast","threshold":1.0,"invert":true}"#);
    let (plain, _) = project(&still, &bottom, r#"{"model":"off"}"#);
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths.clone(), W, H).expect("compositor");
    let mut all = All(Vec::new());
    render(&matted, &mut compositor, &mut all).unwrap_or_else(|e| panic!("export failed: {}", e.0));
    assert!(compositor.matte_problems().is_empty(), "{:?}", compositor.matte_problems());
    assert_eq!(compositor.matte_cache_usage().0, 1, "one matte for every frame of the still");
    let expected = export(&gpu, &plain, &paths);
    for (index, (got, want)) in all.0.iter().zip(&expected).enumerate() {
        let worst = got.iter().zip(want).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "frame {index}: the matted still differs by up to {worst}");
    }
}

/// A 30 fps clip in a 1/1000 time base, where frame steps are 33 or 34 ticks.
fn millisecond_clip() -> PathBuf {
    let path = clips_dir().join("matte-ms-timebase.mp4");
    if !path.is_file() {
        std::fs::create_dir_all(clips_dir()).unwrap();
        let ok = std::process::Command::new(ffmpeg_tool())
            .args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=160x96:rate=30", "-t", "2", "-pix_fmt", "yuv420p", "-c:v", "libopenh264", "-video_track_timescale", "1000"])
            .arg(&path)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok, "making the 1/1000 clip failed");
    }
    path
}

/// The temporal window follows the decoded frames, not timestamps computed from the frame rate:
/// in a 1/1000 time base every frame's matte is computed once, the window of each frame being
/// the frames really before it, and the preview still gives the export's pixels.
#[test]
fn a_temporal_window_in_a_millisecond_time_base_reuses_every_matte() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    if !model_built(MatteModel::Fast) {
        return;
    }
    let (_, bottom) = test_clips();
    let top = millisecond_clip();
    let (project, paths) = project(&top, &bottom, r#"{"model":"fast","temporal":3}"#);
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths.clone(), W, H).expect("compositor");
    let mut all = All(Vec::new());
    render(&project, &mut compositor, &mut all).unwrap_or_else(|e| panic!("export failed: {}", e.0));
    assert_eq!(compositor.matte_cache_usage().0, FRAMES as usize + 3, "each source frame once: the clip's frames and the three before");
    for (index, rgba) in preview(&gpu, &project, &paths, &[6, 1, 4, 0, 7]) {
        assert!(rgba == all.0[index as usize], "preview frame {index} differs from the export");
    }
}
