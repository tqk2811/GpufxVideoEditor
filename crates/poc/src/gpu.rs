//! The application's wgpu device on Direct3D 12, and access to its `ID3D12Device`.

use wgpu::hal::api::Dx12;
use windows::core::w;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::System::LibraryLoader::{GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_PIN};

pub struct AppGpu {
    pub adapter_name: String,
    pub device: wgpu::Device,
    #[allow(dead_code)] // used by the display path in part 2
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
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no D3D12 adapter: {e}"))?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| format!("wgpu device creation failed: {e}"))?;
        pin_d3d12_module()?;
        Ok(Self { adapter_name, device, queue })
    }

    /// The `ID3D12Device` behind the wgpu device (a new COM reference).
    pub fn raw_d3d12_device(&self) -> Result<ID3D12Device, String> {
        // SAFETY: the device is only cloned here; wgpu's use of it is not disturbed.
        let hal_device = unsafe { self.device.as_hal::<Dx12>() }.ok_or("the wgpu device is not a D3D12 device")?;
        Ok(hal_device.raw_device().clone())
    }
}
