//! Compositing with gpufx on the shared D3D12 device.
//!
//! Every image is a raw `R8G8B8A8_UNORM` D3D12 resource with its own fence, in COMMON between
//! users, so every step is one `process_d3d12` call and gpufx orders the GPU work with the fences.
//! Decoded frames go through a `scale|effects...` graph per clip; transitions and track compositing
//! are graphs on those images. The CPU waits only when reading the finished image.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use engine::render::{ClipCache, ClipKey, Compositor, Decoder, FrameContext, RenderError};
use engine::schedule::{ClipClock, Progress};
use engine::{is_image_source, text_raster_scale, Clip, ClipTransform, Effect as ProjectEffect, TextContent, Time, TICKS_PER_SECOND};
use gpufx_core::d3d12::D3d12Frame;
use gpufx_core::{ColorInfo, Effect, EffectGraph, FrameInfo, GpuContext, Package, PixelFormat};
use serde_json::{Map, Value};
use wgpu::hal::{self, api::Dx12, CommandEncoder as _};
use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12Fence, ID3D12Resource, D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_FENCE_FLAG_NONE, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
    D3D12_MEMORY_POOL_UNKNOWN, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON,
    D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::alpha_merge::{larger_alpha_effect, restore_alpha_effect};
use crate::decoder::{DecodedFrame, PublishClock, SequentialDecoder};
use crate::gpu::Gpu;
use crate::packages::{CLIP_DURATION, CLIP_TIME};
use crate::paths::ProjectPaths;
use crate::rotated_place::rotated_place_effect;
use crate::still::{aligned_rows, premultiplied_still, read_still, StillPicture};
use gpufx_text::TextRasterizer;

const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MAX_SPARE_IMAGES: usize = 3;
/// How much GPU memory stills may keep for reuse: what is on no clip is let go first.
const STILL_CACHE_BYTES: u64 = 1 << 30;
/// The side of the images a graph is tried on, to see what it does with alpha.
const ALPHA_PROBE_SIDE: u32 = 16;

/// An index into the image pool, or [`BLANK_IMAGE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageId(usize);

/// The image of a frame no clip covers; it has no texture.
const BLANK_IMAGE: ImageId = ImageId(usize::MAX);

struct Image {
    resource: ID3D12Resource,
    fence: ID3D12Fence,
    fence_value: u64,
    barrier_texture: hal::dx12::Texture,
    copy_source: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    /// The colours in the texture are multiplied by the alpha, as what [`D3d12Compositor::place`] writes.
    premultiplied: bool,
}

fn import_texture(gpu: &Gpu, resource: &ID3D12Resource, size: wgpu::Extent3d, usage: wgpu::TextureUsages, state: wgpu::TextureUses) -> wgpu::Texture {
    let hal_texture = unsafe { hal::dx12::Device::texture_from_raw(resource.clone(), OUTPUT_FORMAT, wgpu::TextureDimension::D2, size, 1, 1) };
    // SAFETY: the resource is valid and stays alive through the clones held by the wrappers.
    // Each use is bracketed by `record_transition` into the `state` declared here.
    unsafe {
        gpu.device.create_texture_from_hal::<Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("gpufx-render image"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OUTPUT_FORMAT,
                usage,
                view_formats: &[],
            },
            state,
        )
    }
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d { width, height, depth_or_array_layers: 1 }
}

impl Image {
    fn create(gpu: &Gpu, width: u32, height: u32) -> Result<Self, String> {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        };
        let device = gpu.raw_device();
        let mut created: Option<ID3D12Resource> = None;
        // SAFETY: the descriptions are valid and outlive the calls; `created` receives the resource.
        let fence: ID3D12Fence = unsafe {
            device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &description, D3D12_RESOURCE_STATE_COMMON, None, &mut created).map_err(|e| format!("creating an image failed: {e}"))?;
            device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(|e| format!("creating an image fence failed: {e}"))?
        };
        let resource = created.ok_or("CreateCommittedResource returned no resource")?;
        let size = extent(width, height);
        let copy_source = import_texture(gpu, &resource, size, wgpu::TextureUsages::COPY_SRC, wgpu::TextureUses::COPY_SRC);
        let display = import_texture(gpu, &resource, size, wgpu::TextureUsages::TEXTURE_BINDING, wgpu::TextureUses::RESOURCE);
        let view = display.create_view(&wgpu::TextureViewDescriptor::default());
        // SAFETY: the resource is valid and this wrapper only records barriers on it.
        let barrier_texture = unsafe { hal::dx12::Device::texture_from_raw(resource.clone(), OUTPUT_FORMAT, wgpu::TextureDimension::D2, size, 1, 1) };
        Ok(Self { resource, fence, fence_value: 0, barrier_texture, copy_source, view, width, height, premultiplied: false })
    }

    /// Makes an image of `picture` on the GPU and waits until it is there.
    fn upload(gpu: &Gpu, picture: &StillPicture) -> Result<Self, String> {
        let (width, height) = (picture.width, picture.height);
        let image = Self::create(gpu, width, height)?;
        let size = extent(width, height);
        let target = import_texture(gpu, &image.resource, size, wgpu::TextureUsages::COPY_DST, wgpu::TextureUses::COPY_DST);
        let row_bytes = width * 4;
        let aligned_row = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let bytes = aligned_rows(&picture.rgba, row_bytes as usize, aligned_row as usize);
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("still upload"), size: bytes.len() as u64, usage: wgpu::BufferUsages::COPY_SRC, mapped_at_creation: true });
        staging.slice(..).get_mapped_range_mut().map_err(|e| format!("mapping the upload buffer failed: {e}"))?.copy_from_slice(&bytes);
        staging.unmap();
        let mut before = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("still upload state") });
        image.record_transition(&mut before, wgpu::TextureUses::COPY_DST, false);
        let mut copy = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("still upload") });
        copy.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo { buffer: &staging, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(aligned_row), rows_per_image: Some(height) } },
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            size,
        );
        let mut after = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("still upload state back") });
        image.record_transition(&mut after, wgpu::TextureUses::COPY_DST, true);
        let submission = {
            let _gate = gpu.lock_submits();
            gpu.queue.submit([before.finish(), copy.finish(), after.finish()])
        };
        gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None }).map_err(|e| format!("waiting for the GPU failed: {e}"))?;
        gpu.check_errors()?;
        Ok(image)
    }

    fn frame(&self, width: u32, height: u32) -> D3d12Frame {
        D3d12Frame {
            format: PixelFormat::Rgba,
            width,
            height,
            resource: self.resource.as_raw().cast(),
            subresource: 0,
            fence: self.fence.as_raw().cast(),
            fence_value: self.fence_value,
            color: ColorInfo::default(),
        }
    }

    /// The frame of this image as the top of a composite: what is premultiplied must say so.
    fn composited_top(&self, width: u32, height: u32) -> D3d12Frame {
        let mut frame = self.frame(width, height);
        frame.color.premultiplied = self.premultiplied;
        frame
    }

    /// Records the barrier between COMMON (where gpufx leaves and expects the resource) and `state`.
    fn record_transition(&self, encoder: &mut wgpu::CommandEncoder, state: wgpu::TextureUses, to_common: bool) {
        let (from, to) = if to_common { (state, wgpu::TextureUses::UNINITIALIZED) } else { (wgpu::TextureUses::UNINITIALIZED, state) };
        // SAFETY: the texture is valid; the caller waited for the image's fence, so nothing else
        // uses the resource, and the wrapper declares `state` around these barriers.
        unsafe {
            encoder.as_hal_mut::<Dx12, _, _>(|raw| {
                if let Some(raw) = raw {
                    raw.transition_textures(std::iter::once(hal::TextureBarrier {
                        texture: &self.barrier_texture,
                        range: wgpu::ImageSubresourceRange::default(),
                        usage: hal::StateTransition { from, to },
                    }));
                }
            });
        }
    }

    fn wait(&self) -> Result<(), String> {
        // SAFETY: a null event makes SetEventOnCompletion block until the value is reached.
        unsafe { self.fence.SetEventOnCompletion(self.fence_value, HANDLE::default()) }.map_err(|e| format!("image fence wait failed: {e}"))
    }

    /// Copies the image, whose fence has been waited for, into `rgba` as tight RGBA8 rows.
    fn read_rgba(&self, gpu: &Gpu, buffer: &wgpu::Buffer, rgba: &mut Vec<u8>) -> Result<(), String> {
        let (width, height) = (self.width as usize, self.height as usize);
        let row_bytes = self.width * 4;
        let aligned_row = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let mut before = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("read-back state") });
        self.record_transition(&mut before, wgpu::TextureUses::COPY_SRC, false);
        let mut copy = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("read-back") });
        copy.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &self.copy_source, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(aligned_row), rows_per_image: Some(self.height) } },
            extent(self.width, self.height),
        );
        let mut after = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("read-back state back") });
        self.record_transition(&mut after, wgpu::TextureUses::COPY_SRC, true);
        let submission = {
            let _gate = gpu.lock_submits();
            gpu.queue.submit([before.finish(), copy.finish(), after.finish()])
        };
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None }).map_err(|e| format!("waiting for the GPU failed: {e}"))?;
        rgba.clear();
        {
            let mapped = slice.get_mapped_range().map_err(|e| format!("mapping the read-back buffer failed: {e}"))?;
            rgba.reserve(width * height * 4);
            for row in mapped.chunks_exact(aligned_row as usize) {
                rgba.extend_from_slice(&row[..row_bytes as usize]);
            }
        }
        buffer.unmap();
        Ok(())
    }
}

