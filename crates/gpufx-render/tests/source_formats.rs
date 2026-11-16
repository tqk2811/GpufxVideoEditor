//! Sources outside the plain D3D12VA path, through the real decoder and compositor: a stream whose
//! size changes midway, and 4:2:2 / 4:4:4 pictures in 8 and 10 bits, which decode in software (or
//! to a layout gpufx does not read) and are converted and uploaded. The 4:2:2 / 4:4:4 frames are
//! compared with what ffmpeg itself decodes them to.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::*;
use d3d12_backend::{D3d12Compositor, Gpu, ProjectPaths, ReverseDecoder, SequentialDecoder};
use engine::render::{render, Decoder, FrameSink, RenderError};
use engine::{validate, Project, Time};

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

/// Keeps the frames in `wanted`.
struct Capture {
    wanted: Vec<i64>,
    frames: Vec<(i64, Vec<u8>)>,
}

impl Capture {
    fn new(wanted: &[i64]) -> Self {
        Self { wanted: wanted.to_vec(), frames: Vec::new() }
    }

    fn frame(&self, index: i64) -> &[u8] {
        &self.frames.iter().find(|(i, _)| *i == index).unwrap_or_else(|| panic!("frame {index} was not captured")).1
    }
}

impl FrameSink for Capture {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        if self.wanted.contains(&index) {
            self.frames.push((index, rgba.to_vec()));
        }
        Ok(())
    }

    fn write_audio(&mut self, _samples: &[f32]) -> Result<(), RenderError> {
        Ok(())
    }
}

