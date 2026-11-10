//! Runs `gpufx-render --effect-previews` for real. Skips, with a message, on a machine without a hardware
//! GPU or without the packages; `GPUFX_REQUIRE_GPU` turns the skip into a failure.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use d3d12_backend::Gpu;
use serde_json::{json, Value};

fn package(name: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter/packages/gpufx")).join(name)
}

fn skip(reason: &str) {
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {reason}");
    eprintln!("effect_previews: skipped: {reason}");
}

fn png_size(path: &Path) -> (u32, u32) {
    let reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(path).expect("the file"))).read_info().expect("a PNG");
    (reader.info().width, reader.info().height)
}

#[test]
fn other_flags_are_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_gpufx-render")).args(["--effect-previews", "--out", "x.mp4"]).output().expect("run");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn the_helper_renders_jobs_and_exits_at_the_end_of_stdin() {
    if !cfg!(windows) || Gpu::create_hardware().expect("device").is_none() {
        return skip("no hardware adapter");
    }
    if !package("ffmpeg/invert/manifest.json").is_file() {
        return skip("the FFmpegGpuFilter submodule has no packages");
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("effect_previews");
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let (still, looped) = (dir.join("invert-still.png"), dir.join("invert-loop.png"));
    let _ = std::fs::remove_file(&still);
    let _ = std::fs::remove_file(&looped);

    let mut child = Command::new(env!("CARGO_BIN_EXE_gpufx-render")).arg("--effect-previews").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    let mut next = || -> Value { serde_json::from_str(&lines.next().expect("an event").expect("a line")).expect("JSON") };

    let ready = next();
    assert_eq!(ready["event"], "ready", "{ready}");
    assert_eq!(ready["protocol"], 1);
    assert!(!ready["device"].as_str().expect("device").is_empty());

    let package_path = package("ffmpeg/invert").to_string_lossy().into_owned();
    writeln!(stdin, "{}", json!({"id": 1, "kind": "still", "package": package_path, "out": still.to_string_lossy()})).expect("write");
    assert_eq!(next(), json!({"event": "done", "id": 1, "static": false}));
    writeln!(stdin, "{}", json!({"id": 2, "kind": "loop", "package": package_path, "out": looped.to_string_lossy()})).expect("write");
    assert_eq!(next(), json!({"event": "done", "id": 2, "static": true}), "invert does not change over time");
    writeln!(stdin, "{}", json!({"id": 3, "kind": "still", "package": dir.join("no-such-package").to_string_lossy(), "out": still.to_string_lossy()})).expect("write");
    let failed = next();
    assert_eq!((failed["event"].as_str(), failed["id"].as_u64()), (Some("failed"), Some(3)), "{failed}");

    assert_eq!(png_size(&still), (144, 144));
    assert_eq!(png_size(&looped), (144, 144));

    drop(stdin);
    let status = child.wait().expect("wait");
    assert!(status.success(), "the end of stdin is a normal exit: {status}");
}