fn readback_buffer(gpu: &Gpu, width: u32, height: u32) -> wgpu::Buffer {
    let aligned_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpufx-render read-back"),
        size: u64::from(aligned_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

/// A finished composited image detached from the compositor's pool. The GPU work on it is done, and
/// it is in COMMON between uses. Give it back with [`D3d12Compositor::recycle`] once the GPU has
/// finished every use of it, or drop it.
pub struct SharedImage {
    image: Image,
}

impl SharedImage {
    pub fn width(&self) -> u32 {
        self.image.width
    }

    pub fn height(&self) -> u32 {
        self.image.height
    }

    /// The shader-readable view of the image.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.image.view
    }

    /// Records the barrier that makes the image readable by shaders.
    pub fn record_begin_sampling(&self, encoder: &mut wgpu::CommandEncoder) {
        self.image.record_transition(encoder, wgpu::TextureUses::RESOURCE, false);
    }

    /// Records the barrier that returns the image to COMMON, after sampling.
    pub fn record_end_sampling(&self, encoder: &mut wgpu::CommandEncoder) {
        self.image.record_transition(encoder, wgpu::TextureUses::RESOURCE, true);
    }

    /// Reads the image back as tight RGBA8 rows. For verification.
    pub fn read_rgba(&self, gpu: &Gpu) -> Result<Vec<u8>, String> {
        let buffer = readback_buffer(gpu, self.image.width, self.image.height);
        let mut rgba = Vec::new();
        self.image.read_rgba(gpu, &buffer, &mut rgba)?;
        Ok(rgba)
    }
}

fn decoded_input(frame: &DecodedFrame) -> Result<D3d12Frame, String> {
    Ok(D3d12Frame {
        format: PixelFormat::Nv12,
        width: frame.width(),
        height: frame.height(),
        resource: frame.texture_ptr(),
        subresource: frame.subresource_index(),
        fence: frame.fence_ptr(),
        fence_value: frame.fence_value(),
        color: frame.color_info()?,
    })
}

/// A still on the GPU, shared by every clip that shows it. The GPU only ever reads it; the fence
/// value goes up with each read, like the transparent image's.
pub struct StillImage {
    image: Image,
    fence_value: AtomicU64,
    aspect: f64,
    has_alpha: bool,
}

impl StillImage {
    fn input(&self) -> D3d12Frame {
        let mut frame = self.image.frame(self.image.width, self.image.height);
        frame.fence_value = self.fence_value.load(Ordering::Relaxed);
        frame
    }
}

/// A picture to composite: a decoded video frame, or a still, the same at any time.
pub enum SourceFrame {
    Video(DecodedFrame),
    Still(Arc<StillImage>),
}

impl SourceFrame {
    fn input(&self) -> Result<D3d12Frame, String> {
        match self {
            Self::Video(frame) => decoded_input(frame),
            Self::Still(still) => Ok(still.input()),
        }
    }

    fn display_aspect(&self) -> f64 {
        match self {
            Self::Video(frame) => frame.display_aspect(),
            Self::Still(still) => still.aspect,
        }
    }

    fn raise_fence_value(&mut self, value: u64) {
        match self {
            Self::Video(frame) => frame.raise_fence_value(value),
            Self::Still(still) => {
                still.fence_value.fetch_max(value, Ordering::Relaxed);
            }
        }
    }

    /// Whether the picture has see-through parts, so even a whole frame of it needs placing.
    fn has_alpha(&self) -> bool {
        matches!(self, Self::Still(still) if still.has_alpha)
    }

    fn is_still(&self) -> bool {
        matches!(self, Self::Still(_))
    }
}

/// What a clip reads its pictures from.
pub enum SourceDecoder {
    Video(SequentialDecoder),
    Still(Arc<StillImage>),
}

impl Decoder for SourceDecoder {
    type Frame = SourceFrame;

    fn frame_at(&mut self, time: Time) -> Result<SourceFrame, RenderError> {
        match self {
            Self::Video(decoder) => decoder.frame_at(time).map(SourceFrame::Video),
            Self::Still(still) => Ok(SourceFrame::Still(still.clone())),
        }
    }

    fn wait_idle(&mut self) -> Result<(), RenderError> {
        match self {
            Self::Video(decoder) => decoder.wait_idle(),
            Self::Still(_) => Ok(()),
        }
    }
}

/// What a kept still was made from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum StillKey {
    /// An image file.
    File(PathBuf),
    /// A text, written out as JSON, rasterized for an output this many pixels tall at this scale
    /// (the bits of the `f32`).
    Text { text: String, output_height: u32, scale: u32 },
}

/// A still kept for the next clip that shows the file.
struct CachedStill {
    stamp: (u64, Option<SystemTime>),
    still: Arc<StillImage>,
    bytes: u64,
    last_used: u64,
}