/// Renders one track of `clips` (JSON objects) at `width` x `height`, keeping the frames `wanted`.
fn render_clips(gpu: &Arc<Gpu>, clips: &str, width: u32, height: u32, wanted: &[i64]) -> Capture {
    let json = format!(r#"{{"version":1,"output":{{"width":{width},"height":{height},"frame_duration":{FRAME}}},"tracks":[{{"clips":[{clips}]}}]}}"#);
    let project = Project::from_json(&json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, width, height).expect("compositor");
    let mut capture = Capture::new(wanted);
    render(&project, &mut compositor, &mut capture).unwrap_or_else(|e| panic!("rendering failed: {}", e.0));
    capture
}

fn clip_json(source: &Path, source_in: i64, duration: i64, reverse: bool) -> String {
    format!(r#"{{"source":"{}","start":0,"source_in":{},"duration":{},"reverse":{reverse},"effects":[]}}"#, json_path(source), source_in * FRAME, duration * FRAME)
}

fn at(frame: i64) -> Time {
    Time::from_ticks(frame * FRAME)
}

/// Checks a rendered frame of the resolution change clip at 640x360: the first part fills the
/// frame and shows its number, the 4:3 second part is grey B between black side bars.
fn check_resized_frame(rgba: &[u8], source_frame: i64, what: &str) {
    let (w, h) = (CLIP_WIDTH, CLIP_HEIGHT);
    if source_frame < RESIZED_PART_FRAMES {
        assert_eq!(read_frame_number(rgba, w), source_frame, "{what}: the number of the first part");
        assert!((sample(rgba, w, w - 10, h / 2) - GREY_A).abs() <= 6, "{what}: the first part reaches the right edge");
    } else {
        // 480x360 shown 360 high: 480 wide, so 80 columns of bar on each side.
        assert!((sample(rgba, w, w / 2, h / 2) - GREY_B).abs() <= 6, "{what}: grey B in the middle, got {}", sample(rgba, w, w / 2, h / 2));
        assert!((sample(rgba, w, 100, h / 2) - GREY_B).abs() <= 6, "{what}: grey B inside the 4:3 picture");
        assert!(sample(rgba, w, 40, h / 2) <= 4 && sample(rgba, w, w - 40, h / 2) <= 4, "{what}: black bars beside the 4:3 picture");
        assert!(sample(rgba, w, w / 2, 12) > 100, "{what}: no frame-number bar of the first part");
    }
}

#[test]
fn a_source_whose_size_changes_midway_renders_both_sizes_in_their_own_shape() {
    let _lock = gpu_lock();
    let clip = resolution_change_clip();
    let Some(gpu) = gpu() else { return };
    let total = 2 * RESIZED_PART_FRAMES;
    let wanted: Vec<i64> = (0..total).collect();
    let capture = render_clips(&gpu, &clip_json(&clip, 0, total, false), CLIP_WIDTH, CLIP_HEIGHT, &wanted);
    for frame in [0, 1, RESIZED_PART_FRAMES - 1, RESIZED_PART_FRAMES, RESIZED_PART_FRAMES + 1, total - 1] {
        check_resized_frame(capture.frame(frame), frame, &format!("output frame {frame}"));
    }
}

#[test]
fn a_reversed_source_whose_size_changes_midway_plays_back_through_the_change() {
    let _lock = gpu_lock();
    let clip = resolution_change_clip();
    let Some(gpu) = gpu() else { return };
    // From the last frame of the second part back into the first.
    let (source_in, duration) = (RESIZED_PART_FRAMES - 30, 60);
    let wanted: Vec<i64> = (0..duration).collect();
    let capture = render_clips(&gpu, &clip_json(&clip, source_in, duration, true), CLIP_WIDTH, CLIP_HEIGHT, &wanted);
    for frame in [0, 29, 30, 31, duration - 1] {
        check_resized_frame(capture.frame(frame), source_in + duration - 1 - frame, &format!("reversed output frame {frame}"));
    }
}

#[test]
fn the_decoders_give_each_frame_the_size_it_was_coded_at_after_seeks_both_ways() {
    let _lock = gpu_lock();
    let clip = resolution_change_clip();
    let Some(gpu) = gpu() else { return };
    let size = |frame: &d3d12_backend::DecodedFrame| (frame.width(), frame.height());
    let first = (CLIP_WIDTH, CLIP_HEIGHT);
    let second = (RESIZED_WIDTH, RESIZED_HEIGHT);
    let mut forward = SequentialDecoder::open(&clip, gpu.raw_device()).expect("open the clip");
    for (frame, expected) in [(0, first), (RESIZED_PART_FRAMES + 10, second), (5, first), (RESIZED_PART_FRAMES + 70, second), (RESIZED_PART_FRAMES - 1, first), (RESIZED_PART_FRAMES, second)] {
        assert_eq!(size(&forward.frame_at(at(frame)).expect("a frame")), expected, "forward, frame {frame}");
    }
    // A reversed decoder with a small cache, so frames of both sizes go to system memory and back.
    let mut reverse = ReverseDecoder::open_with_cache(&clip, gpu.raw_device(), 40 * 640 * 384 * 3 / 2).expect("open the clip");
    let mut forward = SequentialDecoder::open(&clip, gpu.raw_device()).expect("open the clip");
    for frame in (0..2 * RESIZED_PART_FRAMES).rev() {
        let expected = if frame < RESIZED_PART_FRAMES { first } else { second };
        let got = reverse.frame_at(Time::from_ticks(frame * FRAME + FRAME / 3)).expect("a frame");
        assert_eq!(size(&got), expected, "reverse, frame {frame}");
        assert_eq!(got.pts, forward.frame_at(Time::from_ticks(frame * FRAME + FRAME / 3)).expect("a frame").pts, "reverse, frame {frame} is the forward decoder's");
    }
    assert!(reverse.downloaded_frames() > 0, "the cache was used");
}

/// One 4:2:2 or 4:4:4 test source.
struct FormatCase {
    name: &'static str,
    /// The encoder and its options, after `-c:v`.
    encoder: &'static [&'static str],
    pix_fmt: &'static str,
    extension: &'static str,
    /// Whether it needs a GPL ffmpeg (libx264, libx265) instead of the LGPL one of third_party.
    needs_gpl: bool,
}

const SIZE: (u32, u32) = (640, 360);

/// SMPTE bars, big flat patches of saturated colour where chroma errors show, tagged BT.709.
fn format_clip(case: &FormatCase) -> Option<PathBuf> {
    let tool = if case.needs_gpl {
        // The GPL build of third_party (scripts/setup-poc.ps1), else the ffmpeg on PATH.
        let bundled = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg-gpl/bin/ffmpeg.exe"));
        let tool = if bundled.is_file() { bundled } else { PathBuf::from("ffmpeg") };
        let has_encoder = Command::new(&tool).args(["-hide_banner", "-encoders"]).output().is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(case.encoder[0]));
        if !has_encoder {
            eprintln!("skipping {}: no GPL ffmpeg with {} (third_party/ffmpeg-gpl or PATH)", case.name, case.encoder[0]);
            return None;
        }
        tool
    } else {
        ffmpeg_tool()
    };
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).expect("create the clips directory");
    let path = dir.join(format!("format-{}.{}", case.name, case.extension));
    if !path.is_file() {
        let temporary = dir.join(format!("format-{}.{}.tmp.{}", case.name, std::process::id(), case.extension));
        let source = format!("smptehdbars=s={}x{}:r=30:d=1", SIZE.0, SIZE.1);
        let status = Command::new(tool)
            .args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", &source, "-vf", &format!("format={}", case.pix_fmt), "-c:v"])
            .args(case.encoder)
            .args(["-pix_fmt", case.pix_fmt, "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"])
            .arg(&temporary)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "generating {} failed", path.display());
        if std::fs::rename(&temporary, &path).is_err() {
            let _ = std::fs::remove_file(&temporary);
            assert!(path.is_file(), "publishing {} failed", path.display());
        }
    }
    Some(path)
}

