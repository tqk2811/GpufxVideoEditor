//! The real `gpufx-render --effect-previews` helper, driven by the editor's session, on a real
//! package. Skips on a machine without a hardware adapter.

#![cfg(windows)]

use std::path::{Path, PathBuf};

use d3d12_backend::{Gpu, PreviewJobKind};
use gpufx_editor::effect_previews::child::{PreviewRequest, PreviewSession, ProcessLauncher, RenderOutcome, SessionConfig};
use gpufx_editor::effect_previews::{package_digest, package_prefix, preview_key, PreviewSpec, PreviewStore};

const PACKAGE: &str = "ffmpeg/invert";

fn package_dir() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter/packages/gpufx")).join(PACKAGE)
}

/// Reports why a test cannot run. Fails instead when the environment says the GPU must be there.
fn skip(reason: &str) {
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {reason}");
    eprintln!("effect_preview_helper: skipped: {reason}");
}

/// `gpufx-render.exe` of the same profile, which `cargo build -p gpufx-render` puts above `deps`.
fn helper_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let found = exe.parent()?.parent()?.join("gpufx-render.exe");
    found.is_file().then_some(found)
}

#[test]
fn the_real_helper_renders_a_still_that_ends_up_in_the_store() {
    if Gpu::create_hardware().expect("device").is_none() {
        return skip("no hardware adapter");
    }
    if !package_dir().join("manifest.json").is_file() {
        return skip("the FFmpegGpuFilter submodule has no packages");
    }
    let Some(exe) = helper_exe() else { return skip("gpufx-render.exe is not built; run cargo build -p gpufx-render") };

    let root = std::env::temp_dir().join(format!("gpufx-helper-test-{}", std::process::id())).join("previews");
    let _ = std::fs::remove_dir_all(&root);
    let spec = PreviewSpec::STANDARD;
    let digest = package_digest(&package_dir()).expect("digest");
    let key = preview_key(&digest, &[], spec);
    let request = PreviewRequest { package_dir: package_dir(), package_prefix: package_prefix(PACKAGE), key: key.clone(), kind: PreviewJobKind::Still, spec };

    let mut session = PreviewSession::new(Box::new(ProcessLauncher::new(exe)), PreviewStore::new(root.clone()), SessionConfig::new());
    let outcome = session.render(&request);
    assert!(matches!(outcome, RenderOutcome::Published { .. }), "{outcome:?}");
    assert!(session.device().is_some_and(|device| !device.is_empty()));

    let store = PreviewStore::new(root.clone());
    let image = store.read_still(&request.package_prefix, &key, spec).expect("read").expect("the still is in the store");
    assert_eq!((image.width, image.height), (spec.width, spec.height));
    assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0), "the preview is not empty");
    let leftovers: Vec<_> = std::fs::read_dir(&root).expect("list").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).filter(|name| name.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // The same helper takes a second job, and a stopped session leaves nothing running.
    let again = session.render(&request);
    assert!(matches!(again, RenderOutcome::Published { .. }), "{again:?}");
    session.shutdown();
    let _ = std::fs::remove_dir_all(root.parent().expect("parent"));
}