/// The numbers of a parameter value, or `None` for text, which is set through its own call.
fn param_numbers(value: &Value) -> Result<Option<Vec<f64>>, String> {
    match value {
        Value::Number(n) => n.as_f64().map(|v| Some(vec![v])).ok_or_else(|| format!("not a number: {n}")),
        Value::Bool(b) => Ok(Some(vec![f64::from(u8::from(*b))])),
        Value::String(_) => Ok(None),
        Value::Array(items) => items.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>().map(Some).ok_or_else(|| "array items must be numbers".to_owned()),
        other => Err(format!("unsupported value {other}")),
    }
}

/// Sets the static `params` of a project entry on `effect`. `skip` names parameters set later.
fn apply_params(effect: &mut Effect, package: &str, params: &Map<String, Value>, skip: &[&str]) -> Result<(), String> {
    for (name, value) in params {
        if skip.contains(&name.as_str()) {
            continue;
        }
        let result = match (param_numbers(value), value) {
            (Ok(Some(numbers)), _) => effect.params_mut().set(name, &numbers).map_err(|e| e.to_string()),
            (Ok(None), Value::String(text)) => effect.params_mut().set_str(name, text).map_err(|e| e.to_string()),
            (Ok(None), _) => Ok(()),
            (Err(message), _) => Err(message),
        };
        result.map_err(|e| format!("package {package}, parameter {name}: {e}"))?;
    }
    Ok(())
}

/// The parameters a clip's clock drives, in seconds. Only those of a package that declares them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ClockParams {
    time: bool,
    duration: bool,
}

/// Which clock parameters `package` declares.
fn clock_params(package: &Package) -> ClockParams {
    let declares = |wanted: &str| package.manifest.params.iter().any(|param| param.name == wanted);
    ClockParams { time: declares(CLIP_TIME), duration: declares(CLIP_DURATION) }
}

impl ClockParams {
    /// The clock parameter names this package declares, which are never taken from a project.
    fn driven(self) -> Vec<&'static str> {
        [(self.time, CLIP_TIME), (self.duration, CLIP_DURATION)].into_iter().filter_map(|(declared, name)| declared.then_some(name)).collect()
    }

    /// `params` without the driven ones, so a stored value is ignored as a stored `progress` is.
    /// Borrows when nothing has to go, the usual case: this runs for every effect of every frame.
    fn user_params<'a>(self, params: &'a Map<String, Value>) -> Cow<'a, Map<String, Value>> {
        let driven = self.driven();
        if !driven.iter().any(|name| params.contains_key(*name)) {
            return Cow::Borrowed(params);
        }
        Cow::Owned(params.iter().filter(|(name, _)| !driven.contains(&name.as_str())).map(|(name, value)| (name.clone(), value.clone())).collect())
    }
}

/// Timeline ticks as the seconds a shader is given.
fn seconds(time: Time) -> f64 {
    time.ticks() as f64 / TICKS_PER_SECOND as f64
}

/// Gives each effect of a built clip graph the clock of the clip it runs on.
fn set_clip_clock(graph: &mut EffectGraph, clocks: &[ClockParams], effects: &[ProjectEffect], clock: ClipClock) -> Result<(), String> {
    for (index, (declared, effect)) in clocks.iter().zip(effects).enumerate() {
        for (is_declared, name, value) in [(declared.time, CLIP_TIME, clock.elapsed), (declared.duration, CLIP_DURATION, clock.duration)] {
            if is_declared {
                graph.set_param(&format!("fx{index}.{name}"), &[seconds(value)]).map_err(|e| format!("package {}: setting {name} failed: {e}", effect.package))?;
            }
        }
    }
    Ok(())
}

/// What a built graph is tied to: the package, which parameters were set (one left out must go
/// back to its default) and every text value, since an expression needs new shaders. Numbers are
/// not part of it, so dragging a slider only updates the uniforms.
fn graph_shape(package: &str, params: &Map<String, Value>) -> String {
    let mut shape = package.to_owned();
    for (name, value) in params {
        shape.push('\0');
        shape.push_str(name);
        if let Value::String(text) = value {
            shape.push('=');
            shape.push_str(text);
        }
    }
    shape
}

/// The numbers of `wanted` that differ from what the graph was last given, as (name, numbers).
fn changed_numbers<'a>(applied: &Map<String, Value>, wanted: &'a Map<String, Value>) -> Vec<(&'a str, Result<Vec<f64>, String>)> {
    wanted
        .iter()
        .filter(|(name, value)| applied.get(name.as_str()) != Some(*value))
        .filter_map(|(name, value)| match param_numbers(value) {
            Ok(Some(numbers)) => Some((name.as_str(), Ok(numbers))),
            Ok(None) => None,
            Err(message) => Some((name.as_str(), Err(message))),
        })
        .collect()
}

/// Brings a built graph from `applied` to `wanted`. `prefix` selects the effect inside the graph.
fn update_graph_params(graph: &mut EffectGraph, prefix: &str, package: &str, applied: &Map<String, Value>, wanted: &Map<String, Value>) -> Result<(), String> {
    for (name, numbers) in changed_numbers(applied, wanted) {
        let set = numbers.and_then(|numbers| graph.set_param(&format!("{prefix}{name}"), &numbers).map_err(|e| e.to_string()));
        set.map_err(|e| format!("package {package}, parameter {name}: {e}"))?;
    }
    Ok(())
}

/// Whether a graph whose numbers were just updated can no longer run: gpufx fixes the sizes its
/// size functions compute, and refuses to go on when a parameter they read has changed. Costs no
/// GPU work when the sizes are already known, and tells before any fence is staged. For new input
/// sizes a shader computes them with a submit, hence the gate.
fn needs_rebuild(gpu: &Gpu, graph: &EffectGraph, pads: &[(u32, u32)]) -> bool {
    let _gate = gpu.lock_submits();
    graph.output_size(0, pads).is_err()
}

/// A built clip graph and the parameters it has now.
struct ClipGraph {
    graph: EffectGraph,
    applied: Vec<Map<String, Value>>,
    /// One per effect, in order.
    clocks: Vec<ClockParams>,
}

struct TransitionGraph {
    graph: EffectGraph,
    applied: Map<String, Value>,
}

/// Implements [`Compositor`] on a [`Gpu`]. Single threaded, one per [`Gpu`] (see the crate docs).
pub struct D3d12Compositor {
    gpu: Arc<Gpu>,
    paths: Arc<ProjectPaths>,
    width: u32,
    height: u32,
    images: Vec<Image>,
    free: Vec<usize>,
    /// Images handed back through [`D3d12Compositor::recycle`], ready to be reused.
    spares: Vec<Image>,
    clip_graphs: ClipCache<[String], ClipGraph>,
    transition_graphs: HashMap<String, TransitionGraph>,
    overlay: EffectGraph,
    /// The overlay that puts a clip at its place; its numbers change clip by clip.
    placer: EffectGraph,
    /// Draws a clip that is turned. Built the first time one is.
    rotator: Option<EffectGraph>,
    /// A fully transparent image, only ever read. Not in the pool, whose images hold old pictures.
    transparent: Option<ImageId>,
    stills: HashMap<StillKey, CachedStill>,
    stills_used: u64,
    /// Draws text clips. The shared system one unless set, taken the first time text is drawn.
    text_rasterizer: Option<Arc<Mutex<TextRasterizer>>>,
    /// The size each text has at scale 1, so a text in the cache is found without drawing it again
    /// to learn the scale it needs.
    text_sizes: HashMap<StillKey, (u32, u32)>,
    /// How many times text was rasterized.
    text_rasterizations: u64,
    /// The font families text asked for and did not find, so the default font drew it.
    missing_font_families: Vec<String>,
    readback: Option<wgpu::Buffer>,
    /// Passed to every decoder this compositor opens; see [`SequentialDecoder::set_abort`].
    abort: Option<(Arc<AtomicBool>, Duration)>,
    /// Passed to every decoder with the clock: closing cancels a decode at once.
    closing: Option<Arc<AtomicBool>>,
    /// Marked by [`D3d12Compositor::publish`]; a decode is cancelled by a newer request only while it is fresh.
    publish_clock: Arc<PublishClock>,
}