/// Frame 0 of `clip` as RGB decoded by ffmpeg, with the colour description of the stream.
fn reference_rgb(clip: &Path) -> Vec<u8> {
    let output = Command::new(ffmpeg_tool())
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-vf", "scale=flags=accurate_rnd+full_chroma_int", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(output.status.success(), "decoding {} with ffmpeg failed: {}", clip.display(), String::from_utf8_lossy(&output.stderr));
    output.stdout
}

/// The mean absolute difference per channel between `rgba` and `rgb`, and the largest difference
/// at the middle of each of the seven top bars.
fn compare(rgba: &[u8], rgb: &[u8]) -> (f64, i32) {
    let (w, h) = SIZE;
    assert_eq!(rgb.len(), (w * h * 3) as usize, "the reference has the clip's size");
    let mut total = 0u64;
    for (pixel, reference) in rgba.chunks_exact(4).zip(rgb.chunks_exact(3)) {
        for c in 0..3 {
            total += u64::from(pixel[c].abs_diff(reference[c]));
        }
    }
    let mean = total as f64 / f64::from(w * h * 3);
    let mut worst = 0;
    for bar in 0..7 {
        let (x, y) = (w * (2 * bar + 1) / 14, h / 4);
        for c in 0..3 {
            let (a, b) = (rgba[((y * w + x) * 4 + c) as usize], rgb[((y * w + x) * 3 + c) as usize]);
            worst = worst.max(i32::from(a.abs_diff(b)));
        }
    }
    (mean, worst)
}

fn check_format(case: &FormatCase) {
    let _lock = gpu_lock();
    let Some(clip) = format_clip(case) else { return };
    let Some(gpu) = gpu() else { return };
    let capture = render_clips(&gpu, &clip_json(&clip, 0, 3, false), SIZE.0, SIZE.1, &[1]);
    let (mean, worst) = compare(capture.frame(1), &reference_rgb(&clip));
    eprintln!("{}: mean difference {mean:.2}, worst bar middle {worst}", case.name);
    assert!(mean <= 3.0, "{}: mean difference {mean:.2} from ffmpeg's decode", case.name);
    assert!(worst <= 6, "{}: a bar's colour differs by {worst} from ffmpeg's decode", case.name);
    // The decoders still seek and play backwards over such a source.
    let mut reverse = ReverseDecoder::open(&clip, gpu.raw_device()).expect("open the clip");
    let mut forward = SequentialDecoder::open(&clip, gpu.raw_device()).expect("open the clip");
    // Times inside a frame: a reversed decoder floors and a forward one rounds, which differ at a
    // frame's start in Matroska's 1/1000 time base.
    let mid = |frame: i64| Time::from_ticks(frame * FRAME + FRAME / 3);
    for frame in (0..30).rev() {
        assert_eq!(reverse.frame_at(mid(frame)).expect("a frame").pts, forward.frame_at(mid(frame)).expect("a frame").pts, "{}: frame {frame}", case.name);
    }
}

