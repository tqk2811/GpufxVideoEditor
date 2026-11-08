//! Draws egui, with the mix output as an image, into a window surface.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::window::Window;

use crate::compositor::OutputTexture;
use crate::gpu::AppGpu;

pub struct Presenter {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    state: egui_winit::State,
    context: egui::Context,
    texture_id: egui::TextureId,
    present_modes: Vec<wgpu::PresentMode>,
    /// Time the last [`Presenter::render`] spent in surface acquire and in present, which block
    /// when the swap chain paces the app.
    last_acquire: Duration,
    last_present: Duration,
}

impl Presenter {
    /// `vsync`: false asks for immediate presentation, as the benchmark needs.
    pub fn new(gpu: &AppGpu, window: Arc<Window>, output: &OutputTexture, vsync: bool) -> Result<Self, String> {
        let surface = gpu.instance.create_surface(window.clone()).map_err(|e| format!("creating the surface failed: {e}"))?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        // A non-sRGB format: egui writes gamma-encoded colours, and the mix output is gamma encoded.
        let format = capabilities.formats.iter().copied().find(|f| !f.is_srgb()).ok_or("the surface has no non-sRGB format")?;
        let present_mode = if vsync {
            wgpu::PresentMode::Fifo
        } else {
            [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
                .into_iter()
                .find(|mode| capabilities.present_modes.contains(mode))
                .ok_or("the surface cannot present without vsync")?
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: Vec::new(),
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);

        let mut renderer = egui_wgpu::Renderer::new(&gpu.device, format, egui_wgpu::RendererOptions::default());
        let texture_id = renderer.register_native_texture(&gpu.device, &output.view, wgpu::FilterMode::Linear);
        let context = egui::Context::default();
        let state = egui_winit::State::new(context.clone(), egui::ViewportId::ROOT, window.as_ref(), Some(window.scale_factor() as f32), None, None);
        Ok(Self {
            window,
            surface,
            config,
            renderer,
            state,
            context,
            texture_id,
            present_modes: capabilities.present_modes,
            last_acquire: Duration::ZERO,
            last_present: Duration::ZERO,
        })
    }

    pub fn present_modes(&self) -> &[wgpu::PresentMode] {
        &self.present_modes
    }

    pub fn last_blocking(&self) -> (Duration, Duration) {
        (self.last_acquire, self.last_present)
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.config.present_mode
    }

    /// Passes an event to egui; true when the UI consumed it.
    pub fn on_window_event(&mut self, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(&self.window, event).consumed
    }

    pub fn resize(&mut self, gpu: &AppGpu, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&gpu.device, &self.config);
        }
    }

    /// Runs the UI and presents. Returns `None` if the surface was unavailable and nothing was
    /// presented (minimized, resized), otherwise the submission that reads the output.
    pub fn render(
        &mut self,
        gpu: &AppGpu,
        output: &OutputTexture,
        mut ui: impl FnMut(&mut egui::Ui, egui::TextureId),
    ) -> Result<Option<wgpu::SubmissionIndex>, String> {
        let acquire_started = Instant::now();
        let current_texture = self.surface.get_current_texture();
        self.last_acquire = acquire_started.elapsed();
        let surface_texture = match current_texture {
            wgpu::CurrentSurfaceTexture::Success(texture) | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(None),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                return Ok(None);
            }
            wgpu::CurrentSurfaceTexture::Validation => return Err("acquiring the surface texture failed validation".to_owned()),
        };

        let raw_input = self.state.take_egui_input(&self.window);
        let texture_id = self.texture_id;
        let full_output = self.context.run_ui(raw_input, |root| ui(root, texture_id));
        self.state.handle_platform_output(&self.window, full_output.platform_output);
        let paint_jobs = self.context.tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: full_output.pixels_per_point,
        };
        for (id, deltas) in &full_output.textures_delta.set {
            for delta in deltas {
                self.renderer.update_texture(&gpu.device, &gpu.queue, *id, delta);
            }
        }

        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("poc egui") });
        let upload_buffers = self.renderer.update_buffers(&gpu.device, &gpu.queue, &mut encoder, &paint_jobs, &screen);
        let target = surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("poc egui pass"),
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

        let mut before = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("poc output state") });
        output.record_transition(&mut before, wgpu::TextureUses::RESOURCE, false);
        let mut after = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("poc output state back") });
        output.record_transition(&mut after, wgpu::TextureUses::RESOURCE, true);

        let mut buffers = vec![before.finish()];
        buffers.extend(upload_buffers);
        buffers.push(encoder.finish());
        buffers.push(after.finish());
        let submission = gpu.queue.submit(buffers);
        let present_started = Instant::now();
        gpu.queue.present(surface_texture);
        self.last_present = present_started.elapsed();

        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        Ok(Some(submission))
    }
}

/// The largest size with the aspect ratio of `image` inside `available`.
pub fn fit_image(image: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    let scale = (available.x / image.x).min(available.y / image.y);
    image * scale.max(0.0)
}