impl D3d12Compositor {
    /// A compositor writing images of `width` x `height`.
    pub fn new(gpu: Arc<Gpu>, paths: Arc<ProjectPaths>, width: u32, height: u32) -> Result<Self, String> {
        let overlay = load_overlay(&gpu.context, &paths)?;
        let placer = load_overlay(&gpu.context, &paths)?;
        Ok(Self { gpu, paths, width, height, images: Vec::new(), free: Vec::new(), spares: Vec::new(), clip_graphs: ClipCache::default(), transition_graphs: HashMap::new(), overlay, placer, rotator: None, transparent: None, stills: HashMap::new(), stills_used: 0, text_rasterizer: None, text_sizes: HashMap::new(), text_rasterizations: 0, missing_font_families: Vec::new(), readback: None, abort: None, closing: None, publish_clock: Arc::default() })
    }

    /// Makes `closing` stop the decoders opened from now on at once, and lets a newer request stop
    /// them only while a frame was published lately. Call after [`Self::set_abort`].
    pub fn set_closing(&mut self, closing: Arc<AtomicBool>) {
        self.closing = Some(closing);
    }

    /// Lets `flag` stop the decoders opened from now on; see [`SequentialDecoder::set_abort`].
    pub fn set_abort(&mut self, flag: Arc<AtomicBool>, grace: Duration) {
        self.abort = Some((flag, grace));
    }

    /// Runs `work` with no abort flag, so what it decodes is not stopped by a newer preview request.
    /// The closing flag still stops it.
    pub fn without_abort<R>(&mut self, work: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.abort.take();
        let result = work(self);
        self.abort = saved;
        result
    }

    fn acquire(&mut self) -> Result<ImageId, String> {
        if let Some(index) = self.free.pop() {
            return Ok(ImageId(index));
        }
        self.images.push(Image::create(&self.gpu, self.width, self.height)?);
        Ok(ImageId(self.images.len() - 1))
    }

    fn info(at: FrameContext) -> FrameInfo {
        // Seconds for shaders only; the timeline itself stays in integer ticks.
        FrameInfo::new(at.time.ticks() as f64 / engine::TICKS_PER_SECOND as f64, at.index as u64)
    }

    /// Waits for `image` and takes it out of the pool for use outside the compositor, leaving
    /// another image in its slot. `None` for the blank image. The caller still releases `image`.
    pub fn publish(&mut self, image: ImageId) -> Result<Option<SharedImage>, String> {
        if image == BLANK_IMAGE {
            return Ok(None);
        }
        self.images[image.0].wait()?;
        self.publish_clock.mark();
        let replacement = match self.spares.pop() {
            Some(spare) => spare,
            None => Image::create(&self.gpu, self.width, self.height)?,
        };
        Ok(Some(SharedImage { image: std::mem::replace(&mut self.images[image.0], replacement) }))
    }

    /// Takes back a published image for reuse. Every GPU use of it must have finished.
    pub fn recycle(&mut self, shared: SharedImage) {
        if shared.image.width == self.width && shared.image.height == self.height && self.spares.len() < MAX_SPARE_IMAGES {
            self.spares.push(shared.image);
        }
    }

    /// Runs `graph` into the image `output` and records the fence value it will reach.
    fn run_graph(gpu: &Gpu, graph: &mut EffectGraph, inputs: &mut [D3d12Frame], images: &mut [Image], output: ImageId, width: u32, height: u32, info: &FrameInfo) -> Result<(), String> {
        let mut outputs = [images[output.0].frame(width, height)];
        let pads: Vec<(u32, u32)> = inputs.iter().map(|frame| (frame.width, frame.height)).collect();
        // A shader that computes an output size submits once for new input sizes, so it happens
        // here, under the gate; the call below then finds the answer. A failure ends the step here:
        // the answer is not kept, so the call below would compute it again outside the gate.
        {
            let _gate = gpu.lock_submits();
            graph.output_size(0, &pads).map_err(|e| format!("computing the output size failed: {e}"))?;
        }
        // SAFETY: the resources and fences are live objects of the shared device, in COMMON once
        // their fences reach the values passed, and not reused by others before the fences signal.
        // A submit from another thread must not consume the fences this call stages: the submit
        // gate is held for that part only, so building pipelines does not stop the UI.
        unsafe { graph.process_d3d12_with_submit_lock(inputs, &mut outputs, Some(info), || gpu.lock_submits()) }.map_err(|e| format!("process_d3d12 failed: {e}"))?;
        images[output.0].fence_value = outputs[0].fence_value;
        images[output.0].premultiplied = false;
        Ok(())
    }

    /// The still of `path`, decoded once and kept on the GPU while the file stays the same.
    fn load_still(&mut self, path: &Path) -> Result<Arc<StillImage>, String> {
        let metadata = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let stamp = (metadata.len(), metadata.modified().ok());
        self.stills_used += 1;
        let key = StillKey::File(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        if let Some(cached) = self.stills.get_mut(&key) {
            if cached.stamp == stamp {
                cached.last_used = self.stills_used;
                return Ok(cached.still.clone());
            }
        }
        let picture = read_still(path)?;
        self.insert_still(key, stamp, picture)
    }

    /// Draws text clips with `rasterizer` instead of the shared one with the system fonts.
    pub fn set_text_rasterizer(&mut self, rasterizer: Arc<Mutex<TextRasterizer>>) {
        self.text_rasterizer = Some(rasterizer);
    }

    /// How many times this compositor has rasterized text.
    pub fn text_rasterizations(&self) -> u64 {
        self.text_rasterizations
    }

    /// The font families text clips asked for that are not installed, so the default font drew
    /// them; for a warning.
    pub fn missing_font_families(&self) -> &[String] {
        &self.missing_font_families
    }

    fn rasterize(&mut self, text: &TextContent, scale: f32) -> gpufx_text::TextImage {
        let rasterizer = self.text_rasterizer.get_or_insert_with(gpufx_text::shared).clone();
        // A panic in another user leaves the fonts as they were, so the lock is still fine to take.
        let image = rasterizer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).rasterize(text, self.height, scale);
        self.text_rasterizations += 1;
        if let Some(family) = &image.missing_family {
            if !self.missing_font_families.contains(family) {
                eprintln!("text: font family '{family}' is not installed; the default font draws it");
                self.missing_font_families.push(family.clone());
            }
        }
        image
    }

