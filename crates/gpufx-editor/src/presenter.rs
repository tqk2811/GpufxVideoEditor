//! Draws egui into a window surface.

use std::sync::Arc;
use std::time::{Duration, Instant};

use d3d12_backend::Gpu;
use winit::window::Window;

use crate::preview::backend::PreviewImage;

/// How long to wait before redrawing when no frame could be presented.
const SURFACE_RETRY: Duration = Duration::from_millis(100);
/// How soon a frame is tried again when the preview holds the GPU's submit gate.
const SUBMIT_BUSY_RETRY: Duration = Duration::from_millis(8);

/// How long a resize that really failed to configure waits before it is tried again.
const RESIZE_BACKOFF: Duration = Duration::from_millis(250);

/// The current preview frame as egui sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewTexture {
    pub id: egui::TextureId,
    pub width: u32,
    pub height: u32,
}

pub struct Presenter {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    state: egui_winit::State,
    context: egui::Context,
    frame: Option<PreviewImage>,
    frame_texture: Option<egui::TextureId>,
    /// A window size that could not be applied yet; retried on the next resize, Outdated status or redraw.
    wanted_size: Option<(u32, u32)>,
    /// Set after a configure that failed: the redraw retry waits until then. A new resize ignores it.
    retry_after: Option<Instant>,
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
        Ok(Self { window, surface, config, renderer, state, context, frame: None, frame_texture: None, wanted_size: None, retry_after: None })
    }

    /// Shows `frame` from the next [`Presenter::render`] on and returns the frame it replaces.
    pub fn set_frame(&mut self, gpu: &Gpu, frame: Option<PreviewImage>) -> Option<PreviewImage> {
        if let Some(new) = &frame {
            match self.frame_texture {
                Some(id) => self.renderer.update_egui_texture_from_wgpu_texture(&gpu.device, new.image.view(), wgpu::FilterMode::Linear, id),
                None => self.frame_texture = Some(self.renderer.register_native_texture(&gpu.device, new.image.view(), wgpu::FilterMode::Linear)),
            }
        }
        std::mem::replace(&mut self.frame, frame)
    }

    /// The frame shown now, for verification.
    pub fn frame(&self) -> Option<&PreviewImage> {
        self.frame.as_ref()
    }

    fn preview_texture(&self) -> Option<PreviewTexture> {
        let frame = self.frame.as_ref()?;
        Some(PreviewTexture { id: self.frame_texture?, width: frame.image.width(), height: frame.image.height() })
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Passes an event to egui; true when egui wants a redraw.
    pub fn on_window_event(&mut self, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(&self.window, event).repaint
    }

    /// The window lost the focus: every button still down counts as released from the next frame.
    pub fn release_pointer(&mut self) {
        let releases = crate::ui::pointer_release_events(&self.context);
        self.state.egui_input_mut().events.extend(releases);
    }

    /// Reconfigures the surface for a new window size. On failure the old size stays configured but
    /// the new one is kept as wanted, so the next resize, Outdated status or redraw tries it again.
    /// `None` when nothing was tried: the window is minimised, or the preview holds the submit gate
    /// and the size waits for a later redraw. Only `Some(Ok)` means the new size is in place.
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) -> Option<Result<(), String>> {
        if width == 0 || height == 0 {
            return None;
        }
        self.wanted_size = Some((width, height));
        self.retry_after = None;
        self.apply_wanted_size(gpu)
    }

    /// Tries the size a failed resize left wanted. `None` when there is none, when the last try
    /// failed a moment ago, or when the preview holds the submit gate.
    pub fn retry_resize(&mut self, gpu: &Gpu) -> Option<Result<(), String>> {
        if is_backing_off(self.retry_after, Instant::now()) {
            return None;
        }
        let result = self.apply_wanted_size(gpu);
        if let Some(result) = &result {
            self.retry_after = backoff_after(result, Instant::now());
        }
        result
    }

    /// Whether a resize still waits, for a busy GPU or after a failure.
    pub fn resize_pending(&self) -> bool {
        self.wanted_size.is_some()
    }

    /// `None` when there is no wanted size or the preview holds the submit gate; the size stays
    /// wanted, and the next redraw tries it.
    fn apply_wanted_size(&mut self, gpu: &Gpu) -> Option<Result<(), String>> {
        let (width, height) = self.wanted_size?;
        let gate = gpu.try_lock_submits()?;
        let surface = &self.surface;
        let result = apply_resize(&mut self.config, width, height, |config| gpu.configure_surface(gate, surface, config));
        if result.is_ok() {
            self.wanted_size = None;
        }
        Some(result)
    }

    /// Runs the UI and presents. Returns how long egui can wait before the next frame.
    ///
    /// The submit gate is held only around the submit and the present. A peek at the start skips the
    /// frame while a long holder runs, before anything that could not be undone (taking the input
    /// from egui); the surface is acquired and the UI run outside the gate, so the preview and the
    /// export are not held up by them.
    pub fn render(&mut self, gpu: &Gpu, mut ui: impl FnMut(&mut egui::Ui, Option<PreviewTexture>)) -> Result<Duration, String> {
        if gpu.try_lock_submits().is_none() {
            return Ok(SUBMIT_BUSY_RETRY);
        }
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(SURFACE_RETRY),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface_loss_error(gpu.device_lost())?;
                // On failure the status repeats and this runs again on the retry. A wanted size
                // that is still pending is the one to configure.
                if self.apply_wanted_size(gpu).is_none() {
                    if let Some(gate) = gpu.try_lock_submits() {
                        let _ = gpu.configure_surface(gate, &self.surface, &self.config);
                    }
                }
                return Ok(SURFACE_RETRY);
            }
            wgpu::CurrentSurfaceTexture::Validation => return Err("acquiring the surface texture failed validation".to_owned()),
        };

        let raw_input = self.state.take_egui_input(&self.window);
        let preview = self.preview_texture();
        let mut full_output = self.context.run_ui(raw_input, |root| ui(root, preview));
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

        let mut buffers: Vec<wgpu::CommandBuffer> = Vec::with_capacity(upload_buffers.len() + 3);
        if let Some(frame) = &self.frame {
            let mut before = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("preview frame state") });
            frame.image.record_begin_sampling(&mut before);
            buffers.push(before.finish());
        }
        buffers.extend(upload_buffers);
        buffers.push(encoder.finish());
        if let Some(frame) = &self.frame {
            let mut after = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("preview frame state back") });
            frame.image.record_end_sampling(&mut after);
            buffers.push(after.finish());
        }
        // From here to the end of the present nothing returns: the acquired texture must be presented.
        let submission = gpu.submit_and_present(gpu.lock_submits(), buffers, surface_texture);
        if let Some(frame) = &mut self.frame {
            frame.last_use = Some(submission);
        }

        for id in full_output.textures_delta.free.drain() {
            self.renderer.free_texture(&id);
        }
        Ok(repaint_delay)
    }
}

