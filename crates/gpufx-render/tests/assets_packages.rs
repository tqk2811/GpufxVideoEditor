//! Every shader package of the editor's own assets (`external/GpufxVideoEditor-Assets/packages/gpufx`)
//! compiles, runs once with its default parameters on a small frame, and opens on the noop device.
//!
//! Skips without a GPU (fails instead with `GPUFX_REQUIRE_GPU`), and skips when the Assets submodule
//! is not checked out (fails instead with `GPUFX_REQUIRE_ASSETS`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpufx_core::{Backend, Effect, EffectGraph, FrameInfo, GpuContext, Image, ImageMut, Package};

/// Packages allowed to give an all-black picture from a gradient at their defaults, with the reason.
const MAY_BE_BLACK: &[(&str, &str)] = &[];

fn assets_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/GpufxVideoEditor-Assets/packages/gpufx"))
}

/// Pushes every package directory (one with a `manifest.json`) below `dir`, at any depth.
fn collect_packages(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.join("manifest.json").is_file() {
            out.push(path);
        } else if path.is_dir() {
            collect_packages(&path, out);
        }
    }
}

/// Every package of the Assets submodule, sorted, or `None` after a skip when there is none.
fn package_dirs() -> Option<Vec<PathBuf>> {
    let mut all = Vec::new();
    collect_packages(&assets_root(), &mut all);
    if all.is_empty() {
        assert!(std::env::var_os("GPUFX_REQUIRE_ASSETS").is_none(), "GPUFX_REQUIRE_ASSETS is set, but {} has no package", assets_root().display());
        eprintln!("assets_packages: skipped: the GpufxVideoEditor-Assets submodule has no package");
        return None;
    }
    all.sort();
    Some(all)
}

fn name(dir: &Path) -> String {
    dir.strip_prefix(assets_root()).unwrap_or(dir).to_string_lossy().replace('\\', "/")
}

fn gpu() -> Option<Arc<GpuContext>> {
    match GpuContext::new(Backend::Auto) {
        Ok(ctx) => {
            eprintln!("adapter: {}", ctx.description());
            Some(ctx)
        }
        Err(e) => {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {e}");
            eprintln!("assets_packages: skipped: {e}");
            None
        }
    }
}

fn gradient(w: u32, h: u32) -> Vec<u8> {
    (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 128, 255]).collect()
}

/// Runs `fx` alone at `time` on a gradient and returns its outputs, or the error text.
fn run(fx: Effect, w: u32, h: u32, time: f64, frame: u64) -> Result<Vec<Vec<u8>>, String> {
    let src = gradient(w, h);
    let inputs = vec![Image { data: &src, width: w, height: h, stride: w * 4 }; fx.input_count()];
    let mut fx = Some(fx);
    let mut graph = EffectGraph::parse("e0", |_| Ok(("fx".to_string(), fx.take().expect("one effect")))).map_err(|e| e.to_string())?;
    let pads = vec![(w, h); graph.input_count()];
    let sizes: Vec<(u32, u32)> = (0..graph.output_count()).map(|k| graph.output_size(k, &pads)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let mut outs: Vec<Vec<u8>> = sizes.iter().map(|&(ow, oh)| vec![0u8; (ow * oh * 4) as usize]).collect();
    let targets = outs.iter_mut().zip(&sizes).map(|(o, &(ow, oh))| ImageMut { data: o, width: ow, height: oh, stride: ow * 4 }).collect();
    graph.process(&inputs, targets, &FrameInfo::new(time, frame)).map_err(|e| e.to_string())?;
    Ok(outs)
}

#[test]
fn every_assets_package_compiles_and_runs() {
    let Some(dirs) = package_dirs() else { return };
    let Some(ctx) = gpu() else { return };
    eprintln!("{} packages", dirs.len());
    let (w, h) = (67, 41); // not a multiple of the workgroup size on purpose
    let mut failures = Vec::new();
    for dir in &dirs {
        let package = name(dir);
        let fx = match Package::open(dir).map_err(|e| e.to_string()).and_then(|p| Effect::new(ctx.clone(), p).map_err(|e| e.to_string())) {
            Ok(fx) => fx,
            Err(e) => {
                failures.push(format!("{package}: {e}"));
                continue;
            }
        };
        match run(fx, w, h, 0.5, 15) {
            Err(e) => failures.push(format!("{package}: {e}")),
            Ok(outs) => {
                let black = outs.iter().any(|out| out.chunks(4).all(|p| p[..3] == [0, 0, 0]));
                if black && !MAY_BE_BLACK.iter().any(|(n, _)| *n == package) {
                    failures.push(format!("{package}: an output is all black"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} packages failed:\n{}", failures.len(), dirs.len(), failures.join("\n"));
}

/// Effects are opened on the noop device until they get theirs, as libgpufx does.
#[test]
fn every_assets_package_opens_on_the_noop_device() {
    let Some(dirs) = package_dirs() else { return };
    let ctx = GpuContext::noop().expect("the noop backend is built in");
    let failures: Vec<String> = dirs
        .iter()
        .filter_map(|dir| Package::open(dir).map_err(|e| e.to_string()).and_then(|p| Effect::new(ctx.clone(), p).map(|_| ()).map_err(|e| e.to_string())).err().map(|e| format!("{}: {e}", name(dir))))
        .collect();
    assert!(failures.is_empty(), "{} of {} packages failed:\n{}", failures.len(), dirs.len(), failures.join("\n"));
}