    /// The still of a text clip, rasterized sharp enough for the widest the clip is shown (see
    /// [`engine::text_raster_scale`]) and kept like an image, by its text, output height and scale.
    fn load_text(&mut self, clip: &Clip, text: &TextContent) -> Result<Arc<StillImage>, String> {
        let json = serde_json::to_string(text).map_err(|e| format!("text: {e}"))?;
        let size_key = StillKey::Text { text: json.clone(), output_height: self.height, scale: 1f32.to_bits() };
        let mut natural = None;
        let (width, height) = match self.text_sizes.get(&size_key) {
            Some(size) => *size,
            None => {
                let image = self.rasterize(text, 1.0);
                let size = (image.width, image.height);
                if self.text_sizes.len() >= 4096 {
                    self.text_sizes.clear();
                }
                self.text_sizes.insert(size_key, size);
                natural = Some(image);
                size
            }
        };
        let source_aspect = if width == 0 || height == 0 { 1.0 } else { f64::from(width) / f64::from(height) };
        let output_aspect = f64::from(self.width) / f64::from(self.height.max(1));
        let scale = text_raster_scale(clip.widest(output_aspect, source_aspect), self.width, width);
        let key = StillKey::Text { text: json, output_height: self.height, scale: scale.to_bits() };
        self.stills_used += 1;
        if let Some(cached) = self.stills.get_mut(&key) {
            cached.last_used = self.stills_used;
            return Ok(cached.still.clone());
        }
        let image = match natural {
            Some(image) if scale == 1.0 => image,
            _ => self.rasterize(text, scale),
        };
        let picture = premultiplied_still(image.width, image.height, image.pixels);
        self.insert_still(key, (0, None), picture)
    }

    fn insert_still(&mut self, key: StillKey, stamp: (u64, Option<SystemTime>), picture: StillPicture) -> Result<Arc<StillImage>, String> {
        let image = Image::upload(&self.gpu, &picture)?;
        let bytes = u64::from(picture.width) * u64::from(picture.height) * 4;
        let still = Arc::new(StillImage { aspect: picture.aspect(), has_alpha: picture.has_alpha, fence_value: AtomicU64::new(image.fence_value), image });
        self.stills.insert(key, CachedStill { stamp, still: still.clone(), bytes, last_used: self.stills_used });
        self.trim_stills();
        Ok(still)
    }

    /// Lets go of the stills used longest ago, while over the limit. One on a clip stays.
    fn trim_stills(&mut self) {
        let mut total: u64 = self.stills.values().map(|cached| cached.bytes).sum();
        while total > STILL_CACHE_BYTES {
            let oldest = self.stills.iter().filter(|(_, cached)| Arc::strong_count(&cached.still) == 1).min_by_key(|(_, cached)| cached.last_used).map(|(key, cached)| (key.clone(), cached.bytes));
            let Some((key, bytes)) = oldest else { break };
            self.stills.remove(&key);
            total -= bytes;
        }
    }

    /// The transparent image. A new committed resource is zeroed, and nothing ever writes to it.
    fn transparent_image(&mut self) -> Result<ImageId, String> {
        if let Some(image) = self.transparent {
            return Ok(image);
        }
        self.images.push(Image::create(&self.gpu, self.width, self.height)?);
        let image = ImageId(self.images.len() - 1);
        self.transparent = Some(image);
        Ok(image)
    }

    /// Draws `picture`, a full-frame image of a source of display aspect `source_aspect` (width over height, sample aspect ratio applied), where `transform` says, on a
    /// transparent image. Gives `picture` back when it needs no moving, the usual case, at no cost, unless
    /// `through_alpha` says it has see-through parts, whose colours only count through their alpha.
    /// The overlay mixes with the zero colour of the transparent image, so the result is premultiplied.
    fn place(&mut self, picture: ImageId, transform: ClipTransform, source_aspect: f64, through_alpha: bool, info: &FrameInfo) -> Result<ImageId, String> {
        let (width, height) = (self.width, self.height);
        let rect = transform.rect(f64::from(width) / f64::from(height), source_aspect);
        if rect.fills_frame(width, height) && !through_alpha && transform.opacity >= 1.0 && !transform.rotates {
            return Ok(picture);
        }
        // A clip that turns at any moment takes this path at every frame, so the way it is drawn does
        // not change at the frames where it happens to be upright.
        if transform.rotates {
            return self.place_rotated(picture, rect, transform, info);
        }
        let background = self.transparent_image()?;
        for (name, value) in [("x", rect.x), ("y", rect.y), ("scale", rect.width), ("height", rect.height), ("opacity", transform.opacity)] {
            self.placer.set_param(name, &[value]).map_err(|e| format!("overlay.{name}: {e}"))?;
        }
        let output = self.acquire()?;
        let mut inputs = [self.images[background.0].frame(width, height), self.images[picture.0].frame(width, height)];
        if let Err(message) = Self::run_graph(&self.gpu, &mut self.placer, &mut inputs, &mut self.images, output, width, height, info) {
            self.release_image(output);
            return Err(message);
        }
        self.images[background.0].fence_value = inputs[0].fence_value;
        self.images[picture.0].fence_value = inputs[1].fence_value;
        self.images[output.0].premultiplied = true;
        self.release_image(picture);
        Ok(output)
    }

    /// Draws `picture` turned around the middle of `rect`, in premultiplied colours on transparent.
    fn place_rotated(&mut self, picture: ImageId, rect: engine::PlacedRect, transform: ClipTransform, info: &FrameInfo) -> Result<ImageId, String> {
        let (width, height) = (self.width, self.height);
        let rotator = match &mut self.rotator {
            Some(rotator) => rotator,
            empty => empty.insert(load_rotator(&self.gpu.context)?),
        };
        for (name, value) in [("x", rect.x), ("y", rect.y), ("width", rect.width), ("height", rect.height), ("rotation", transform.rotation), ("opacity", transform.opacity)] {
            rotator.set_param(name, &[value]).map_err(|e| format!("rotated place.{name}: {e}"))?;
        }
        let output = self.acquire()?;
        let rotator = self.rotator.as_mut().expect("built above");
        let mut inputs = [self.images[picture.0].frame(width, height)];
        if let Err(message) = Self::run_graph(&self.gpu, rotator, &mut inputs, &mut self.images, output, width, height, info) {
            self.release_image(output);
            return Err(message);
        }
        self.images[picture.0].fence_value = inputs[0].fence_value;
        self.images[output.0].premultiplied = true;
        self.release_image(picture);
        Ok(output)
    }

    /// `output` if wgpu reported nothing, otherwise an error, with `output` given back.
    fn checked_output(&mut self, output: ImageId) -> Result<ImageId, RenderError> {
        let check = self.gpu.check_errors();
        keep_output_unless(check, output, |image| self.release_image(image))
    }

    fn release_image(&mut self, image: ImageId) {
        if image != BLANK_IMAGE {
            self.free.push(image.0);
        }
    }
}

