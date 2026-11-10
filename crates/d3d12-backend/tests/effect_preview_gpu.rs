//! Effect previews on a real GPU, with real packages. Skips on a machine without a hardware adapter.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use d3d12_backend::{preview_image_files, Gpu, PreviewError, PreviewRenderer, PREVIEW_FRAMES, PREVIEW_SIZE};

fn package(name: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter/packages/gpufx")).join(name)
}

/// One test at a time on the GPU.
fn gpu_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Reports why a test cannot run. Fails instead when the environment says the GPU must be there.
fn skip(reason: &str) {
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {reason}");
    eprintln!("effect_preview_gpu: skipped: {reason}");
}

/// A renderer on a machine with a hardware adapter and the packages, or `None` after a skip.
fn renderer() -> Option<PreviewRenderer> {
    if Gpu::create_hardware().expect("device").is_none() {
        skip("no hardware adapter");
        return None;
    }
    if !package("ffmpeg/fade/manifest.json").is_file() {
        skip("the FFmpegGpuFilter submodule has no packages");
        return None;
    }
    Some(PreviewRenderer::new().expect("renderer"))
}

fn decode(file: &[u8]) -> Vec<u8> {
    let decoder = png::Decoder::new(std::io::Cursor::new(file));
    let mut reader = decoder.read_info().expect("header");
    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buffer).expect("frame");
    buffer[..info.buffer_size()].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
}

/// The mean absolute difference of the colour channels, 0 to 255.
fn difference(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let total: u64 = a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(x, y)| (0..3).map(|c| u64::from(x[c].abs_diff(y[c]))).sum::<u64>()).sum();
    total as f64 / (a.len() / 4 * 3) as f64
}

fn side() -> usize {
    (PREVIEW_SIZE * PREVIEW_SIZE * 4) as usize
}

#[test]
fn an_effect_still_differs_from_its_input() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    let still = renderer.render_still(&package("ffmpeg/invert")).expect("still");
    assert_eq!(still.len(), side());
    let [a, _] = preview_image_files();
    assert!(difference(&still, &decode(a)) > 10.0);
}

#[test]
fn a_fade_uses_a_progress_of_one_half() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    let still = renderer.render_still(&package("ffmpeg/fade")).expect("still");
    let input = decode(preview_image_files()[0]);
    // Fading to black: half way, every channel is about half its input.
    let (a, b): (u64, u64) = (still.iter().map(|&v| u64::from(v)).sum(), input.iter().map(|&v| u64::from(v)).sum());
    let alpha = (side() / 4) as u64 * 255 * 2;
    let ratio = (a - alpha / 2) as f64 / (b - alpha / 2) as f64;
    assert!((ratio - 0.5).abs() < 0.05, "ratio {ratio}");
}

#[test]
fn a_fade_loop_plays_its_progress() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    let preview = renderer.render_loop(&package("ffmpeg/fade")).expect("loop");
    assert!(!preview.is_static);
    assert_eq!(preview.frames.len(), PREVIEW_FRAMES as usize);
    let input = decode(preview_image_files()[0]);
    assert!(difference(&preview.frames[0], &input) < 2.0);
    assert!(difference(&preview.frames[PREVIEW_FRAMES as usize - 1], &input) > 10.0);
}

#[test]
fn a_transition_loop_goes_from_a_to_b() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    let preview = renderer.render_loop(&package("ffmpeg/dissolve")).expect("loop");
    assert!(!preview.is_static);
    assert_eq!(preview.frames.len(), PREVIEW_FRAMES as usize);
    assert!(preview.frames.iter().all(|frame| frame.len() == side()));
    let [a, b] = preview_image_files().map(decode);
    let last = &preview.frames[PREVIEW_FRAMES as usize - 1];
    assert!(difference(&preview.frames[0], &a) < 2.0, "first {}", difference(&preview.frames[0], &a));
    assert!(difference(last, &b) < 2.0, "last {}", difference(last, &b));
    let middle = renderer.render_still(&package("ffmpeg/dissolve")).expect("still");
    assert!(difference(&middle, &a) > 5.0 && difference(&middle, &b) > 5.0);
}

#[test]
fn an_effect_that_ignores_time_has_one_frame() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    let preview = renderer.render_loop(&package("ffmpeg/invert")).expect("loop");
    assert!(preview.is_static);
    assert_eq!(preview.frames.len(), 1);
    assert_eq!(preview.frames[0], renderer.render_still(&package("ffmpeg/invert")).expect("still"));
}

#[test]
fn an_effect_that_changes_over_time_has_every_frame() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    let preview = renderer.render_loop(&package("standalone/rgb-split")).expect("loop");
    assert!(!preview.is_static);
    assert_eq!(preview.frames.len(), PREVIEW_FRAMES as usize);
    assert_ne!(preview.frames[0], preview.frames[12]);
}

#[test]
fn a_bad_package_is_an_error_and_the_renderer_goes_on() {
    let _gpu = gpu_lock();
    let Some(mut renderer) = renderer() else { return };
    // A file that cannot be read is not the package's fault: it may be locked while it is installed.
    let missing = renderer.render_still(&package("ffmpeg/no-such-package"));
    assert!(matches!(missing, Err(PreviewError::Device(_))), "{missing:?}");

    let directory = std::env::temp_dir().join(format!("gpufx-bad-package-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("directory");
    std::fs::write(directory.join("manifest.json"), "{ not json").expect("manifest");
    let broken = renderer.render_loop(&directory);
    std::fs::remove_dir_all(&directory).expect("clean up");
    assert!(matches!(broken, Err(PreviewError::Package(_))), "{broken:?}");

    renderer.render_still(&package("ffmpeg/invert")).expect("still after failures");
}
