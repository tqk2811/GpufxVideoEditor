//! Phone video through the real decoder and compositor: 10-bit HDR (PQ, HLG) tone mapped to SDR,
//! and streams whose display matrix turns them upright. The clips are made at test time with the
//! ffmpeg of third_party. The 10-bit tests skip, with a message, when that ffmpeg has none of the
//! 10-bit encoders tried; the rotation test uses 8-bit NV12 clips and always runs on a GPU.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use common::*;
use d3d12_backend::{D3d12Compositor, FrameSampler, Gpu, ProjectPaths};
use engine::render::{render, FrameSink, RenderError};
use engine::{validate, Project, Time};
use gpufx_core::{hdr, Primaries, Transfer};

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

/// Keeps the first frame.
struct First(Option<Vec<u8>>);

impl FrameSink for First {
    fn write_frame(&mut self, _index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        self.0.get_or_insert_with(|| rgba.to_vec());
        Ok(())
    }

    fn write_audio(&mut self, _samples: &[f32]) -> Result<(), RenderError> {
        Ok(())
    }
}

/// The first frame of a project of one clip of `source`, `width` x `height`.
fn render_first(gpu: &Arc<Gpu>, source: &Path, width: u32, height: u32) -> Vec<u8> {
    let clip = format!(r#"{{"source":"{}","start":0,"source_in":0,"duration":{},"effects":[]}}"#, json_path(source), 3 * FRAME);
    let json = format!(r#"{{"version":1,"output":{{"width":{width},"height":{height},"frame_rate":{{"num":90000,"den":{FRAME}}}}},"tracks":[{{"clips":[{clip}]}}]}}"#);
    let project = Project::from_json(&json).expect("project parses");
    let paths = Arc::new(ProjectPaths::new(Path::new(&format!("{}/test.json", clips_dir().display())), vec![packages_root()]));
    let errors = validate(&project, paths.as_ref());
    assert!(errors.is_empty(), "invalid project: {errors:?}");
    let mut compositor = D3d12Compositor::new(gpu.clone(), paths, width, height).expect("compositor");
    let mut first = First(None);
    render(&project, &mut compositor, &mut first).unwrap_or_else(|e| panic!("rendering {} failed: {}", source.display(), e.0));
    first.0.expect("a frame")
}

fn run_ffmpeg(arguments: &[&str], output: &Path) -> bool {
    Command::new(ffmpeg_tool()).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(output).status().expect("ffmpeg runs").success()
}

/// Clip size of the 10-bit clips: grey on the left half, a saturated orange on the right.
const HDR_W: u32 = 256;
const HDR_H: u32 = 144;

/// The 10-bit encoders tried, in order: software AV1 first (no GPU needed to encode), then NVENC.
const TEN_BIT_ENCODERS: [(&str, &[&str]); 3] = [
    ("libsvtav1", &["-preset", "10", "-crf", "10"]),
    ("libaom-av1", &["-cpu-used", "8", "-crf", "10"]),
    ("hevc_nvenc", &["-profile:v", "main10", "-qp", "8", "-tag:v", "hvc1"]),
];

/// A 10-bit 4:2:0 clip tagged with `trc`, BT.2020 primaries and matrix; `None` when no encoder works.
fn ten_bit_clip(trc: &str) -> Option<PathBuf> {
    static ENCODER: OnceLock<Option<usize>> = OnceLock::new();
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).expect("create the clips directory");
    let make = |index: usize| -> Option<PathBuf> {
        let (codec, options) = TEN_BIT_ENCODERS[index];
        let path = dir.join(format!("ten-bit-{trc}-{codec}.mp4"));
        if path.is_file() {
            return Some(path);
        }
        let source = format!("color=c=0x808080:s={}x{}:r=30:d=0.5[g];color=c=0xe06020:s={}x{}:r=30:d=0.5[o];[g][o]hstack", HDR_W / 2, HDR_H, HDR_W / 2, HDR_H);
        // Tagged on the frames: some encoders (libsvtav1) ignore the -color_trc output options.
        let filter = format!("format=yuv420p10le,setparams=range=tv:colorspace=bt2020nc:color_primaries=bt2020:color_trc={trc}");
        let mut arguments = vec!["-f", "lavfi", "-i", &source, "-vf", &filter, "-c:v", codec];
        arguments.extend_from_slice(options);
        run_ffmpeg(&arguments, &path).then_some(path)
    };
    let chosen = *ENCODER.get_or_init(|| (0..TEN_BIT_ENCODERS.len()).find(|&index| make(index).is_some()));
    match chosen {
        Some(index) => make(index),
        None => {
            eprintln!("skipping: none of the 10-bit encoders {:?} works with third_party/ffmpeg", TEN_BIT_ENCODERS.map(|(name, _)| name));
            None
        }
    }
}

/// The 10-bit Y, U, V codes at `(x, y)` of the first frame, as ffmpeg decodes them in software.
fn yuv_codes(path: &Path, x: u32, y: u32) -> [f64; 3] {
    let output = Command::new(ffmpeg_tool())
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(output.status.success(), "decoding {} failed", path.display());
    let raw = output.stdout;
    let sample = |offset: usize| f64::from(u16::from_le_bytes([raw[offset * 2], raw[offset * 2 + 1]]));
    let (w, h) = (HDR_W as usize, HDR_H as usize);
    let luma = sample(y as usize * w + x as usize);
    let chroma = (y as usize / 2) * (w / 2) + x as usize / 2;
    [luma, sample(w * h + chroma), sample(w * h + w * h / 4 + chroma)]
}

/// Limited range BT.2020 YUV codes of 10 bits to non-linear RGB.
fn bt2020_rgb(yuv: [f64; 3]) -> [f64; 3] {
    let (kr, kb) = (0.2627, 0.0593);
    let y = (yuv[0] - 64.0) / 876.0;
    let u = (yuv[1] - 512.0) / 896.0;
    let v = (yuv[2] - 512.0) / 896.0;
    let r = y + 2.0 * (1.0 - kr) * v;
    let b = y + 2.0 * (1.0 - kb) * u;
    let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b].map(|c| c.clamp(0.0, 1.0))
}

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [i32; 3] {
    let at = ((y * width + x) * 4) as usize;
    [i32::from(rgba[at]), i32::from(rgba[at + 1]), i32::from(rgba[at + 2])]
}