/// `output` when `check` passed. Otherwise the image goes to `release`, because the caller gets no
/// handle to release (the contract of [`Compositor`]).
fn keep_output_unless<I>(check: Result<(), String>, output: I, release: impl FnOnce(I)) -> Result<I, RenderError> {
    match check {
        Ok(()) => Ok(output),
        Err(message) => {
            release(output);
            Err(RenderError(message))
        }
    }
}

/// Reads the input of a clip, then takes its output image. The input comes first: an unusable frame
/// must not cost an image, which nothing could give back.
fn input_then_output<F, I>(input: impl FnOnce() -> Result<F, String>, output: impl FnOnce() -> Result<I, String>) -> Result<(F, I), String> {
    let input = input()?;
    Ok((input, output()?))
}

fn package_path(paths: &ProjectPaths, package: &str) -> Result<std::path::PathBuf, String> {
    paths.package_path(package).ok_or_else(|| format!("package {package} not found"))
}

fn open_effect(context: &Arc<GpuContext>, path: &Path, package: &str) -> Result<Effect, String> {
    let opened = Package::open(path).map_err(|e| format!("package {package}: {e}"))?;
    Effect::new(context.clone(), opened).map_err(|e| format!("package {package}: {e}"))
}

/// `scale` to the project size, then the clip's effects, as one graph, and which clock parameters
/// each effect declares.
/// With `restore_alpha`, the scaled source also goes around the effects, and their result takes its alpha.
fn build_clip_graph(context: &Arc<GpuContext>, paths: &ProjectPaths, width: u32, height: u32, effects: &[ProjectEffect], restore_alpha: bool) -> Result<(EffectGraph, Vec<ClockParams>), String> {
    let scale_package = package_path(paths, "packages/gpufx/ffmpeg/scale")?;
    let mut pending = Vec::with_capacity(effects.len() + 1);
    let mut scale = open_effect(context, &scale_package, "scale")?;
    scale.params_mut().set("w", &[f64::from(width)]).map_err(|e| format!("scale.w: {e}"))?;
    scale.params_mut().set("h", &[f64::from(height)]).map_err(|e| format!("scale.h: {e}"))?;
    pending.push(("scale".to_owned(), scale));
    let mut clocks = Vec::with_capacity(effects.len());
    for (index, project_effect) in effects.iter().enumerate() {
        let path = package_path(paths, &project_effect.package)?;
        let mut effect = open_effect(context, &path, &project_effect.package)?;
        let clock = clock_params(effect.package());
        apply_params(&mut effect, &project_effect.package, &project_effect.params, &clock.driven())?;
        clocks.push(clock);
        pending.push((format!("fx{index}"), effect));
    }
    let chain = |range: std::ops::Range<usize>| range.map(|i| format!("e{i}")).collect::<Vec<_>>().join("|");
    let text = if restore_alpha {
        let last = pending.len();
        let graph = format!("[0]e0[scaled]; [scaled]split[picture][matte]; [picture]{}[done]; [done][matte]e{last}", chain(1..last));
        pending.push(("alpha".to_owned(), restore_alpha_effect(context)?));
        graph
    } else {
        chain(0..pending.len())
    };
    let mut pending = pending.into_iter();
    let graph = EffectGraph::parse(&text, |_| Ok(pending.next().expect("one effect per item"))).map_err(|e| format!("building the clip graph failed: {e}"))?;
    Ok((graph, clocks))
}

/// With `larger_alpha`, the result takes the larger alpha of the two sides at each pixel.
fn build_transition_graph(context: &Arc<GpuContext>, paths: &ProjectPaths, package: &str, params: &Map<String, Value>, larger_alpha: bool) -> Result<EffectGraph, String> {
    let path = package_path(paths, package)?;
    let mut effect = open_effect(context, &path, package)?;
    apply_params(&mut effect, package, params, &["progress"])?;
    let mut holder = Some(effect);
    let mut merge = if larger_alpha { Some(larger_alpha_effect(context)?) } else { None };
    let text = if larger_alpha { "[0]split[a][a_alpha]; [1]split[b][b_alpha]; [a][b]transition[mixed]; [mixed][a_alpha][b_alpha]alpha" } else { "transition" };
    EffectGraph::parse(text, |name| {
        let effect = if name == "alpha" { merge.take() } else { holder.take() };
        Ok((name.to_owned(), effect.expect("one effect per item")))
    })
    .map_err(|e| format!("package {package}: {e}"))
}

/// Whether `graph` writes an alpha where its inputs have none: tried on transparent images, one per
/// input, every pixel of the result is at least half opaque. A graph that keeps alpha, or moves it or
/// paints only some of the picture, leaves see-through pixels. A picture effect that fails on such a
/// small image says yes: its result would be better cut by the source's alpha than a solid block.
fn writes_alpha(gpu: &Gpu, graph: &mut EffectGraph, input_count: usize, info: &FrameInfo) -> bool {
    // An error from before the trial belongs to the real frame, which is going to fail with it.
    if gpu.check_errors().is_err() {
        return true;
    }
    let mut probe = || -> Result<bool, String> {
        let side = ALPHA_PROBE_SIDE;
        let sources = (0..input_count).map(|_| Image::create(gpu, side, side)).collect::<Result<Vec<_>, _>>()?;
        let mut inputs: Vec<D3d12Frame> = sources.iter().map(|image| image.frame(side, side)).collect();
        let mut outputs = [Image::create(gpu, side, side)?];
        D3d12Compositor::run_graph(gpu, graph, &mut inputs, &mut outputs, ImageId(0), side, side, info)?;
        outputs[0].wait()?;
        let buffer = readback_buffer(gpu, side, side);
        let mut rgba = Vec::new();
        outputs[0].read_rgba(gpu, &buffer, &mut rgba)?;
        gpu.check_errors()?;
        Ok(rgba.chunks_exact(4).all(|pixel| pixel[3] >= 128))
    };
    let result = probe();
    // Whatever went wrong in the trial belongs to the trial, not to the next real frame.
    gpu.take_error();
    result.unwrap_or(true)
}

fn load_overlay(context: &Arc<GpuContext>, paths: &ProjectPaths) -> Result<EffectGraph, String> {
    let path = package_path(paths, "packages/gpufx/ffmpeg/overlay")?;
    let mut effect = open_effect(context, &path, "overlay")?;
    for (name, value) in [("x", 0.0), ("y", 0.0), ("scale", 1.0), ("opacity", 1.0)] {
        effect.params_mut().set(name, &[value]).map_err(|e| format!("overlay.{name}: {e}"))?;
    }
    let mut holder = Some(effect);
    EffectGraph::parse("overlay", |_| Ok(("overlay".to_owned(), holder.take().expect("one effect")))).map_err(|e| format!("overlay graph: {e}"))
}

fn load_rotator(context: &Arc<GpuContext>) -> Result<EffectGraph, String> {
    let mut holder = Some(rotated_place_effect(context)?);
    EffectGraph::parse("place", |_| Ok(("place".to_owned(), holder.take().expect("one effect")))).map_err(|e| format!("rotated place graph: {e}"))
}

impl Compositor for D3d12Compositor {
    type Decoder = SourceDecoder;
    type Image = ImageId;

