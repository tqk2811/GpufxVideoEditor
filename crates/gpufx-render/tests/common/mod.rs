//! Helpers of the integration tests: test clips with burnt-in frame numbers and media tools.
//!
//! The clips are 640x360 at 30 fps. Row 0-23 holds a 16-cell binary counter of the frame number
//! (white = 1, most significant cell on the left); the rest of the picture is a flat colour.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const CLIP_WIDTH: u32 = 640;
pub const CLIP_HEIGHT: u32 = 360;
pub const CLIP_FRAMES: i64 = 180;
/// Ticks per clip frame (30 fps).
pub const FRAME: i64 = 3000;
/// Cell width of the frame-number bar.
pub const CELL: u32 = 40;
/// Flat colours of the two clips (grey, so colour matrices do not matter).
pub const GREY_A: i32 = 64;
pub const GREY_B: i32 = 160;

pub fn clips_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("clips")
}

pub fn packages_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter"))
}

/// The ffmpeg of third_party (LGPL): it has no libx264, but ffprobe and the bitstream filters are the same.
fn third_party_tool(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin")).join(name)
}

/// An ffmpeg that can encode H.264 with B-frames: the one on PATH if it has libx264, else
/// the one of third_party with libopenh264 (baseline profile, no B-frames).
fn encoder_tool() -> (PathBuf, &'static str, Vec<String>) {
    if let Ok(output) = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output() {
        if String::from_utf8_lossy(&output.stdout).contains("libx264") {
            return (PathBuf::from("ffmpeg"), "libx264", vec!["-bf".into(), "2".into(), "-b_strategy".into(), "0".into(), "-g".into(), "30".into(), "-sc_threshold".into(), "0".into(), "-crf".into(), "16".into()]);
        }
    }
    (third_party_tool("ffmpeg.exe"), "libopenh264", vec!["-b:v".into(), "4M".into(), "-g".into(), "30".into()])
}

fn bar_filter() -> String {
    let mut parts = vec![format!("format=yuv420p,drawbox=x=0:y=0:w={CLIP_WIDTH}:h=24:color=black:t=fill")];
    for cell in 0..16 {
        let shift = 15 - cell;
        parts.push(format!("drawbox=x={}:y=0:w={CELL}:h=24:color=white:t=fill:enable='mod(floor(n/pow(2,{shift})),2)'", cell * CELL));
    }
    parts.join(",")
}

fn generate_clip(path: &Path, grey: i32) {
    let (tool, codec, options) = encoder_tool();
    let color = format!("color=c=0x{grey:02x}{grey:02x}{grey:02x}:s={CLIP_WIDTH}x{CLIP_HEIGHT}:r=30:d=6");
    let status = Command::new(tool)
        .args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", &color, "-vf", &bar_filter(), "-c:v", codec, "-pix_fmt", "yuv420p"])
        .args(options)
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "generating {} failed", path.display());
}

/// Whether `path` has a B-frame, by the picture types ffprobe reports.
fn has_b_frames(path: &Path) -> bool {
    let output = Command::new(ffprobe_tool())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pict_type", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(output.status.success(), "ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).lines().any(|line| line.trim().starts_with('B'))
}

/// Generates the test clips once per run; returns the paths of clip A and clip B.
///
/// The file names carry the encoder, so clips cached by an earlier run with another encoder (a
/// different profile, with or without B-frames) are never reused. When the clips have no
/// B-frames, a warning says the frame-accuracy tests do not cover B-frame reordering.
pub fn test_clips() -> (PathBuf, PathBuf) {
    static CLIPS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    CLIPS
        .get_or_init(|| {
            let dir = clips_dir();
            std::fs::create_dir_all(&dir).expect("create the clips directory");
            let (_, codec, _) = encoder_tool();
            let (a, b) = (dir.join(format!("clip-a-{codec}.mp4")), dir.join(format!("clip-b-{codec}.mp4")));
            for (path, grey) in [(&a, GREY_A), (&b, GREY_B)] {
                if !path.is_file() {
                    generate_clip(path, grey);
                }
            }
            if !has_b_frames(&a) {
                eprintln!("WARNING: the test clips were made with {codec} and contain no B-frames, so the frame-accuracy tests do NOT cover B-frame (presentation order) decoding. Put an ffmpeg with libx264 on PATH.");
            }
            (a, b)
        })
        .clone()
}

