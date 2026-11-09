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
    match Gpu::create() {
        Ok(gpu) if gpu.adapter_name.contains("Microsoft Basic Render") => {
            eprintln!("skipping: only the WARP software adapter is available");
            false
        }
        Ok(_) => true,
        Err(message) => {
            eprintln!("skipping: {message}");
            false
        }
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
