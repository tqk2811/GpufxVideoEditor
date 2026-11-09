//! The one wgpu Direct3D 12 device shared by the compositor and gpufx.

use std::sync::{Arc, Mutex, MutexGuard};

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
    /// The instance and adapter the device came from, for making a surface on the same device.
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    raw: ID3D12Device,
    /// The first error wgpu reported outside an error scope.
    error: Arc<Mutex<Option<String>>>,
    /// Set once when the device is lost; never cleared.
    lost: Arc<Mutex<Option<String>>>,
    submit_gate: Mutex<()>,
}

impl Gpu {
    /// Opens the high-performance Direct3D 12 adapter, or the first adapter whose name contains
    /// the `GPUFX_ADAPTER` environment variable (for example `Intel` or `NVIDIA`).
    pub fn create() -> Result<Self, String> {
        Self::open(false)?.ok_or_else(|| "no D3D12 adapter".to_owned())
    }

    /// Like [`Gpu::create`], but `Ok(None)` when the machine has no hardware adapter: no adapter
    /// at all, or only the WARP software one (it cannot decode with D3D12VA). Tests skip on this
    /// alone. Every other failure is an `Err`, including a `GPUFX_ADAPTER` that matches nothing
    /// and a device that fails to open.
    pub fn create_hardware() -> Result<Option<Self>, String> {
        Self::open(true)
    }

    fn open(hardware_only: bool) -> Result<Option<Self>, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::DX12, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let adapter = match std::env::var("GPUFX_ADAPTER") {
            Ok(wanted) if !wanted.is_empty() => pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12))
                .into_iter()
                .find(|a| a.get_info().name.to_lowercase().contains(&wanted.to_lowercase()))
                .ok_or_else(|| format!("no D3D12 adapter name contains {wanted}"))?,
            _ => match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })) {
                Ok(adapter) => adapter,
                Err(wgpu::RequestAdapterError::NotFound { .. }) if hardware_only => return Ok(None),
                Err(e) => return Err(format!("no D3D12 adapter: {e}")),
            },
        };
        let info = adapter.get_info();
        if hardware_only && (info.device_type == wgpu::DeviceType::Cpu || info.name.contains("Microsoft Basic Render")) {
            return Ok(None);
        }
        let adapter_name = info.name;
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
            let mut slot = sink.lock().unwrap_or_else(|p| p.into_inner());
            slot.get_or_insert_with(|| e.to_string());
        }));
        let lost: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let lost_sink = lost.clone();
        device.set_device_lost_callback(move |reason, message| {
            let mut slot = lost_sink.lock().unwrap_or_else(|p| p.into_inner());
            slot.get_or_insert_with(|| format!("{reason:?}: {message}"));
        });

        // SAFETY: the device is only cloned here; wgpu's use of it is not disturbed.
        let raw = {
            let hal = unsafe { device.as_hal::<Dx12>() }.ok_or("the wgpu device is not a D3D12 device")?;
            hal.raw_device().clone()
        };
        let context = GpuContext::from_device(&adapter, device.clone(), queue.clone(), None).map_err(|e| format!("GpuContext::from_device failed: {e}"))?;
        Ok(Some(Self { device, queue, context, adapter_name, instance, adapter, raw, error, lost, submit_gate: Mutex::new(()) }))
    }

    /// Held across every `queue.submit` and `process_d3d12` on this device. gpufx stages fences on
    /// the queue that the next submit from anyone consumes, so no submit may overlap
    /// `process_d3d12` (the contract of `GpuContext::from_device`). Never held while decoding.
    pub fn lock_submits(&self) -> MutexGuard<'_, ()> {
        self.submit_gate.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The `ID3D12Device` behind the wgpu device.
    pub fn raw_device(&self) -> &ID3D12Device {
        &self.raw
    }

    /// `Err` when the device is lost, or wgpu reported an error since the device was made or since
    /// the last [`Gpu::take_error`].
    pub fn check_errors(&self) -> Result<(), String> {
        if let Some(message) = self.device_lost() {
            return Err(format!("the GPU device was lost: {message}"));
        }
        match self.error.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            Some(message) => Err(format!("wgpu error: {message}")),
            None => Ok(()),
        }
    }

    /// Why the device was lost, once it has been.
    pub fn device_lost(&self) -> Option<String> {
        self.lost.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Takes the reported wgpu error, so one bad frame does not fail the frames after it.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}