    fn open_decoder(&mut self, clip: &Clip) -> Result<SourceDecoder, RenderError> {
        if let Some(text) = &clip.text {
            return self.load_text(clip, text).map(SourceDecoder::Still).map_err(RenderError);
        }
        let source = clip.source.as_str();
        if is_image_source(source) {
            let path = self.paths.source_path(source);
            return self.load_still(&path).map(SourceDecoder::Still).map_err(RenderError);
        }
        let mut decoder = SequentialDecoder::open(&self.paths.source_path(source), self.gpu.raw_device())?;
        if let Some((flag, grace)) = &self.abort {
            decoder.set_abort(flag.clone(), *grace);
            decoder.set_abort_policy(self.closing.clone(), Some(self.publish_clock.clone()));
        } else {
            decoder.set_abort_policy(self.closing.clone(), None);
        }
        Ok(SourceDecoder::Video(decoder))
    }

    fn prepare_clip(&mut self, key: ClipKey, frame: &mut <SourceDecoder as Decoder>::Frame, effects: &[ProjectEffect], transform: ClipTransform, clock: ClipClock, at: FrameContext) -> Result<ImageId, RenderError> {
        let info = Self::info(at);
        let (input, output) = input_then_output(|| frame.input(), || self.acquire())?;
        let (width, height) = (self.width, self.height);
        let source_aspect = frame.display_aspect();
        let has_alpha = frame.has_alpha();
        let mut inputs = [input];
        let (gpu, paths) = (&self.gpu, &self.paths);
        // The input format is part of what a graph was built for.
        let mut shape: Vec<String> = effects.iter().map(|effect| graph_shape(&effect.package, &effect.params)).collect();
        if frame.is_still() {
            shape.push("\0still".to_owned());
        }
        if has_alpha {
            shape.push("\0alpha".to_owned());
        }
        let build = || {
            // A clip graph is built once, so this is one trial for one clip, and none for a source without alpha.
            let restore_alpha = has_alpha
                && !effects.is_empty()
                && match build_clip_graph(&gpu.context, paths, ALPHA_PROBE_SIDE, ALPHA_PROBE_SIDE, effects, false) {
                    Ok((mut graph, clocks)) => {
                        let _ = set_clip_clock(&mut graph, &clocks, effects, clock);
                        writes_alpha(gpu, &mut graph, 1, &info)
                    }
                    Err(_) => true,
                };
            build_clip_graph(&gpu.context, paths, width, height, effects, restore_alpha).map(|(graph, clocks)| {
                let applied = clocks.iter().zip(effects).map(|(clock, effect)| clock.user_params(&effect.params).into_owned()).collect();
                ClipGraph { graph, applied, clocks }
            })
        };
        let pads = [(inputs[0].width, inputs[0].height)];
        let (clip_graphs, images) = (&mut self.clip_graphs, &mut self.images);
        let mut render = || -> Result<(), String> {
            let built = clip_graphs.get_or_build(key, shape.as_slice(), &build)?;
            let mut changed = false;
            let mut failure = None;
            for (index, effect) in effects.iter().enumerate() {
                let wanted = built.clocks[index].user_params(&effect.params);
                changed |= built.applied[index] != *wanted;
                if let Err(message) = update_graph_params(&mut built.graph, &format!("fx{index}."), &effect.package, &built.applied[index], &wanted) {
                    failure = Some(message);
                    break;
                }
                built.applied[index].clone_from(&wanted);
            }
            // A failure part way may have left an earlier size parameter changed, and `applied`
            // then hides that from the next frame, so the graph goes as the transition's does.
            if let Some(message) = failure {
                clip_graphs.retain(|entry| entry != key);
                return Err(message);
            }
            if changed && needs_rebuild(gpu, &built.graph, &pads) {
                clip_graphs.retain(|entry| entry != key);
            }
            let built = clip_graphs.get_or_build(key, shape.as_slice(), &build)?;
            set_clip_clock(&mut built.graph, &built.clocks, effects, clock)?;
            Self::run_graph(gpu, &mut built.graph, &mut inputs, images, output, width, height, &info)
        };
        let result = render();
        if let Err(e) = result {
            self.release_image(output);
            return Err(RenderError(e));
        }
        frame.raise_fence_value(inputs[0].fence_value);
        let placed = match self.place(output, transform, source_aspect, has_alpha, &info) {
            Ok(placed) => placed,
            Err(message) => {
                self.release_image(output);
                return Err(RenderError(message));
            }
        };
        self.checked_output(placed)
    }

    fn transition(&mut self, package: &str, params: &Map<String, Value>, from: ImageId, to: ImageId, progress: Progress, at: FrameContext) -> Result<ImageId, RenderError> {
        let see_through = self.images[from.0].premultiplied || self.images[to.0].premultiplied;
        let mut graph_key = graph_shape(package, params);
        if see_through {
            graph_key.push_str("\0alpha");
        }
        let info = Self::info(at);
        let value = progress.elapsed.ticks() as f64 / progress.total.ticks() as f64;
        let (width, height) = (self.width, self.height);
        let pads = [(width, height), (width, height)];
        // Takes the graph out of the map, so a failed update never leaves a half-updated one.
        let usable = self.transition_graphs.remove(&graph_key).and_then(|mut built| {
            let changed = built.applied != *params;
            let updated = update_graph_params(&mut built.graph, "", package, &built.applied, params);
            match updated {
                Ok(()) if changed && needs_rebuild(&self.gpu, &built.graph, &pads) => None,
                Ok(()) => {
                    built.applied.clone_from(params);
                    Some(Ok(built))
                }
                Err(message) => Some(Err(message)),
            }
        });
        let mut built = match usable {
            Some(result) => result?,
            None => {
                let larger_alpha = see_through && {
                    // One trial per graph, halfway through, and none between two opaque pictures.
                    let mut trial = build_transition_graph(&self.gpu.context, &self.paths, package, params, false)?;
                    trial.set_param("progress", &[0.5]).map_err(|e| format!("package {package}: setting progress failed: {e}"))?;
                    writes_alpha(&self.gpu, &mut trial, 2, &info)
                };
                TransitionGraph { graph: build_transition_graph(&self.gpu.context, &self.paths, package, params, larger_alpha)?, applied: params.clone() }
            }
        };
        let output = self.acquire()?;
        let mut inputs = [self.images[from.0].frame(width, height), self.images[to.0].frame(width, height)];
        let result = built
            .graph
            .set_param("progress", &[value])
            .map_err(|e| format!("package {package}: setting progress failed: {e}"))
            .and_then(|()| Self::run_graph(&self.gpu, &mut built.graph, &mut inputs, &mut self.images, output, width, height, &info));
        self.transition_graphs.insert(graph_key, built);
        if let Err(e) = result {
            self.release_image(output);
            return Err(RenderError(e));
        }
        self.images[from.0].fence_value = inputs[0].fence_value;
        self.images[to.0].fence_value = inputs[1].fence_value;
        // Both sides are read as they are, premultiplied or opaque, so what they mix to is premultiplied.
        self.images[output.0].premultiplied = see_through;
        self.checked_output(output)
    }

