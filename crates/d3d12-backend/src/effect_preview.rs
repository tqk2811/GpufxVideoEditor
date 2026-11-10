//! Renders the small previews of one effect package on two built-in pictures.
//!
//! This uses its own device and gpufx's CPU image path. A preview is not time critical, and keeping
//! it off the device of the editor's preview means a broken package cannot disturb that one.
//!
//! The values are those of an editor frame: RGBA8 with gamma-encoded colour and straight alpha.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::gpu::Gpu;
use gpufx_core::{Effect, EffectGraph, FrameInfo, GpuContext, Image, ImageMut, Manifest, Package, StreamInfo};

/// The width and height of every preview, and of the two built-in pictures.
pub const PREVIEW_SIZE: u32 = 144;
/// How many frames an animated preview has.
pub const PREVIEW_FRAMES: u32 = 24;
/// The frame rate of an animated preview.
pub const PREVIEW_FPS: u32 = 20;

/// The largest side of a package's output that is still rendered and scaled down.
const MAX_OUTPUT_SIDE: u32 = 4096;
/// The time of a still, in seconds.
const STILL_TIME: f64 = 0.6;
/// The progress of a still, for packages that have one.
const STILL_PROGRESS: f64 = 0.5;
/// When an effect is rendered to find out whether it changes over time, in seconds. The times
/// include an odd frame index (7), so an effect that only reacts to odd or even frames is seen.
const PROBE_TIMES: [f64; 4] = [0.0, 0.35, 0.6, 1.1];

const IMAGE_A_PNG: &[u8] = include_bytes!("../assets/effect-preview-a.png");
const IMAGE_B_PNG: &[u8] = include_bytes!("../assets/effect-preview-b.png");

/// The encoded built-in pictures, in the order A, B. A preview depends on them, so its cache key does.
pub fn preview_image_files() -> [&'static [u8]; 2] {
    [IMAGE_A_PNG, IMAGE_B_PNG]
}

/// Why a preview could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    /// Anything no package is to blame for: no (hardware) device could be opened, the device was
    /// lost, it ran out of memory, or the package could not be read. This includes temporary
    /// conditions, so it must not be taken as a verdict on the package.
    Device(String),
    /// This package cannot be previewed. Others still can.
    Package(String),
}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewError::Device(message) | PreviewError::Package(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PreviewError {}

/// An animated preview, of RGBA8 frames with [`PREVIEW_SIZE`] sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewLoop {
    /// [`PREVIEW_FRAMES`] frames, or a single one when `is_static`.
    pub frames: Vec<Vec<u8>>,
    /// The package does not change over time.
    pub is_static: bool,
}

/// How a package is driven, which is what its manifest says it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Effect,
    /// One input and a `progress` parameter, such as `fade`.
    ProgressEffect,
    Transition,
}

/// Mirrors the editor's sorting of packages into effects and transitions.
fn classify(inputs: usize, has_progress: bool) -> Option<Role> {
    match (inputs, has_progress) {
        (1, false) => Some(Role::Effect),
        (1, true) => Some(Role::ProgressEffect),
        (2, true) => Some(Role::Transition),
        _ => None,
    }
}

/// What a package is given for one rendered frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    time: f64,
    frame: u64,
    progress: Option<f64>,
}

/// The frame index that goes with a time at the preview's frame rate.
fn frame_at(time: f64) -> u64 {
    (time * f64::from(PREVIEW_FPS)).round() as u64
}

fn still_sample(role: Role) -> Sample {
    let progress = (role != Role::Effect).then_some(STILL_PROGRESS);
    Sample { time: STILL_TIME, frame: frame_at(STILL_TIME), progress }
}

fn probe_sample(role: Role, time: f64) -> Sample {
    let progress = (role != Role::Effect).then_some(STILL_PROGRESS);
    Sample { time, frame: frame_at(time), progress }
}

/// Frame `k` of the loop. A transition and a `progress` effect (such as `fade`) both play their
/// progress from 0 at the first frame to 1 at the last; a plain effect has none.
fn loop_sample(role: Role, k: u32) -> Sample {
    let progress = match role {
        Role::Effect => None,
        Role::ProgressEffect | Role::Transition => Some(f64::from(k) / f64::from(PREVIEW_FRAMES - 1)),
    };
    Sample { time: f64::from(k) / f64::from(PREVIEW_FPS), frame: u64::from(k), progress }
}

