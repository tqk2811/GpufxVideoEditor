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