/// A copy of clip A whose H.264 headers carry a non-zero left crop.
pub fn cropped_clip() -> PathBuf {
    let (a, _) = test_clips();
    let path = clips_dir().join(a.file_name().map(|n| format!("cropped-{}", n.to_string_lossy())).unwrap_or_else(|| "cropped.mp4".into()));
    if !path.is_file() {
        let status = Command::new(third_party_tool("ffmpeg.exe"))
            .args(["-hide_banner", "-y", "-v", "error", "-i"])
            .arg(&a)
            .args(["-c", "copy", "-bsf:v", "h264_metadata=crop_left=8"])
            .arg(&path)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "generating the cropped clip failed");
    }
    path
}

/// The frequency that sounds during second `second` of the stepped test tone.
pub fn stepped_tone_frequency(second: i64) -> f64 {
    300.0 + 200.0 * second as f64
}

/// The stepped test tone at `seconds`, matching [`STEPPED_TONE`].
pub fn stepped_tone(seconds: f64) -> f64 {
    0.5 * (2.0 * std::f64::consts::PI * stepped_tone_frequency(seconds.floor() as i64) * seconds).sin()
}

/// A lavfi expression: a sine whose frequency steps once per second.
pub const STEPPED_TONE: &str = "0.5*sin(2*PI*(300+200*floor(t))*t)";

/// Six seconds of the stepped tone at `rate`, in `channels`, written once per run to `name`.
pub fn stepped_tone_clip(name: &str, rate: u32, channels: u32, codec: &str) -> PathBuf {
    audio_test_file(name, &["-f", "lavfi", "-i", &format!("aevalsrc='{STEPPED_TONE}':s={rate}:d=6"), "-ac", &channels.to_string(), "-c:a", codec])
}

/// Runs the ffmpeg of third_party with `arguments` to make `name` in the clips directory, unless it exists.
pub fn audio_test_file(name: &str, arguments: &[&str]) -> PathBuf {
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).expect("create the clips directory");
    let path = dir.join(name);
    if !path.is_file() {
        // Named per process and call, so parallel tests never see a half-written clip.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let temporary = dir.join(format!("{}.{unique}.tmp.{}", path.file_stem().unwrap_or_default().to_string_lossy(), path.extension().unwrap_or_default().to_string_lossy()));
        let status = Command::new(ffmpeg_tool()).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(&temporary).status().expect("ffmpeg runs");
        assert!(status.success(), "generating {} failed", path.display());
        if std::fs::rename(&temporary, &path).is_err() {
            let _ = std::fs::remove_file(&temporary);
            assert!(path.is_file(), "publishing {} failed", path.display());
        }
    }
    path
}

/// Paths use forward slashes so they can be pasted into JSON text.
pub fn json_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Serializes the tests that drive the GPU and D3D12VA.
pub fn gpu_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn ffprobe_tool() -> PathBuf {
    third_party_tool("ffprobe.exe")
}

pub fn ffmpeg_tool() -> PathBuf {
    third_party_tool("ffmpeg.exe")
}

/// The bits of the frame-number bar in `rgba`, scaled for an image of `width`.
pub fn read_frame_number(rgba: &[u8], width: u32) -> i64 {
    let scale = width / CLIP_WIDTH;
    let mut number = 0i64;
    for cell in 0..16u32 {
        let (x, y) = ((cell * CELL + CELL / 2) * scale, 12 * scale);
        let value = rgba[((y * width + x) * 4) as usize];
        number = number << 1 | i64::from(value > 128);
    }
    number
}

/// The red channel at `(x, y)` of an RGBA image of `width`.
pub fn sample(rgba: &[u8], width: u32, x: u32, y: u32) -> i32 {
    i32::from(rgba[((y * width + x) * 4) as usize])
}
