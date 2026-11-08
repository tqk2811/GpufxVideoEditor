//! The interactive window: choose two clips, seek, and set the transition progress.

use std::path::PathBuf;
use std::sync::Arc;

use gpufx_core::FrameInfo;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::compositor::MixCompositor;
use crate::decoder::D3d12Decoder;
use crate::gpu::AppGpu;
use crate::presenter::{fit_image, Presenter};

struct Session {
    gpu: AppGpu,
    compositor: MixCompositor,
    decoders: Vec<D3d12Decoder>,
    presenter: Presenter,
    first_clip: usize,
    second_clip: usize,
    seconds: f64,
    progress: f64,
    dirty: bool,
}

impl Session {
    fn open(event_loop: &ActiveEventLoop, clips: &[PathBuf]) -> Result<Self, String> {
        let gpu = AppGpu::create()?;
        let device = gpu.raw_d3d12_device()?;
        let decoders = clips.iter().map(|clip| D3d12Decoder::open(clip, &device)).collect::<Result<Vec<_>, _>>()?;
        let compositor = MixCompositor::create(&gpu, &device, 1920, 1080)?;
        let attributes = Window::default_attributes().with_title("poc player").with_inner_size(winit::dpi::LogicalSize::new(1280, 820));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("creating the window failed: {e}"))?);
        let presenter = Presenter::new(&gpu, window, compositor.output(), true)?;
        Ok(Self { gpu, compositor, decoders, presenter, first_clip: 0, second_clip: 1, seconds: 0.0, progress: 0.5, dirty: true })
    }

    fn duration(&self) -> f64 {
        self.decoders.iter().map(|d| d.duration_seconds()).fold(f64::MAX, f64::min) - 0.1
    }

    fn update_output(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let mut first = self.decoders[self.first_clip].seek_accurate(self.seconds)?;
        let mut second = self.decoders[self.second_clip].seek_accurate(self.seconds)?;
        self.decoders[self.first_clip].wait_until_idle()?;
        self.decoders[self.second_clip].wait_until_idle()?;
        let info = FrameInfo::new(self.seconds, 0);
        self.compositor.mix(&mut first.frame, &mut second.frame, self.progress, &info)
    }

    fn redraw(&mut self) -> Result<(), String> {
        self.update_output()?;
        let duration = self.duration();
        let clip_count = self.decoders.len();
        let image_size = egui::vec2(self.compositor.output().width() as f32, self.compositor.output().height() as f32);
        let (mut first_clip, mut second_clip, mut seconds, mut progress) = (self.first_clip, self.second_clip, self.seconds, self.progress);
        let mut changed = false;
        let submission = self.presenter.render(&self.gpu, self.compositor.output(), |root, texture_id| {
            egui::CentralPanel::default().show(root, |ui| {
                ui.horizontal(|ui| {
                    changed |= clip_combo(ui, "Clip A", &mut first_clip, clip_count);
                    changed |= clip_combo(ui, "Clip B", &mut second_clip, clip_count);
                });
                changed |= ui.add(egui::Slider::new(&mut seconds, 0.0..=duration).text("seek (s)")).changed();
                changed |= ui.add(egui::Slider::new(&mut progress, 0.0..=1.0).text("progress")).changed();
                ui.separator();
                let available = ui.available_size();
                let size = fit_image(image_size, available);
                ui.image(egui::load::SizedTexture::new(texture_id, size));
            });
        })?;
        // The next mix writes the output; this draw must have finished reading it.
        if let Some(submission) = submission {
            self.gpu.wait_for(submission)?;
        }
        self.first_clip = first_clip;
        self.second_clip = second_clip;
        self.seconds = seconds;
        self.progress = progress;
        self.dirty |= changed;
        self.presenter.window().request_redraw();
        Ok(())
    }
}

fn clip_combo(ui: &mut egui::Ui, label: &str, selected: &mut usize, count: usize) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label(label).selected_text(format!("{}", *selected + 1)).show_ui(ui, |ui| {
        for index in 0..count {
            changed |= ui.selectable_value(selected, index, format!("{}", index + 1)).changed();
        }
    });
    changed
}

struct PlayerApp {
    clips: Vec<PathBuf>,
    session: Option<Session>,
    error: Option<String>,
}

impl PlayerApp {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.error = Some(message);
        event_loop.exit();
    }
}

impl ApplicationHandler for PlayerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.session.is_some() {
            return;
        }
        match Session::open(event_loop, &self.clips) {
            Ok(session) => {
                session.presenter.window().request_redraw();
                self.session = Some(session);
            }
            Err(message) => self.fail(event_loop, message),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window: WindowId, event: WindowEvent) {
        let Some(session) = self.session.as_mut() else { return };
        session.presenter.on_window_event(&event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => session.presenter.resize(&session.gpu, size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(message) = session.redraw() {
                    self.fail(event_loop, message);
                }
            }
            _ => {}
        }
    }
}

pub fn run(clips: Vec<PathBuf>) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("creating the event loop failed: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = PlayerApp { clips, session: None, error: None };
    event_loop.run_app(&mut app).map_err(|e| format!("the event loop failed: {e}"))?;
    app.error.map_or(Ok(()), Err)
}
