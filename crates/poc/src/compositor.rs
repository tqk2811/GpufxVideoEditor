//! Runs the gpufx `mix` transition on two decoded frames into an RGBA texture that the app's
//! wgpu device can draw.
//!
//! `gpufx-core` owns a second wgpu device on the same `ID3D12Device`, so the output texture is a
//! raw D3D12 resource imported into the app's device. The two devices use it in turn, it stays in
//! COMMON between them, and the order is enforced by waiting on the CPU: [`MixCompositor::mix`]
//! returns after the GPU finished the effect, and the caller waits for its draw before the next mix.

use std::ffi::c_void;
use std::sync::Arc;

use gpufx_core::d3d12::D3d12Frame;
use gpufx_core::{ColorInfo, Effect, EffectGraph, FrameInfo, GpuContext, MemoryStats, Package, PixelFormat, PoolStats};
use wgpu::hal::{self, api::Dx12, CommandEncoder as _};
use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12Device, ID3D12Fence, ID3D12Resource, D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_FENCE_FLAG_NONE, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES,
    D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
    D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::decoder::DecodedFrame;
use crate::gpu::AppGpu;

const MIX_PACKAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter/packages/gpufx/capcut/mix");
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The RGBA result, as a D3D12 resource and as a texture of the app's wgpu device.
pub struct OutputTexture {
    resource: ID3D12Resource,
    fence: ID3D12Fence,
    /// The value `fence` reaches when the last mix into this texture is done.
    fence_value: u64,
    width: u32,
    height: u32,
    barrier_texture: hal::dx12::Texture,
    pub view: wgpu::TextureView,
    copy_source: wgpu::Texture,
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d { width, height, depth_or_array_layers: 1 }
}

/// Wraps `resource` in the app's device. Its state in the wgpu tracker is `state` for its whole
/// life, so wgpu records no barrier for it: [`OutputTexture::record_transition`] does that.
unsafe fn import_texture(
    device: &wgpu::Device,
    resource: &ID3D12Resource,
    size: wgpu::Extent3d,
    usage: wgpu::TextureUsages,
    state: wgpu::TextureUses,
) -> wgpu::Texture {
    let hal_texture = hal::dx12::Device::texture_from_raw(resource.clone(), OUTPUT_FORMAT, wgpu::TextureDimension::D2, size, 1, 1);
    device.create_texture_from_hal::<Dx12>(
        hal_texture,
        &wgpu::TextureDescriptor {
            label: Some("poc mix output"),
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

impl OutputTexture {
    fn create(gpu: &AppGpu, device: &ID3D12Device, width: u32, height: u32) -> Result<Self, String> {
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
        let mut created: Option<ID3D12Resource> = None;
        // SAFETY: the descriptions are valid and outlive the calls; `created` receives the resource.
        let fence: ID3D12Fence = unsafe {
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &description, D3D12_RESOURCE_STATE_COMMON, None, &mut created)
                .map_err(|e| format!("creating the output texture failed: {e}"))?;
            device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(|e| format!("creating the output fence failed: {e}"))?
        };
        let resource = created.ok_or("CreateCommittedResource returned no resource")?;

        let size = extent(width, height);
        // SAFETY: the resource is valid, stays alive through the clones held by the wrappers,
        // and record_transition puts it in the state the wrapper declares before each use.
        let (display, copy_source, barrier_texture) = unsafe {
            (
                import_texture(&gpu.device, &resource, size, wgpu::TextureUsages::TEXTURE_BINDING, wgpu::TextureUses::RESOURCE),
                import_texture(&gpu.device, &resource, size, wgpu::TextureUsages::COPY_SRC, wgpu::TextureUses::COPY_SRC),
                hal::dx12::Device::texture_from_raw(resource.clone(), OUTPUT_FORMAT, wgpu::TextureDimension::D2, size, 1, 1),
            )
        };
        let view = display.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self { resource, fence, fence_value: 0, width, height, barrier_texture, view, copy_source })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Records the barrier between COMMON (where gpufx leaves and expects the resource) and `state`.
    pub fn record_transition(&self, encoder: &mut wgpu::CommandEncoder, state: wgpu::TextureUses, to_common: bool) {
        let (from, to) = if to_common { (state, wgpu::TextureUses::UNINITIALIZED) } else { (wgpu::TextureUses::UNINITIALIZED, state) };
        // SAFETY: the texture is valid. The caller orders uses with the fence, and the wrappers
        // declare `state`, so the wgpu tracker and the real state agree around the barriers.
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

    /// Blocks until the last mix into this texture is done on the GPU.
    fn wait_for_mix(&self) -> Result<(), String> {
        // SAFETY: a null event makes SetEventOnCompletion block until the value is reached.
        unsafe { self.fence.SetEventOnCompletion(self.fence_value, HANDLE::default()) }.map_err(|e| format!("output fence wait failed: {e}"))
    }

    /// Copies the texture to memory as RGBA8 rows, for verification.
    pub fn read_pixels(&self, gpu: &AppGpu) -> Result<Vec<u8>, String> {
        let bytes_per_row = self.width * 4;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("poc mix read-back"),
            size: u64::from(bytes_per_row) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut before = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("poc read-back state") });
        self.record_transition(&mut before, wgpu::TextureUses::COPY_SRC, false);
        let mut copy = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("poc read-back") });
        copy.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &self.copy_source, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(self.height) },
            },
            extent(self.width, self.height),
        );
        let mut after = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("poc read-back state back") });
        self.record_transition(&mut after, wgpu::TextureUses::COPY_SRC, true);
        let submission = gpu.queue.submit([before.finish(), copy.finish(), after.finish()]);

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.wait_for(submission)?;
        let pixels = slice.get_mapped_range().map_err(|e| format!("mapping the read-back buffer failed: {e}"))?.to_vec();
        buffer.unmap();
        Ok(pixels)
    }
}

