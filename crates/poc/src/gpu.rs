//! The application's wgpu device on Direct3D 12, access to its `ID3D12Device`, and the video
//! memory the process uses on that adapter.

use wgpu::hal::api::Dx12;
use windows::core::w;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter3, IDXGIFactory4, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_PIN};

use crate::validation::{self, ValidationLog};

pub struct AppGpu {
    pub adapter_name: String,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// wgpu unloads d3d12.dll when its last device goes away. COM references held elsewhere (FFmpeg's,
/// the decoder's frames) would then release into unmapped code, so keep the DLL for the whole process.
fn pin_d3d12_module() -> Result<(), String> {
    let mut module = HMODULE::default();
    // SAFETY: the name is a valid NUL-terminated string and module outlives the call.
    unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, w!("d3d12.dll"), &mut module) }
        .map_err(|e| format!("pinning d3d12.dll failed: {e}"))
}

impl AppGpu {
    pub fn create() -> Result<Self, String> {
        let mut descriptor = wgpu::InstanceDescriptor { backends: wgpu::Backends::DX12, ..wgpu::InstanceDescriptor::new_without_display_handle() };
        if validation::is_requested() {
            // The debug layer is process-wide, so it also covers gpufx's device and FFmpeg's decode queues.
            descriptor.flags |= wgpu::InstanceFlags::VALIDATION | wgpu::InstanceFlags::GPU_BASED_VALIDATION;
        }
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no D3D12 adapter: {e}"))?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| format!("wgpu device creation failed: {e}"))?;
        pin_d3d12_module()?;
        Ok(Self { adapter_name, instance, adapter, device, queue })
    }

    /// The `ID3D12Device` behind the wgpu device (a new COM reference).
    pub fn raw_d3d12_device(&self) -> Result<ID3D12Device, String> {
        // SAFETY: the device is only cloned here; wgpu's use of it is not disturbed.
        let hal_device = unsafe { self.device.as_hal::<Dx12>() }.ok_or("the wgpu device is not a D3D12 device")?;
        Ok(hal_device.raw_device().clone())
    }

    /// The collector of D3D12 validation messages, when `POC_D3D12_DEBUG=1`.
    pub fn validation_log(&self) -> Result<Option<ValidationLog>, String> {
        if !validation::is_requested() {
            return Ok(None);
        }
        ValidationLog::attach(&self.raw_d3d12_device()?).map(Some)
    }

    /// Blocks until the GPU has finished `submission`.
    pub fn wait_for(&self, submission: wgpu::SubmissionIndex) -> Result<(), String> {
        self.device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .map(|_| ())
            .map_err(|e| format!("waiting for the GPU failed: {e}"))
    }
}

/// `IDXGIAdapter3::QueryVideoMemoryInfo` for the adapter of a device.
pub struct VideoMemoryProbe {
    adapter: IDXGIAdapter3,
}

impl VideoMemoryProbe {
    pub fn for_device(device: &ID3D12Device) -> Result<Self, String> {
        // SAFETY: plain COM calls on valid objects; the LUID comes from a live device.
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1 failed: {e}"))?;
            let adapter = factory.EnumAdapterByLuid(device.GetAdapterLuid()).map_err(|e| format!("EnumAdapterByLuid failed: {e}"))?;
            Ok(Self { adapter })
        }
    }

    /// Bytes of local video memory this process uses now.
    pub fn current_usage(&self) -> Result<u64, String> {
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: info outlives the call and node 0 is the only node of a single-GPU adapter.
        unsafe { self.adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }
            .map_err(|e| format!("QueryVideoMemoryInfo failed: {e}"))?;
        Ok(info.CurrentUsage)
    }
}