/// The error for a surface that cannot be acquired on a lost device. Reconfiguring cannot help then,
/// and retrying forever would freeze the window without saying why.
fn surface_loss_error(device_lost: Option<String>) -> Result<(), String> {
    device_lost.map_or(Ok(()), |message| Err(format!("the GPU device was lost: {message}")))
}

/// When to try again after `result`: later for a configure that failed, at once otherwise.
fn backoff_after(result: &Result<(), String>, now: Instant) -> Option<Instant> {
    result.as_ref().err().map(|_| now + RESIZE_BACKOFF)
}

/// Whether a retry that waits until `until` has to wait still at `now`.
fn is_backing_off(until: Option<Instant>, now: Instant) -> bool {
    until.is_some_and(|until| now < until)
}

/// Runs `configure` with `config` at the new size and records that size only when it succeeded.
fn apply_resize(config: &mut wgpu::SurfaceConfiguration, width: u32, height: u32, configure: impl FnOnce(&wgpu::SurfaceConfiguration) -> Result<(), String>) -> Result<(), String> {
    let wanted = wgpu::SurfaceConfiguration { width, height, ..config.clone() };
    configure(&wanted)?;
    *config = wanted;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(width: u32, height: u32) -> wgpu::SurfaceConfiguration {
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8Unorm,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
            color_space: wgpu::SurfaceColorSpace::Auto,
        }
    }

    #[test]
    fn a_surface_on_a_lost_device_is_an_error_naming_the_reason() {
        let error = surface_loss_error(Some("Destroyed: removed".to_owned())).unwrap_err();
        assert!(error.contains("device was lost") && error.contains("removed"), "{error}");
    }

    #[test]
    fn a_surface_on_a_live_device_is_retried() {
        assert_eq!(surface_loss_error(None), Ok(()));
    }

    #[test]
    fn a_failed_configure_keeps_the_old_size() {
        let mut current = config(100, 50);
        let mut asked = (0, 0);
        let result = apply_resize(&mut current, 200, 80, |c| {
            asked = (c.width, c.height);
            Err("timed out".to_owned())
        });
        assert!(result.is_err());
        assert_eq!(asked, (200, 80), "the new size was tried");
        assert_eq!((current.width, current.height), (100, 50), "the surface still has the old size");
    }

    #[test]
    fn a_failed_configure_backs_off_and_a_success_does_not() {
        let now = Instant::now();
        assert_eq!(backoff_after(&Err("timed out".to_owned()), now), Some(now + RESIZE_BACKOFF));
        assert_eq!(backoff_after(&Ok(()), now), None);
    }

    #[test]
    fn a_backoff_holds_until_its_time_and_no_backoff_never_holds() {
        let now = Instant::now();
        assert!(!is_backing_off(None, now));
        assert!(is_backing_off(Some(now + RESIZE_BACKOFF), now));
        assert!(is_backing_off(Some(now + RESIZE_BACKOFF), now + RESIZE_BACKOFF / 2));
        assert!(!is_backing_off(Some(now + RESIZE_BACKOFF), now + RESIZE_BACKOFF));
    }

    #[test]
    fn a_successful_configure_records_the_new_size() {
        let mut current = config(100, 50);
        assert_eq!(apply_resize(&mut current, 200, 80, |_| Ok(())), Ok(()));
        assert_eq!((current.width, current.height), (200, 80));
    }
}