/// Whether every picture holds the same bytes.
fn all_identical(pictures: &[Vec<u8>]) -> bool {
    pictures.windows(2).all(|pair| pair[0] == pair[1])
}

/// Bilinear resize of RGBA8. Some packages choose their own output size.
fn resize_rgba(source: &[u8], width: u32, height: u32, out_width: u32, out_height: u32) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let (out_width, out_height) = (out_width as usize, out_height as usize);
    let mut out = Vec::with_capacity(out_width * out_height * 4);
    // Pixel centres of the output, mapped to positions between the pixel centres of the source.
    let position = |index: usize, out_size: usize, size: usize| ((index as f64 + 0.5) * size as f64 / out_size as f64 - 0.5).clamp(0.0, (size - 1) as f64);
    for y in 0..out_height {
        let fy = position(y, out_height, height);
        let (y0, wy) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(height - 1);
        for x in 0..out_width {
            let fx = position(x, out_width, width);
            let (x0, wx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(width - 1);
            for channel in 0..4 {
                let at = |px: usize, py: usize| f64::from(source[(py * width + px) * 4 + channel]);
                let top = at(x0, y0) * (1.0 - wx) + at(x1, y0) * wx;
                let bottom = at(x0, y1) * (1.0 - wx) + at(x1, y1) * wx;
                out.push((top * (1.0 - wy) + bottom * wy).round() as u8);
            }
        }
    }
    out
}

/// Decodes one of the built-in pictures to RGBA8.
fn decode_picture(file: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(file));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("the picture is too large")?];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    if (info.width, info.height) != (PREVIEW_SIZE, PREVIEW_SIZE) || info.bit_depth != png::BitDepth::Eight {
        return Err(format!("expected an 8 bit {PREVIEW_SIZE}x{PREVIEW_SIZE} picture, found {}x{} {:?}", info.width, info.height, info.bit_depth));
    }
    let pixels = &buffer[..info.buffer_size()];
    match info.color_type {
        png::ColorType::Rgba => Ok(pixels.to_vec()),
        png::ColorType::Rgb => Ok(pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()),
        other => Err(format!("unexpected colour type {other:?}")),
    }
}

/// The kind of a wgpu error: only a validation error is the fault of what was given to wgpu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WgpuErrorKind {
    Validation,
    /// Out of memory or internal: a condition of the machine, not of the package.
    Other,
}

/// Whose fault a failure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blame {
    Device,
    Package,
}

/// A failure of one render: what it says, and whether the package can be blamed for it.
#[derive(Debug)]
struct Failure {
    blame: Blame,
    message: String,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure { blame: Blame::Package, message }
    }
}

/// A device, and what it reported outside gpufx's own error handling.
struct Device {
    context: Arc<GpuContext>,
    /// The first wgpu error since the last render.
    error: Arc<Mutex<Option<(WgpuErrorKind, String)>>>,
    lost: Arc<Mutex<Option<String>>>,
    /// Keeps the instance and the adapter of the device alive.
    _gpu: Gpu,
}

impl Device {
    /// Opens a device of its own on the adapter the backend would use (`GPUFX_ADAPTER` included).
    /// A software adapter is refused: its slow output must not be stored as a preview.
    fn open() -> Result<Self, PreviewError> {
        match catch_unwind(Self::open_unguarded) {
            Ok(result) => result,
            Err(payload) => Err(PreviewError::Device(format!("opening a GPU device panicked: {}", panic_message(payload.as_ref())))),
        }
    }

    fn open_unguarded() -> Result<Self, PreviewError> {
        let gpu = Gpu::create_hardware()
            .map_err(|e| PreviewError::Device(format!("opening a GPU device failed: {e}")))?
            .ok_or_else(|| PreviewError::Device("no hardware GPU adapter (none, or only a software one)".to_owned()))?;
        let context = gpu.context.clone();
        let error: Arc<Mutex<Option<(WgpuErrorKind, String)>>> = Arc::default();
        let sink = error.clone();
        // The default handler panics, and a package's error must not. These replace the handlers of `Gpu`.
        context.device().on_uncaptured_error(Arc::new(move |e: gpufx_core::wgpu::Error| {
            let kind = if matches!(e, gpufx_core::wgpu::Error::Validation { .. }) { WgpuErrorKind::Validation } else { WgpuErrorKind::Other };
            sink.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(|| (kind, e.to_string()));
        }));
        let lost: Arc<Mutex<Option<String>>> = Arc::default();
        let lost_sink = lost.clone();
        context.device().set_device_lost_callback(move |reason, message| {
            lost_sink.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(|| format!("{reason:?}: {message}"));
        });
        Ok(Self { context, error, lost, _gpu: gpu })
    }