fn check_ten_bit(trc: &str, transfer: Transfer) {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let Some(clip) = ten_bit_clip(trc) else { return };
    let rgba = render_first(&gpu, &clip, HDR_W, HDR_H);
    for x in [HDR_W / 4, HDR_W * 3 / 4] {
        let y = HDR_H / 2;
        let signal = bt2020_rgb(yuv_codes(&clip, x, y));
        let expected = hdr::to_sdr(signal, transfer, Primaries::Bt2020, 0).map(|c| (c * 255.0).round() as i32);
        let got = pixel(&rgba, HDR_W, x, y);
        assert!(got.iter().zip(expected).all(|(g, e)| (g - e).abs() <= 4), "{trc} at x {x}: rendered {got:?}, the tone map of {signal:?} is {expected:?}");
        // Without the tone map the grey would come out near its code, 128.
        if x == HDR_W / 4 && transfer == Transfer::Pq {
            assert!(got[0] > 160, "{trc}: the grey was not tone mapped: {got:?}");
        }
    }
    // The thumbnail goes through the same maths on the CPU.
    let mut sampler = FrameSampler::open(&clip, HDR_H).expect("open the thumbnail sampler");
    let thumbnail = sampler.picture_at(Time::from_ticks(0), &|| false).expect("a thumbnail");
    let (x, y) = (thumbnail.width / 4, thumbnail.height / 2);
    let shown = pixel(&thumbnail.rgba, thumbnail.width, x, y);
    let rendered = pixel(&rgba, HDR_W, HDR_W / 4, HDR_H / 2);
    assert!(shown.iter().zip(rendered).all(|(s, r)| (s - r).abs() <= 6), "{trc}: thumbnail {shown:?}, preview {rendered:?}");
}

#[test]
fn a_ten_bit_pq_clip_is_decoded_and_tone_mapped_to_sdr() {
    check_ten_bit("smpte2084", Transfer::Pq);
}

#[test]
fn a_ten_bit_hlg_clip_is_decoded_and_tone_mapped_to_sdr() {
    check_ten_bit("arib-std-b67", Transfer::Hlg);
}

/// A 320x180 NV12 clip: red top-left quarter, green top-right, blue bottom-left, white bottom-right.
fn corner_clip() -> PathBuf {
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).expect("create the clips directory");
    let path = dir.join("corners.mp4");
    if !path.is_file() {
        let source = "color=c=red:s=160x90:r=30:d=0.5[a];color=c=lime:s=160x90:r=30:d=0.5[b];color=c=blue:s=160x90:r=30:d=0.5[c];color=c=white:s=160x90:r=30:d=0.5[d];[a][b]hstack[top];[c][d]hstack[bottom];[top][bottom]vstack";
        assert!(run_ffmpeg(&["-f", "lavfi", "-i", source, "-pix_fmt", "yuv420p", "-c:v", "libopenh264", "-b:v", "2M"], &path), "making the corner clip failed");
    }
    path
}

/// The corner clip remuxed with a display matrix of `degrees` counterclockwise.
fn rotated_clip(degrees: i32) -> PathBuf {
    let source = corner_clip();
    let path = clips_dir().join(format!("corners-rotated-{degrees}.mp4"));
    if !path.is_file() {
        let degrees = degrees.to_string();
        let source = source.to_string_lossy().into_owned();
        assert!(run_ffmpeg(&["-display_rotation", &degrees, "-i", &source, "-c", "copy"], &path), "remuxing with rotation {degrees} failed");
    }
    path
}

/// The first frame as ffmpeg shows it, turned by its own autorotate, at `width` x `height`.
fn ffmpeg_upright(path: &Path, width: u32, height: u32) -> Vec<u8> {
    let output = Command::new(ffmpeg_tool())
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale={width}:{height}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(output.status.success(), "decoding {} failed", path.display());
    assert_eq!(output.stdout.len(), (width * height * 4) as usize, "ffmpeg's picture is {width}x{height}");
    output.stdout
}

