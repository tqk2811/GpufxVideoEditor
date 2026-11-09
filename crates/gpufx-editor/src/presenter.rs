//! Draws egui into a window surface.

use std::sync::Arc;
use std::time::Duration;

use winit::window::Window;

/// How long to wait before redrawing when no frame could be presented.
const SURFACE_RETRY: Duration = Duration::from_millis(100);

pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Gpu {
    pub fn create() -> Result<Self, String> {
        let descriptor = wgpu::InstanceDescriptor { backends: wgpu::Backends::DX12, ..wgpu::InstanceDescriptor::new_without_display_handle() };
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no D3D12 adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| format!("wgpu device creation failed: {e}"))?;
        Ok(Self { instance, adapter, device, queue })
    }
}

pub struct Presenter {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    state: egui_winit::State,
    context: egui::Context,
}

impl Presenter {
    pub fn new(gpu: &Gpu, window: Arc<Window>) -> Result<Self, String> {
        let surface = gpu.instance.create_surface(window.clone()).map_err(|e| format!("creating the surface failed: {e}"))?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        // A non-sRGB format, because egui writes gamma-encoded colours.
        let format = capabilities.formats.iter().copied().find(|f| !f.is_srgb()).ok_or("the surface has no non-sRGB format")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: Vec::new(),
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);
        let renderer = egui_wgpu::Renderer::new(&gpu.device, format, egui_wgpu::RendererOptions::default());
        let context = egui::Context::default();
        let state = egui_winit::State::new(context.clone(), egui::ViewportId::ROOT, window.as_ref(), Some(window.scale_factor() as f32), None, None);
        Ok(Self { window, surface, config, renderer, state, context })
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Passes an event to egui; true when egui wants a redraw.
    pub fn on_window_event(&mut self, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(&self.window, event).repaint
    }

    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&gpu.device, &self.config);
        }
    }

    /// Runs the UI and presents. Returns how long egui can wait before the next frame.
    pub fn render(&mut self, gpu: &Gpu, mut ui: impl FnMut(&mut egui::Ui)) -> Result<Duration, String> {
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(SURFACE_RETRY),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                return Ok(SURFACE_RETRY);
            }
            wgpu::CurrentSurfaceTexture::Validation => return Err("acquiring the surface texture failed validation".to_owned()),
        };

        let raw_input = self.state.take_egui_input(&self.window);
        let mut full_output = self.context.run_ui(raw_input, |root| ui(root));
        let repaint_delay = full_output.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |viewport| viewport.repaint_delay);
        self.state.handle_platform_output(&self.window, full_output.platform_output);
        let paint_jobs = self.context.tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: full_output.pixels_per_point,
        };
        for (id, deltas) in full_output.textures_delta.set.drain() {
            for delta in deltas {
                self.renderer.update_texture(&gpu.device, &gpu.queue, id, &delta);
            }
        }

        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("editor egui") });
        let upload_buffers = self.renderer.update_buffers(&gpu.device, &gpu.queue, &mut encoder, &paint_jobs, &screen);
        let target = surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("editor egui pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &paint_jobs, &screen);
        }

        let mut buffers: Vec<wgpu::CommandBuffer> = upload_buffers;
        buffers.push(encoder.finish());
        gpu.queue.submit(buffers);
        gpu.queue.present(surface_texture);

        for id in full_output.textures_delta.free.drain() {
            self.renderer.free_texture(&id);
        }
        Ok(repaint_delay)
    }
}