    fn lost(&self) -> Option<String> {
        self.lost.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn take_error(&self) -> Option<(WgpuErrorKind, String)> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// Who is to blame for one render, from what happened during it. `None` when it went well.
/// `result` is the blame of the work's own failure, if it failed.
fn blame_for(panicked: bool, lost: bool, wgpu_error: Option<WgpuErrorKind>, result: Option<Blame>) -> Option<Blame> {
    if lost {
        // A lost device explains a panic or an error that followed it.
        return Some(Blame::Device);
    }
    if panicked {
        return Some(Blame::Package);
    }
    match wgpu_error {
        Some(WgpuErrorKind::Validation) => Some(Blame::Package),
        Some(WgpuErrorKind::Other) => Some(Blame::Device),
        None => result,
    }
}

/// One opened package, ready to render frames of the preview's size.
struct PackageRender<'a> {
    graph: EffectGraph,
    role: Role,
    inputs: Vec<Image<'a>>,
    output_width: u32,
    output_height: u32,
}

impl<'a> PackageRender<'a> {
    fn open(context: &Arc<GpuContext>, path: &Path, pictures: &'a [Vec<u8>; 2]) -> Result<Self, Failure> {
        let package = Package::open(path).map_err(|e| match e {
            // A file that cannot be read now (locked while it is installed, a share that is gone) says nothing of the package.
            gpufx_core::Error::Io(io) => Failure { blame: Blame::Device, message: format!("reading the package failed: {io}") },
            other => Failure::from(other.to_string()),
        })?;
        let role = role_of(&package.manifest)?;
        let effect = Effect::new(context.clone(), package).map_err(|e| e.to_string())?;
        let mut holder = Some(effect);
        let graph = EffectGraph::parse(if role == Role::Transition { "transition" } else { "effect" }, |_| Ok(("preview".to_owned(), holder.take().expect("one effect")))).map_err(|e| e.to_string())?;
        if graph.output_count() != 1 {
            return Err(format!("the package has {} outputs, a preview shows one", graph.output_count()).into());
        }
        let count = if role == Role::Transition { 2 } else { 1 };
        let inputs: Vec<Image<'a>> = pictures[..count].iter().map(|data| Image { data, width: PREVIEW_SIZE, height: PREVIEW_SIZE, stride: PREVIEW_SIZE * 4 }).collect();
        let streams: Vec<StreamInfo> = inputs.iter().map(|_| StreamInfo { format: None, width: PREVIEW_SIZE, height: PREVIEW_SIZE }).collect();
        graph.check_inputs(&streams).map_err(|e| e.to_string())?;
        let pads = vec![(PREVIEW_SIZE, PREVIEW_SIZE); count];
        let (output_width, output_height) = graph.output_size(0, &pads).map_err(|e| format!("computing the output size failed: {e}"))?;
        if output_width == 0 || output_height == 0 || output_width > MAX_OUTPUT_SIDE || output_height > MAX_OUTPUT_SIDE {
            return Err(format!("the output would be {output_width}x{output_height}").into());
        }
        Ok(Self { graph, role, inputs, output_width, output_height })
    }