#[test]
fn prores_422_10_bit_matches_ffmpeg() {
    check_format(&FormatCase { name: "prores-422-10", encoder: &["prores_ks", "-profile:v", "3"], pix_fmt: "yuv422p10le", extension: "mov", needs_gpl: false });
}

#[test]
fn prores_4444_10_bit_matches_ffmpeg() {
    check_format(&FormatCase { name: "prores-4444-10", encoder: &["prores_ks", "-profile:v", "4"], pix_fmt: "yuv444p10le", extension: "mov", needs_gpl: false });
}

#[test]
fn ffv1_444_8_bit_matches_ffmpeg() {
    check_format(&FormatCase { name: "ffv1-444-8", encoder: &["ffv1"], pix_fmt: "yuv444p", extension: "mkv", needs_gpl: false });
}

#[test]
fn full_range_mjpeg_422_matches_ffmpeg() {
    check_format(&FormatCase { name: "mjpeg-422-full", encoder: &["mjpeg", "-q:v", "2"], pix_fmt: "yuvj422p", extension: "avi", needs_gpl: false });
}

#[test]
fn h264_422_10_bit_matches_ffmpeg() {
    check_format(&FormatCase { name: "h264-422-10", encoder: &["libx264", "-crf", "12"], pix_fmt: "yuv422p10le", extension: "mp4", needs_gpl: true });
}

#[test]
fn h264_444_8_bit_matches_ffmpeg() {
    check_format(&FormatCase { name: "h264-444-8", encoder: &["libx264", "-crf", "12"], pix_fmt: "yuv444p", extension: "mp4", needs_gpl: true });
}

#[test]
fn hevc_422_10_bit_matches_ffmpeg() {
    // A GPU with HEVC 4:2:2 decoding gives Y210 surfaces, which are downloaded and converted.
    check_format(&FormatCase { name: "hevc-422-10", encoder: &["libx265", "-crf", "12", "-x265-params", "log-level=error"], pix_fmt: "yuv422p10le", extension: "mp4", needs_gpl: true });
}

#[test]
fn hevc_444_8_bit_matches_ffmpeg() {
    check_format(&FormatCase { name: "hevc-444-8", encoder: &["libx265", "-crf", "12", "-x265-params", "log-level=error"], pix_fmt: "yuv444p", extension: "mp4", needs_gpl: true });
}

#[test]
fn an_odd_sized_software_source_is_not_stretched_to_its_even_surface() {
    let _lock = gpu_lock();
    // Grey stripes 1 to 5 pixels wide across a 641x361 picture: luma only, so halving the chroma
    // changes nothing, and stretching to 642x362 moves the edges on the right and bottom.
    let (w, h) = (641u32, 361u32);
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).expect("create the clips directory");
    let clip = dir.join("format-ffv1-444-odd.mkv");
    if !clip.is_file() {
        let temporary = dir.join(format!("format-ffv1-444-odd.{}.tmp.mkv", std::process::id()));
        let source = format!(r"nullsrc=s={w}x{h}:r=30:d=1,geq=lum='if(lt(mod(X+Y\,7)\,3)\,220\,30)':cb=128:cr=128");
        let status = Command::new(ffmpeg_tool())
            .args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", &source, "-c:v", "ffv1", "-pix_fmt", "yuv444p", "-colorspace", "bt709"])
            .arg(&temporary)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "generating {} failed", clip.display());
        if std::fs::rename(&temporary, &clip).is_err() {
            let _ = std::fs::remove_file(&temporary);
            assert!(clip.is_file(), "publishing {} failed", clip.display());
        }
    }
    let Some(gpu) = gpu() else { return };
    let reference = reference_rgb(&clip);
    assert_eq!(reference.len(), (w * h * 3) as usize);
    let capture = render_clips(&gpu, &clip_json(&clip, 0, 3, false), w, h, &[1]);
    let rgba = capture.frame(1);
    let wrong = rgba.chunks_exact(4).zip(reference.chunks_exact(3)).filter(|(a, b)| a[0].abs_diff(b[0]) > 40).count();
    assert!(wrong < 50, "{wrong} pixels differ from ffmpeg's decode");
}
