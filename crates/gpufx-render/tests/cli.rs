//! Runs the `gpufx-render` executable and checks the file it writes with ffprobe and ffmpeg.
//! GPU cases skip, with a message, when no Direct3D 12 hardware adapter is available.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::*;
use d3d12_backend::Gpu;

const CLIP_DURATION: i64 = 45 * FRAME;

fn scratch_dir(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test);
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

fn render(project: &PathBuf, out: &PathBuf, hashes: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gpufx-render")).arg(project).arg("--out").arg(out).arg("--hashes").arg(hashes).output().expect("run gpufx-render")
}

fn write_project(dir: &PathBuf, json: &str) -> PathBuf {
    let path = dir.join("project.json");
    std::fs::write(&path, json).expect("write the project file");
    path
}

fn two_clip_project() -> String {
    let (a, b) = test_clips();
    format!(
        r#"{{"version":1,"output":{{"width":{CLIP_WIDTH},"height":{CLIP_HEIGHT},"frame_duration":{FRAME}}},"tracks":[{{"clips":[
           {{"source":"{}","start":0,"source_in":0,"duration":{CLIP_DURATION}}},
           {{"source":"{}","start":{CLIP_DURATION},"source_in":0,"duration":{CLIP_DURATION}}}],
           "transitions":[{{"after_clip":0,"package":"mix","duration":{transition}}}]}}]}}"#,
        json_path(&a),
        json_path(&b),
        transition = 15 * FRAME,
    )
}