    /// One frame, with [`PREVIEW_SIZE`] sides.
    fn render(&mut self, sample: Sample) -> Result<Vec<u8>, String> {
        if let Some(progress) = sample.progress {
            self.graph.set_param("progress", &[progress]).map_err(|e| format!("setting progress failed: {e}"))?;
        }
        let (width, height) = (self.output_width, self.output_height);
        let mut out = vec![0u8; width as usize * height as usize * 4];
        self.graph.process(&self.inputs, vec![ImageMut { data: &mut out, width, height, stride: width * 4 }], &FrameInfo::new(sample.time, sample.frame)).map_err(|e| e.to_string())?;
        if (width, height) == (PREVIEW_SIZE, PREVIEW_SIZE) {
            Ok(out)
        } else {
            Ok(resize_rgba(&out, width, height, PREVIEW_SIZE, PREVIEW_SIZE))
        }
    }
}

fn role_of(manifest: &Manifest) -> Result<Role, String> {
    let has_progress = manifest.params.iter().any(|param| param.name == "progress");
    classify(manifest.inputs.len(), has_progress).ok_or_else(|| format!("{} inputs{} is neither an effect nor a transition", manifest.inputs.len(), if has_progress { " with progress" } else { " without progress" }))
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown panic".to_owned())
}

/// Renders previews, one package at a time, on a device of its own.
pub struct PreviewRenderer {
    /// `None` once the device was lost or a render panicked; the next render opens another.
    device: Option<Device>,
    pictures: [Vec<u8>; 2],
}

impl PreviewRenderer {
    pub fn new() -> Result<Self, PreviewError> {
        let decode = |file| decode_picture(file).map_err(|e| PreviewError::Device(format!("a built-in preview picture is unusable: {e}")));
        Ok(Self { device: Some(Device::open()?), pictures: [decode(IMAGE_A_PNG)?, decode(IMAGE_B_PNG)?] })
    }

    /// The name of the device in use, for logs.
    pub fn device_description(&self) -> Option<String> {
        self.device.as_ref().map(|device| device.context.description())
    }

    /// The single picture of a package: RGBA8 with [`PREVIEW_SIZE`] sides.
    pub fn render_still(&mut self, package: &Path) -> Result<Vec<u8>, PreviewError> {
        self.guarded(package, |render| Ok(render.render(still_sample(render.role))?))
    }

    /// The animation of a package, played on hover. An effect that does not change over time gives
    /// one frame, found by comparing frames rendered at a few times.
    pub fn render_loop(&mut self, package: &Path) -> Result<PreviewLoop, PreviewError> {
        self.guarded(package, |render| {
            if render.role == Role::Effect {
                let mut probes = Vec::with_capacity(PROBE_TIMES.len());
                for time in PROBE_TIMES {
                    probes.push(render.render(probe_sample(render.role, time))?);
                }
                if all_identical(&probes) {
                    probes.truncate(1);
                    return Ok(PreviewLoop { frames: probes, is_static: true });
                }
            }
            let frames = (0..PREVIEW_FRAMES).map(|k| render.render(loop_sample(render.role, k))).collect::<Result<Vec<_>, _>>()?;
            Ok(PreviewLoop { frames, is_static: false })
        })
    }