/// The colour a probe point shows, as the strongest channels: red, green, blue or white.
fn hue(rgb: [i32; 3]) -> &'static str {
    match rgb {
        [r, g, b] if r > 160 && g > 160 && b > 160 => "white",
        [r, g, b] if r > 160 && g < 100 && b < 100 => "red",
        [r, g, b] if g > 160 && r < 100 && b < 100 => "green",
        [r, g, b] if b > 160 && r < 100 && g < 100 => "blue",
        _ => "mixed",
    }
}

#[test]
fn a_rotated_phone_clip_is_shown_upright_like_ffmpeg_shows_it() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    for degrees in [-90, 90, 180] {
        let clip = rotated_clip(degrees);
        let (width, height) = if degrees == 180 { (320, 180) } else { (180, 320) };
        // The thumbnail sampler is what tells the editor the shape of the source.
        let sampler = FrameSampler::open(&clip, 64).expect("open the thumbnail sampler");
        let (thumb_w, thumb_h) = sampler.picture_size();
        assert_eq!(thumb_w > thumb_h, width > height, "rotation {degrees}: the thumbnail is {thumb_w}x{thumb_h}");
        // An output of the turned shape: the clip fills it.
        let rendered = render_first(&gpu, &clip, width, height);
        let reference = ffmpeg_upright(&clip, width, height);
        let mut thumbnail_sampler = sampler;
        let thumbnail = thumbnail_sampler.picture_at(Time::from_ticks(0), &|| false).expect("a thumbnail");
        let reference_thumbnail = ffmpeg_upright(&clip, thumbnail.width, thumbnail.height);
        for (fx, fy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
            let (x, y) = ((f64::from(width) * fx) as u32, (f64::from(height) * fy) as u32);
            let (got, want) = (hue(pixel(&rendered, width, x, y)), hue(pixel(&reference, width, x, y)));
            assert_ne!(want, "mixed", "the reference probe sits inside a quarter");
            assert_eq!(got, want, "rotation {degrees}, preview at ({x}, {y})");
            let (x, y) = ((f64::from(thumbnail.width) * fx) as u32, (f64::from(thumbnail.height) * fy) as u32);
            let (got, want) = (hue(pixel(&thumbnail.rgba, thumbnail.width, x, y)), hue(pixel(&reference_thumbnail, thumbnail.width, x, y)));
            assert_eq!(got, want, "rotation {degrees}, thumbnail at ({x}, {y})");
        }
    }
}

/// A 320x180 NV12 clip of vertical stripes two pixels wide, remuxed with a display matrix of -90
/// degrees: upright it is 180x320 with horizontal stripes.
fn rotated_stripe_clip() -> PathBuf {
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).expect("create the clips directory");
    let stored = dir.join("stripes.mp4");
    if !stored.is_file() {
        let source = "nullsrc=s=320x180:r=30:d=0.5,format=yuv420p,geq=lum='if(lt(mod(X,4),2),220,30)':cb=128:cr=128";
        assert!(run_ffmpeg(&["-f", "lavfi", "-i", source, "-pix_fmt", "yuv420p", "-c:v", "libopenh264", "-b:v", "20M"], &stored), "making the stripe clip failed");
    }
    let path = dir.join("stripes-rotated--90.mp4");
    if !path.is_file() {
        let stored = stored.to_string_lossy().into_owned();
        assert!(run_ffmpeg(&["-display_rotation", "-90", "-i", &stored, "-c", "copy"], &path), "remuxing the stripes with a rotation failed");
    }
    path
}

/// PSNR in dB of the RGB channels of two RGBA pictures of the same size.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let (mut sum, mut count) = (0.0, 0.0);
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for i in 0..3 {
            let d = f64::from(pa[i]) - f64::from(pb[i]);
            sum += d * d;
            count += 1.0;
        }
    }
    let mse = sum / count;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() }
}

#[test]
fn a_rotated_clip_keeps_its_full_detail() {
    let _lock = gpu_lock();
    let Some(gpu) = gpu() else { return };
    let clip = rotated_stripe_clip();
    // A portrait project of the upright size: the turn must not resample the picture, so the
    // two-pixel stripes come out as sharp as ffmpeg's own autorotate shows them.
    let (width, height) = (180, 320);
    let rendered = render_first(&gpu, &clip, width, height);
    let reference = ffmpeg_upright(&clip, width, height);
    let quality = psnr(&rendered, &reference);
    // Rows of one stripe: the contrast between neighbouring stripes survives.
    let column = width / 2;
    let contrast = (0..height - 2).map(|y| (pixel(&rendered, width, column, y)[0] - pixel(&rendered, width, column, y + 2)[0]).abs()).max().unwrap_or(0);
    assert!(contrast > 120, "the stripes were blurred: the largest contrast two rows apart is {contrast}");
    assert!(quality > 30.0, "the rotated clip differs from ffmpeg's autorotate: PSNR {quality:.1} dB");
}