    fn composite(&mut self, base: ImageId, top: ImageId, at: FrameContext) -> Result<ImageId, RenderError> {
        let info = Self::info(at);
        let output = self.acquire()?;
        let (width, height) = (self.width, self.height);
        // The base is what stands behind, taken as it is. Only the top is mixed through its alpha, so
        // only its premultiplied colours have to be turned back for the overlay.
        let mut inputs = [self.images[base.0].frame(width, height), self.images[top.0].composited_top(width, height)];
        let result = Self::run_graph(&self.gpu, &mut self.overlay, &mut inputs, &mut self.images, output, width, height, &info);
        if let Err(e) = result {
            self.release_image(output);
            return Err(RenderError(e));
        }
        self.images[base.0].fence_value = inputs[0].fence_value;
        self.images[top.0].fence_value = inputs[1].fence_value;
        self.checked_output(output)
    }

    fn blank(&mut self) -> Result<ImageId, RenderError> {
        Ok(BLANK_IMAGE)
    }

    fn read(&mut self, image: ImageId, rgba: &mut Vec<u8>) -> Result<(), RenderError> {
        let (width, height) = (self.width as usize, self.height as usize);
        if image == BLANK_IMAGE {
            rgba.clear();
            rgba.resize(width * height * 4, 0);
            rgba.iter_mut().skip(3).step_by(4).for_each(|alpha| *alpha = 255);
            return Ok(());
        }
        let gpu = self.gpu.clone();
        let source = &self.images[image.0];
        source.wait()?;
        let buffer = self.readback.get_or_insert_with(|| readback_buffer(&gpu, source.width, source.height));
        source.read_rgba(&gpu, buffer, rgba)?;
        gpu.check_errors()?;
        Ok(())
    }

    fn release(&mut self, image: ImageId) {
        self.release_image(image);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;
    use std::sync::Barrier;
    use std::time::Instant;

    fn params(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    /// The hardware device and the overlay graph, or `None` when the machine has no such device.
    fn overlay_on_hardware() -> Option<(Gpu, EffectGraph)> {
        let Some(gpu) = Gpu::create_hardware().expect("device") else {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but there is no hardware adapter");
            return None;
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/FFmpegGpuFilter");
        let overlay = load_overlay(&gpu.context, &ProjectPaths::new(Path::new("project.json"), vec![root])).expect("the overlay package");
        Some((gpu, overlay))
    }

    #[test]
    fn the_rebuild_check_waits_for_the_submit_gate() {
        const HELD: Duration = Duration::from_millis(200);
        let Some((gpu, overlay)) = overlay_on_hardware() else { return };
        let taken = Barrier::new(2);
        let waited = std::thread::scope(|scope| {
            scope.spawn(|| {
                let _gate = gpu.lock_submits();
                taken.wait();
                std::thread::sleep(HELD);
            });
            taken.wait();
            let started = Instant::now();
            needs_rebuild(&gpu, &overlay, &[(16, 16), (16, 16)]);
            started.elapsed()
        });
        assert!(waited >= HELD / 2, "the check ran while the gate was held, after {waited:?}");
    }

    #[test]
    fn a_failing_output_size_ends_the_step_before_gpufx_runs() {
        let Some((gpu, mut overlay)) = overlay_on_hardware() else { return };
        let mut images = vec![Image::create(&gpu, 16, 16).expect("an image")];
        // The overlay takes two inputs, so asking for its size with one fails.
        let mut inputs = [images[0].frame(16, 16)];
        let error = D3d12Compositor::run_graph(&gpu, &mut overlay, &mut inputs, &mut images, ImageId(0), 16, 16, &FrameInfo::new(0.0, 0)).expect_err("one input is refused");
        assert!(error.starts_with("computing the output size failed"), "{error}");
    }

    #[test]
    fn a_stored_clock_value_is_dropped_only_where_the_package_declares_it() {
        let stored = params(json!({ "clip_time": 9, "clip_duration": 9, "amp": 1 }));
        let both = ClockParams { time: true, duration: true };
        assert_eq!(both.user_params(&stored).into_owned(), params(json!({ "amp": 1 })));
        let only_time = ClockParams { time: true, duration: false };
        assert_eq!(only_time.user_params(&stored).into_owned(), params(json!({ "clip_duration": 9, "amp": 1 })));
        assert_eq!(ClockParams::default().user_params(&stored).into_owned(), stored, "a package without a clock keeps today's behaviour");
    }

    #[test]
    fn a_number_change_keeps_the_graph_shape() {
        assert_eq!(graph_shape("blur", &params(json!({ "radius": 2 }))), graph_shape("blur", &params(json!({ "radius": 40.5 }))));
        assert_eq!(graph_shape("c", &params(json!({ "tint": [1, 0, 0, 1] }))), graph_shape("c", &params(json!({ "tint": [0, 1, 0, 1] }))));
    }

    #[test]
    fn a_new_package_name_set_or_text_rebuilds_the_graph() {
        let base = graph_shape("blur", &params(json!({ "radius": 2 })));
        assert_ne!(base, graph_shape("sharpen", &params(json!({ "radius": 2 }))));
        assert_ne!(base, graph_shape("blur", &params(json!({}))), "a parameter left out goes back to its default");
        assert_ne!(graph_shape("g", &params(json!({ "expr": "x" }))), graph_shape("g", &params(json!({ "expr": "y" }))), "an expression needs new shaders");
    }

    #[test]
    fn only_changed_numbers_are_sent_to_the_graph() {
        let applied = params(json!({ "a": 1, "b": true, "c": [1, 2], "text": "x" }));
        let wanted = params(json!({ "a": 1, "b": false, "c": [1, 3], "text": "x" }));
        let changed: Vec<(&str, Vec<f64>)> = changed_numbers(&applied, &wanted).into_iter().map(|(name, numbers)| (name, numbers.unwrap())).collect();
        assert_eq!(changed, vec![("b", vec![0.0]), ("c", vec![1.0, 3.0])]);
    }

    #[test]
    fn a_value_that_is_not_a_number_is_reported() {
        let wanted = params(json!({ "a": { "nested": 1 } }));
        let changed = changed_numbers(&Map::new(), &wanted);
        assert!(changed[0].1.is_err());
    }

    #[test]
    fn a_failed_check_releases_the_output() {
        let released = Cell::new(None);
        let result = keep_output_unless(Err("wgpu error".to_owned()), 7, |image| released.set(Some(image)));
        assert_eq!(result, Err(RenderError("wgpu error".to_owned())));
        assert_eq!(released.get(), Some(7), "the caller has no handle to release");
    }

    #[test]
    fn an_unusable_input_takes_no_output_image() {
        let taken = Cell::new(false);
        let result = input_then_output(
            || Err::<i32, _>("unsupported colorspace".to_owned()),
            || {
                taken.set(true);
                Ok(7)
            },
        );
        assert_eq!(result, Err("unsupported colorspace".to_owned()));
        assert!(!taken.get(), "the image would never be released");
    }

    #[test]
    fn a_usable_input_gets_its_output_image() {
        assert_eq!(input_then_output(|| Ok::<_, String>(1), || Ok(7)), Ok((1, 7)));
    }

    #[test]
    fn a_passed_check_keeps_the_output() {
        let released = Cell::new(None);
        let result = keep_output_unless(Ok(()), 7, |image: i32| released.set(Some(image)));
        assert_eq!(result, Ok(7));
        assert_eq!(released.get(), None);
    }
}
