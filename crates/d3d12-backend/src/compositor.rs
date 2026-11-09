//! Compositing with gpufx on the shared D3D12 device.
//!
//! Every image is a raw `R8G8B8A8_UNORM` D3D12 resource with its own fence, in COMMON between
//! users, so every step is one `process_d3d12` call and gpufx orders the GPU work with the fences.
//! Decoded frames go through a `scale|effects...` graph per clip; transitions and track compositing
//! are graphs on those images. The CPU waits only when reading the finished image.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use engine::render::{ClipCache, ClipKey, Compositor, Decoder, FrameContext, RenderError};
use engine::schedule::Progress;
use engine::Effect as ProjectEffect;
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

use crate::decoder::{DecodedFrame, PublishClock, SequentialDecoder};
use crate::gpu::Gpu;
use crate::paths::ProjectPaths;

const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MAX_SPARE_IMAGES: usize = 3;

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
        Ok(Self { resource, fence, fence_value: 0, barrier_texture, copy_source, view, width, height })
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

/// Sets the static `params` of a project entry on `effect`. `skip` names parameters set later.
fn apply_params(effect: &mut Effect, package: &str, params: &Map<String, Value>, skip: &[&str]) -> Result<(), String> {
    for (name, value) in params {
        if skip.contains(&name.as_str()) {
            continue;
        }
        let result = match value {
            Value::Number(n) => n.as_f64().map_or_else(|| Err(format!("not a number: {n}")), |v| effect.params_mut().set(name, &[v]).map_err(|e| e.to_string())),
            Value::Bool(b) => effect.params_mut().set(name, &[f64::from(u8::from(*b))]).map_err(|e| e.to_string()),
            Value::String(s) => effect.params_mut().set_str(name, s).map_err(|e| e.to_string()),
            Value::Array(items) => {
                let values: Option<Vec<f64>> = items.iter().map(Value::as_f64).collect();
                values.map_or_else(|| Err("array items must be numbers".to_owned()), |v| effect.params_mut().set(name, &v).map_err(|e| e.to_string()))
            }
            other => Err(format!("unsupported value {other}")),
        };
        result.map_err(|e| format!("package {package}, parameter {name}: {e}"))?;
    }
    Ok(())
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
    clip_graphs: ClipCache<[ProjectEffect], EffectGraph>,
    transition_graphs: HashMap<String, EffectGraph>,
    overlay: EffectGraph,
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
        Ok(Self { gpu, paths, width, height, images: Vec::new(), free: Vec::new(), spares: Vec::new(), clip_graphs: ClipCache::default(), transition_graphs: HashMap::new(), overlay, readback: None, abort: None, closing: None, publish_clock: Arc::default() })
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
        // A submit from another thread must not consume the fences this call stages.
        let _gate = gpu.lock_submits();
        // SAFETY: the resources and fences are live objects of the shared device, in COMMON once
        // their fences reach the values passed, and not reused by others before the fences signal.
        // The submit gate is held, so no submit overlaps this call.
        unsafe { graph.process_d3d12(inputs, &mut outputs, Some(info)) }.map_err(|e| format!("process_d3d12 failed: {e}"))?;
        images[output.0].fence_value = outputs[0].fence_value;
        Ok(())
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

/// `scale` to the project size, then the clip's effects, as one graph.
fn build_clip_graph(context: &Arc<GpuContext>, paths: &ProjectPaths, width: u32, height: u32, effects: &[ProjectEffect]) -> Result<EffectGraph, String> {
    let scale_package = package_path(paths, "packages/gpufx/ffmpeg/scale")?;
    let mut pending = Vec::with_capacity(effects.len() + 1);
    let mut scale = open_effect(context, &scale_package, "scale")?;
    scale.params_mut().set("w", &[f64::from(width)]).map_err(|e| format!("scale.w: {e}"))?;
    scale.params_mut().set("h", &[f64::from(height)]).map_err(|e| format!("scale.h: {e}"))?;
    pending.push(("scale".to_owned(), scale));
    for (index, project_effect) in effects.iter().enumerate() {
        let path = package_path(paths, &project_effect.package)?;
        let mut effect = open_effect(context, &path, &project_effect.package)?;
        apply_params(&mut effect, &project_effect.package, &project_effect.params, &[])?;
        pending.push((format!("fx{index}"), effect));
    }
    let text = (0..pending.len()).map(|i| format!("e{i}")).collect::<Vec<_>>().join("|");
    let mut pending = pending.into_iter();
    EffectGraph::parse(&text, |_| Ok(pending.next().expect("one effect per item"))).map_err(|e| format!("building the clip graph failed: {e}"))
}

fn build_transition_graph(context: &Arc<GpuContext>, paths: &ProjectPaths, package: &str, params: &Map<String, Value>) -> Result<EffectGraph, String> {
    let path = package_path(paths, package)?;
    let mut effect = open_effect(context, &path, package)?;
    apply_params(&mut effect, package, params, &["progress"])?;
    let mut holder = Some(effect);
    EffectGraph::parse("transition", |_| Ok(("transition".to_owned(), holder.take().expect("one effect")))).map_err(|e| format!("package {package}: {e}"))
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

impl Compositor for D3d12Compositor {
    type Decoder = SequentialDecoder;
    type Image = ImageId;

    fn open_decoder(&mut self, source: &str) -> Result<SequentialDecoder, RenderError> {
        let mut decoder = SequentialDecoder::open(&self.paths.source_path(source), self.gpu.raw_device())?;
        if let Some((flag, grace)) = &self.abort {
            decoder.set_abort(flag.clone(), *grace);
            decoder.set_abort_policy(self.closing.clone(), Some(self.publish_clock.clone()));
        }
        Ok(decoder)
    }

    fn prepare_clip(&mut self, key: ClipKey, frame: &mut <SequentialDecoder as Decoder>::Frame, effects: &[ProjectEffect], at: FrameContext) -> Result<ImageId, RenderError> {
        let info = Self::info(at);
        let (input, output) = input_then_output(|| decoded_input(frame), || self.acquire())?;
        let (width, height) = (self.width, self.height);
        let mut inputs = [input];
        let (gpu, paths) = (&self.gpu, &self.paths);
        let result = self
            .clip_graphs
            .get_or_build(key, effects, || build_clip_graph(&gpu.context, paths, width, height, effects))
            .and_then(|graph| Self::run_graph(gpu, graph, &mut inputs, &mut self.images, output, width, height, &info));
        if let Err(e) = result {
            self.release_image(output);
            return Err(RenderError(e));
        }
        frame.raise_fence_value(inputs[0].fence_value);
        self.checked_output(output)
    }

    fn transition(&mut self, package: &str, params: &Map<String, Value>, from: ImageId, to: ImageId, progress: Progress, at: FrameContext) -> Result<ImageId, RenderError> {
        let graph_key = format!("{package}\0{}", serde_json::to_string(params).map_err(|e| RenderError(e.to_string()))?);
        if !self.transition_graphs.contains_key(&graph_key) {
            let graph = build_transition_graph(&self.gpu.context, &self.paths, package, params)?;
            self.transition_graphs.insert(graph_key.clone(), graph);
        }
        let info = Self::info(at);
        let value = progress.elapsed.ticks() as f64 / progress.total.ticks() as f64;
        let output = self.acquire()?;
        let (width, height) = (self.width, self.height);
        let mut inputs = [self.images[from.0].frame(width, height), self.images[to.0].frame(width, height)];
        let result = {
            let images = &mut self.images;
            let graph = self.transition_graphs.get_mut(&graph_key).expect("graph exists");
            match graph.set_param("progress", &[value]) {
                Ok(()) => Self::run_graph(&self.gpu, graph, &mut inputs, images, output, width, height, &info),
                Err(e) => Err(format!("package {package}: setting progress failed: {e}")),
            }
        };
        if let Err(e) = result {
            self.release_image(output);
            return Err(RenderError(e));
        }
        self.images[from.0].fence_value = inputs[0].fence_value;
        self.images[to.0].fence_value = inputs[1].fence_value;
        self.checked_output(output)
    }

    fn composite(&mut self, base: ImageId, top: ImageId, at: FrameContext) -> Result<ImageId, RenderError> {
        let info = Self::info(at);
        let output = self.acquire()?;
        let (width, height) = (self.width, self.height);
        let mut inputs = [self.images[base.0].frame(width, height), self.images[top.0].frame(width, height)];
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
    use std::cell::Cell;

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