    /// Runs `work` on the opened package, turning every failure of the package into an `Err`.
    fn guarded<T>(&mut self, package: &Path, work: impl FnOnce(&mut PackageRender) -> Result<T, Failure>) -> Result<T, PreviewError> {
        if self.device.as_ref().is_some_and(|device| device.lost().is_some()) {
            self.device = None;
        }
        if self.device.is_none() {
            self.device = Some(Device::open()?);
        }
        let device = self.device.as_ref().expect("opened above");
        device.take_error();
        let pictures = &self.pictures;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut render = PackageRender::open(&device.context, package, pictures)?;
            work(&mut render)
        }));
        let name = package.display();
        // Read before the device is dropped: a panic may be the effect of a lost device.
        let lost = device.lost();
        let wgpu_error = device.take_error();
        let panicked = outcome.is_err();
        let result_blame = match &outcome {
            Ok(Err(failure)) => Some(failure.blame),
            _ => None,
        };
        let blame = blame_for(panicked, lost.is_some(), wgpu_error.as_ref().map(|(kind, _)| *kind), result_blame);
        if panicked {
            // A panic part way may have left the device's state behind.
            self.device = None;
        }
        let Some(blame) = blame else {
            return outcome.ok().and_then(Result::ok).ok_or_else(|| PreviewError::Package(format!("package {name}: no result")));
        };
        let message = if let Some(reason) = &lost {
            format!("package {name}: the GPU device was lost: {reason}")
        } else if let Err(payload) = &outcome {
            format!("package {name}: rendering panicked: {}", panic_message(payload.as_ref()))
        } else if let Some((_, message)) = &wgpu_error {
            format!("package {name}: wgpu error: {message}")
        } else if let Ok(Err(failure)) = &outcome {
            format!("package {name}: {}", failure.message)
        } else {
            format!("package {name}: failed")
        };
        Err(match blame {
            Blame::Device => PreviewError::Device(message),
            Blame::Package => PreviewError::Package(message),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_mirrors_the_editor() {
        assert_eq!(classify(1, false), Some(Role::Effect));
        assert_eq!(classify(1, true), Some(Role::ProgressEffect));
        assert_eq!(classify(2, true), Some(Role::Transition));
        assert_eq!(classify(2, false), None);
        assert_eq!(classify(0, false), None);
        assert_eq!(classify(3, true), None);
    }

    #[test]
    fn a_still_is_at_the_chosen_moment() {
        assert_eq!(still_sample(Role::Effect), Sample { time: 0.6, frame: 12, progress: None });
        assert_eq!(still_sample(Role::ProgressEffect).progress, Some(0.5));
        assert_eq!(still_sample(Role::Transition).progress, Some(0.5));
    }

    #[test]
    fn a_transition_loop_runs_progress_from_zero_to_one() {
        assert_eq!(loop_sample(Role::Transition, 0).progress, Some(0.0));
        assert_eq!(loop_sample(Role::Transition, PREVIEW_FRAMES - 1).progress, Some(1.0));
    }

    #[test]
    fn an_effect_loop_runs_at_the_preview_rate() {
        assert_eq!(loop_sample(Role::Effect, 0), Sample { time: 0.0, frame: 0, progress: None });
        assert_eq!(loop_sample(Role::Effect, 23), Sample { time: 1.15, frame: 23, progress: None });
        assert_eq!(loop_sample(Role::ProgressEffect, 0).progress, Some(0.0));
        assert_eq!(loop_sample(Role::ProgressEffect, 23).progress, Some(1.0));
        assert_eq!(loop_sample(Role::ProgressEffect, 23).frame, 23);
    }

    #[test]
    fn probes_keep_the_frame_index_in_step_with_time() {
        let frames: Vec<u64> = PROBE_TIMES.iter().map(|&time| probe_sample(Role::Effect, time).frame).collect();
        assert_eq!(frames, [0, 7, 12, 22]);
    }

    #[test]
    fn blame_follows_the_cause() {
        use Blame::{Device, Package};
        use WgpuErrorKind::{Other, Validation};
        assert_eq!(blame_for(false, false, None, None), None);
        assert_eq!(blame_for(false, false, None, Some(Package)), Some(Package));
        assert_eq!(blame_for(false, false, None, Some(Device)), Some(Device));
        assert_eq!(blame_for(true, false, None, None), Some(Package));
        // A lost device explains a panic, an error and a failed result.
        assert_eq!(blame_for(true, true, None, None), Some(Device));
        assert_eq!(blame_for(false, true, Some(Validation), Some(Package)), Some(Device));
        assert_eq!(blame_for(false, true, None, None), Some(Device));
        assert_eq!(blame_for(false, false, Some(Validation), None), Some(Package));
        assert_eq!(blame_for(false, false, Some(Other), None), Some(Device));
        assert_eq!(blame_for(false, false, Some(Other), Some(Package)), Some(Device));
    }

    #[test]
    fn identical_means_every_byte() {
        assert!(all_identical(&[vec![1, 2], vec![1, 2], vec![1, 2]]));
        assert!(!all_identical(&[vec![1, 2], vec![1, 2], vec![1, 3]]));
        assert!(!all_identical(&[vec![1, 2], vec![9, 2], vec![1, 2]]));
        assert!(all_identical(&[vec![1, 2]]));
    }

    #[test]
    fn a_same_size_resize_changes_nothing() {
        let source: Vec<u8> = (0..16).collect();
        assert_eq!(resize_rgba(&source, 2, 2, 2, 2), source);
    }

    #[test]
    fn a_flat_picture_stays_flat_when_resized() {
        let source = [10u8, 20, 30, 40].repeat(6 * 4);
        let out = resize_rgba(&source, 6, 4, 3, 5);
        assert_eq!(out, [10u8, 20, 30, 40].repeat(15));
    }

    #[test]
    fn the_built_in_pictures_are_opaque_and_the_right_size() {
        for file in preview_image_files() {
            let rgba = decode_picture(file).expect("decodes");
            assert_eq!(rgba.len(), (PREVIEW_SIZE * PREVIEW_SIZE * 4) as usize);
            assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
        }
    }
}
