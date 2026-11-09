//! The one wgpu Direct3D 12 device shared by the compositor and gpufx.

use std::sync::{Arc, Mutex};

use gpufx_core::GpuContext;
use wgpu::hal::api::Dx12;
use windows::core::w;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::System::LibraryLoader::{GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_PIN};

/// wgpu unloads d3d12.dll when its last device goes away. COM references held elsewhere (FFmpeg's
/// and the decoder's frames) would then release into unmapped code, so keep the DLL.
fn pin_d3d12_module() -> Result<(), String> {
    let mut module = HMODULE::default();
    // SAFETY: the name is a valid NUL-terminated string and module outlives the call.
    unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, w!("d3d12.dll"), &mut module) }.map_err(|e| format!("pinning d3d12.dll failed: {e}"))
}

/// The device, its queue and the gpufx context on them. There is one [`GpuContext`] per queue.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub context: Arc<GpuContext>,
    pub adapter_name: String,
    raw: ID3D12Device,
    /// The first error wgpu reported outside an error scope.
    error: Arc<Mutex<Option<String>>>,
}

impl Gpu {
    /// Opens the high-performance Direct3D 12 adapter, or the first adapter whose name contains
    /// the `GPUFX_ADAPTER` environment variable (for example `Intel` or `NVIDIA`).
    pub fn create() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::DX12, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let adapter = match std::env::var("GPUFX_ADAPTER") {
            Ok(wanted) if !wanted.is_empty() => pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12))
                .into_iter()
                .find(|a| a.get_info().name.to_lowercase().contains(&wanted.to_lowercase()))
                .ok_or_else(|| format!("no D3D12 adapter name contains {wanted}"))?,
            _ => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }))
                .map_err(|e| format!("no D3D12 adapter: {e}"))?,
        };
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gpufx-render"),
            required_features: GpuContext::wanted_features(&adapter),
            required_limits: GpuContext::wanted_limits(&adapter),
            ..Default::default()
        }))
        .map_err(|e| format!("wgpu device creation failed: {e}"))?;
        pin_d3d12_module()?;

        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sink = error.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            let mut slot = sink.lock().expect("not poisoned");
            slot.get_or_insert_with(|| e.to_string());
        }));

        // SAFETY: the device is only cloned here; wgpu's use of it is not disturbed.
        let raw = {
            let hal = unsafe { device.as_hal::<Dx12>() }.ok_or("the wgpu device is not a D3D12 device")?;
            hal.raw_device().clone()
        };
        let context = GpuContext::from_device(&adapter, device.clone(), queue.clone(), None).map_err(|e| format!("GpuContext::from_device failed: {e}"))?;
        Ok(Self { device, queue, context, adapter_name, raw, error })
    }

    /// The `ID3D12Device` behind the wgpu device.
    pub fn raw_device(&self) -> &ID3D12Device {
        &self.raw
    }

    /// `Err` when wgpu reported an error since the device was made.
    pub fn check_errors(&self) -> Result<(), String> {
        match self.error.lock().expect("not poisoned").as_ref() {
            Some(message) => Err(format!("wgpu error: {message}")),
            None => Ok(()),
        }
    }
}