fn frame_of(frame: &DecodedFrame) -> Result<D3d12Frame, String> {
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

pub struct MixCompositor {
    context: Arc<GpuContext>,
    graph: EffectGraph,
    output: OutputTexture,
}

impl MixCompositor {
    pub fn create(gpu: &AppGpu, device: &ID3D12Device, width: u32, height: u32) -> Result<Self, String> {
        // SAFETY: the pointer is a live ID3D12Device, kept alive by `device` and the wgpu device.
        let context = unsafe { GpuContext::from_d3d12(device.as_raw().cast::<c_void>()) }.map_err(|e| format!("GpuContext::from_d3d12 failed: {e}"))?;
        let opener = context.clone();
        let graph = EffectGraph::parse("mix", move |name| {
            let package = Package::open(MIX_PACKAGE)?;
            Ok((name.to_owned(), Effect::new(opener.clone(), package)?))
        })
        .map_err(|e| format!("loading the mix effect failed: {e}"))?;
        let output = OutputTexture::create(gpu, device, width, height)?;
        Ok(Self { context, graph, output })
    }

    pub fn output(&self) -> &OutputTexture {
        &self.output
    }

    pub fn graph_memory(&self) -> MemoryStats {
        self.graph.memory()
    }

    pub fn pool_stats(&self) -> PoolStats {
        self.context.pool_stats()
    }

    /// Blends `first` into `second` and returns once the GPU has finished, so both frames may be
    /// released and the output drawn. `progress` is 0 for `first`, 1 for `second`.
    pub fn mix(&mut self, first: &mut DecodedFrame, second: &mut DecodedFrame, progress: f64, info: &FrameInfo) -> Result<(), String> {
        self.graph.set_param("progress", &[progress]).map_err(|e| format!("setting progress failed: {e}"))?;
        let mut inputs = [frame_of(first)?, frame_of(second)?];
        let mut outputs = [D3d12Frame {
            format: PixelFormat::Rgba,
            width: self.output.width,
            height: self.output.height,
            resource: self.output.resource.as_raw().cast(),
            subresource: 0,
            fence: self.output.fence.as_raw().cast(),
            fence_value: self.output.fence_value,
            color: ColorInfo::default(),
        }];
        // SAFETY: the resources and fences are live objects of the shared device, in COMMON once their
        // fences reach the values passed (the app's draws of the output have been waited for), and
        // are not reused until the wait below.
        unsafe { self.graph.process_d3d12(&mut inputs, &mut outputs, Some(info)) }.map_err(|e| format!("process_d3d12 failed: {e}"))?;

        first.raise_fence_value(inputs[0].fence_value);
        second.raise_fence_value(inputs[1].fence_value);
        self.output.fence_value = outputs[0].fence_value;
        self.output.wait_for_mix()
    }
}