fn probe(args: &[&str], file: &PathBuf) -> String {
    let output = Command::new(ffprobe_tool()).args(args).arg(file).output().expect("run ffprobe");
    assert!(output.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn hardware_available() -> bool {
    match Gpu::create_hardware() {
        Ok(Some(_)) => true,
        Ok(None) => {
            eprintln!("skipping: no D3D12 hardware adapter is available");
            false
        }
        Err(message) => panic!("opening the GPU failed: {message}"),
    }
}

#[test]
fn renders_a_file_that_ffprobe_accepts_and_repeats_exactly() {
    let _lock = gpu_lock();
    if !hardware_available() {
        return;
    }
    let dir = scratch_dir("cli-output");
    let project = write_project(&dir, &two_clip_project());
    let (first_out, second_out) = (dir.join("first.mp4"), dir.join("second.mp4"));
    let (first_hashes, second_hashes) = (dir.join("first.txt"), dir.join("second.txt"));
    let first = render(&project, &first_out, &first_hashes);
    assert!(first.status.success(), "render failed: {}", String::from_utf8_lossy(&first.stderr));
    let printed = String::from_utf8_lossy(&first.stdout);
    let encoder = printed.lines().find_map(|l| l.strip_prefix("encoder: ")).expect("the encoder is reported").to_owned();
    println!("encoder used: {encoder}");
    let second = render(&project, &second_out, &second_hashes);
    assert!(second.status.success(), "second render failed: {}", String::from_utf8_lossy(&second.stderr));

    let first_text = std::fs::read_to_string(&first_hashes).expect("read hashes");
    assert_eq!(first_text.lines().count(), 90, "one hash per frame");
    assert_eq!(first_text, std::fs::read_to_string(&second_hashes).expect("read hashes"), "per-frame hashes differ between runs");

    assert_eq!(probe(&["-v", "error", "-select_streams", "v:0", "-count_frames", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"], &first_out), "90");
    assert_eq!(probe(&["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"], &first_out), "h264");
    assert_eq!(probe(&["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"], &first_out), format!("{CLIP_WIDTH},{CLIP_HEIGHT}"));

    // A full decode with no errors logged: the file is playable.
    let decode = Command::new(ffmpeg_tool()).args(["-hide_banner", "-v", "error", "-i"]).arg(&first_out).args(["-f", "null", "-"]).output().expect("run ffmpeg");
    assert!(decode.status.success() && decode.stderr.is_empty(), "decoding failed: {}", String::from_utf8_lossy(&decode.stderr));

    let raw = dir.join("first-frame.rgba");
    let extract = Command::new(ffmpeg_tool())
        .args(["-hide_banner", "-v", "error", "-y", "-i"])
        .arg(&first_out)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg(&raw)
        .status()
        .expect("run ffmpeg");
    assert!(extract.success());
    let rgba = std::fs::read(&raw).expect("read the extracted frame");
    assert_eq!(read_frame_number(&rgba, CLIP_WIDTH), 0, "the encoded first frame is frame 0");
}

fn invalid_project(tracks: &str) -> String {
    format!(r#"{{"version":1,"output":{{"width":64,"height":64,"frame_duration":3000}},"tracks":[{tracks}]}}"#)
}

fn expect_rejected(test: &str, tracks: &str, message: &str) {
    let dir = scratch_dir(test);
    let project = write_project(&dir, &invalid_project(tracks));
    let output = render(&project, &dir.join("out.mp4"), &dir.join("hashes.txt"));
    assert!(!output.status.success(), "the project must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "expected {message:?} in: {stderr}");
    assert!(stderr.contains("not valid"), "{stderr}");
}

#[test]
fn a_missing_source_is_rejected_with_its_location() {
    expect_rejected("reject-source", r#"{"clips":[{"source":"nope.mp4","start":0,"source_in":0,"duration":3000}]}"#, "track 0, clip 0: source 'nope.mp4' not found");
}

#[test]
fn a_missing_package_is_rejected_with_its_location() {
    let (a, b) = test_clips();
    let tracks = format!(
        r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":6000}},{{"source":"{}","start":6000,"source_in":0,"duration":6000}}],
            "transitions":[{{"after_clip":0,"package":"packages/gpufx/capcut/nope","duration":3000}}]}}"#,
        json_path(&a),
        json_path(&b)
    );
    expect_rejected("reject-package", &tracks, "track 0, transition 0: package 'packages/gpufx/capcut/nope' not found");
}

#[test]
fn overlapping_clips_are_rejected() {
    let (a, b) = test_clips();
    let tracks = format!(
        r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":6000}},{{"source":"{}","start":3000,"source_in":0,"duration":6000}}]}}"#,
        json_path(&a),
        json_path(&b)
    );
    expect_rejected("reject-overlap", &tracks, "track 0: clip 0 overlaps clip 1");
}

#[test]
fn a_transition_longer_than_the_clips_is_rejected() {
    let (a, b) = test_clips();
    let tracks = format!(
        r#"{{"clips":[{{"source":"{}","start":0,"source_in":0,"duration":3000}},{{"source":"{}","start":3000,"source_in":0,"duration":3000}}],
            "transitions":[{{"after_clip":0,"package":"mix","duration":9000}}]}}"#,
        json_path(&a),
        json_path(&b)
    );
    expect_rejected("reject-long-transition", &tracks, "track 0, transition 0: needs");
}

fn one_clip_project(source: &str) -> String {
    invalid_project(&format!(r#"{{"clips":[{{"source":"{source}","start":0,"source_in":0,"duration":9000}}]}}"#))
}

fn render_with(project: &PathBuf, out: &PathBuf, extra: &[&PathBuf]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gpufx-render"));
    command.arg(project).arg("--out").arg(out);
    for hashes in extra {
        command.arg("--hashes").arg(hashes);
    }
    command.output().expect("run gpufx-render")
}

fn expect_refused_and_intact(test: &str, out_of: impl Fn(&PathBuf) -> PathBuf, hashes_of: impl Fn(&PathBuf) -> Option<PathBuf>) {
    let dir = scratch_dir(test);
    let (clip, _) = test_clips();
    let source = dir.join("source.mp4");
    std::fs::copy(&clip, &source).expect("copy the clip");
    let before = std::fs::read(&source).expect("read the source");
    let project = write_project(&dir, &one_clip_project("source.mp4"));
    let other = dir.join("other.mp4");
    let _ = std::fs::remove_file(&other);
    let hashes = hashes_of(&source);
    let result = match &hashes {
        Some(h) => render_with(&project, &other, &[h]),
        None => render_with(&project, &out_of(&source), &[]),
    };
    assert!(!result.status.success(), "writing over a source must be refused");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("would be overwritten"), "{stderr}");
    assert_eq!(std::fs::read(&source).expect("read the source"), before, "the source was modified");
    assert!(!other.exists(), "nothing may be written when the arguments are refused");
}

#[test]
fn an_output_that_is_a_source_is_refused() {
    expect_refused_and_intact("refuse-out-source", |s| s.clone(), |_| None);
}

#[test]
fn an_output_spelled_differently_is_still_the_source() {
    expect_refused_and_intact("refuse-out-spelling", |s| s.parent().unwrap().join(".").join("SOURCE.MP4"), |_| None);
}

#[test]
fn a_hash_file_that_is_a_source_is_refused() {
    expect_refused_and_intact("refuse-hashes-source", |s| s.clone(), |s| Some(s.clone()));
}

#[test]
fn a_failed_render_leaves_existing_outputs_untouched() {
    let _lock = gpu_lock();
    if !hardware_available() {
        return;
    }
    let dir = scratch_dir("failed-render");
    // Passes validation (the file exists) but cannot be decoded, so the render fails after the encoder opened.
    std::fs::write(dir.join("broken.mp4"), b"this is not a video").expect("write the broken source");
    let project = write_project(&dir, &one_clip_project("broken.mp4"));
    let (out, hashes) = (dir.join("existing.mp4"), dir.join("existing.txt"));
    std::fs::write(&out, b"previous output").expect("write the existing output");
    std::fs::write(&hashes, b"previous hashes").expect("write the existing hashes");
    let result = render_with(&project, &out, &[&hashes]);
    assert!(!result.status.success(), "the render must fail");
    assert_eq!(std::fs::read(&out).expect("read the output"), b"previous output");
    assert_eq!(std::fs::read(&hashes).expect("read the hashes"), b"previous hashes");
    let leftovers: Vec<_> = std::fs::read_dir(&dir).expect("list the directory").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".tmp")).collect();
    assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
}
