//! The window and its event loop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use d3d12_backend::Gpu;
use engine::{Project, Time, TrackKind};

use crate::editor::EditorState;
use crate::presenter::Presenter;
use crate::preview::backend::{D3d12Producer, PreviewImage, PreviewRequest};
use crate::preview::plan::{frame_index, is_current, RequestId, RequestTracker};
use crate::preview::worker::{Outcome, PreviewWorker};
use crate::ui::{self, UiRequest};

const MEDIA_EXTENSIONS: [&str; 10] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a", "flac"];

/// Hidden check of the frame path: names a media file to open at one second, and the app reports
/// the first frame it shows on stdout and exits a few seconds later.
const SMOKE_VARIABLE: &str = "GPUFX_EDITOR_SMOKE";
const SMOKE_LINGER: Duration = Duration::from_secs(4);

#[derive(Default)]
struct Smoke {
    enabled: bool,
    reported: bool,
    exit_at: Option<Instant>,
}

/// Field order matters: the worker stops before the presenter and device are dropped.
struct Session {
    preview: PreviewWorker<D3d12Producer>,
    gpu: Arc<Gpu>,
    presenter: Presenter,
    state: EditorState,
    next_repaint: Option<Instant>,
    tracker: RequestTracker,
    /// The request behind the frame on screen.
    shown: Option<RequestId>,
    snapshot: Option<(u64, Arc<Project>)>,
    smoke: Smoke,
}

impl Session {
    fn open(event_loop: &ActiveEventLoop) -> Result<Self, String> {
        let gpu = Arc::new(Gpu::create()?);
        let attributes = Window::default_attributes().with_title("gpufx editor").with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("creating the window failed: {e}"))?);
        let presenter = Presenter::new(&gpu, window.clone())?;
        let producer_gpu = gpu.clone();
        let preview = PreviewWorker::spawn(move || D3d12Producer::new(producer_gpu), move || window.request_redraw());
        let mut state = EditorState::default();
        let mut smoke = Smoke::default();
        if let Some(path) = std::env::var_os(SMOKE_VARIABLE) {
            smoke.enabled = true;
            state.add_track(TrackKind::Video);
            state.add_clip_from_file(0, std::path::Path::new(&path));
            state.set_playhead(Time::from_ticks(engine::TICKS_PER_SECOND));
        }
        Ok(Self { preview, gpu, presenter, state, next_repaint: None, tracker: RequestTracker::default(), shown: None, snapshot: None, smoke })
    }

    fn apply_outcome(&mut self, Outcome { id, result }: Outcome<PreviewImage>) {
        if !is_current(self.tracker.wanted(), self.shown, id) {
            // Not shown, and it says nothing about what is wanted now.
            if let Ok(Some(frame)) = result {
                self.preview.give_back(frame);
            }
            return;
        }
        match result {
            Ok(frame) => {
                self.shown = Some(id);
                self.state.set_preview_error(None);
                if let Some(old) = self.presenter.set_frame(&self.gpu, frame) {
                    self.preview.give_back(old);
                }
                self.report_smoke_frame();
            }
            Err(message) => {
                if self.smoke.enabled {
                    println!("smoke error: {message}");
                }
                self.state.set_preview_error(Some(message));
            }
        }
    }

    fn collect_preview(&mut self) {
        if let Some(outcome) = self.preview.poll() {
            self.apply_outcome(outcome);
        }
        if let Some(message) = self.gpu.device_lost() {
            self.state.set_preview_error(Some(format!("the GPU device was lost: {message}")));
        }
    }

    fn report_smoke_frame(&mut self) {
        if !self.smoke.enabled || self.smoke.reported {
            return;
        }
        let Some(frame) = self.presenter.frame() else { return };
        self.smoke.reported = true;
        match frame.image.read_rgba(&self.gpu) {
            Ok(rgba) => {
                let pixels = rgba.len() / 4;
                let mean = |channel: usize| rgba.iter().skip(channel).step_by(4).map(|&b| u64::from(b)).sum::<u64>() / pixels.max(1) as u64;
                println!("smoke frame {}x{} mean rgba {} {} {} {} first pixel {:?}", frame.image.width(), frame.image.height(), mean(0), mean(1), mean(2), mean(3), &rgba[..4]);
            }
            Err(message) => println!("smoke read-back failed: {message}"),
        }
        self.smoke.exit_at = Instant::now().checked_add(SMOKE_LINGER);
    }

    /// Asks for the frame under the playhead when the wish changed since the last request.
    fn request_preview(&mut self) {
        let revision = self.state.revision();
        let frame = frame_index(self.state.project(), self.state.playhead());
        if !self.tracker.should_send(revision, frame) {
            return;
        }
        let stale = self.snapshot.as_ref().is_none_or(|(known, _)| *known != revision);
        if stale {
            self.snapshot = Some((revision, Arc::new(self.state.project().clone())));
        }
        if let Some((_, project)) = &self.snapshot {
            self.preview.request(PreviewRequest { revision, project: project.clone(), frame });
        }
    }

    fn redraw(&mut self) -> Result<(), String> {
        self.collect_preview();
        let mut requests = Vec::new();
        let state = &mut self.state;
        let delay = self.presenter.render(&self.gpu, |root, preview| ui::show(root, state, &mut requests, preview))?;
        self.next_repaint = match self.smoke.exit_at {
            Some(at) => Some(at),
            None => Instant::now().checked_add(delay),
        };
        for request in requests {
            self.handle(request);
        }
        self.request_preview();
        Ok(())
    }

    fn smoke_finished(&self) -> bool {
        self.smoke.exit_at.is_some_and(|at| at <= Instant::now())
    }

    /// Runs outside the frame, because the file dialog blocks this thread.
    fn handle(&mut self, request: UiRequest) {
        match request {
            UiRequest::PickClipFile { track } => {
                let picked = rfd::FileDialog::new()
                    .add_filter("Media", &MEDIA_EXTENSIONS)
                    .add_filter("All files", &["*"])
                    .set_parent(self.presenter.window())
                    .pick_file();
                if let Some(path) = picked {
                    self.state.add_clip_from_file(track, &path);
                }
                self.presenter.window().request_redraw();
            }
        }
    }
}

#[derive(Default)]
struct EditorApp {
    session: Option<Session>,
    error: Option<String>,
}

impl EditorApp {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.error = Some(message);
        event_loop.exit();
    }
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.session.is_some() {
            return;
        }
        match Session::open(event_loop) {
            Ok(session) => {
                session.presenter.window().request_redraw();
                self.session = Some(session);
            }
            Err(message) => self.fail(event_loop, message),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window: WindowId, event: WindowEvent) {
        let Some(session) = self.session.as_mut() else { return };
        let repaint = session.presenter.on_window_event(&event);
        if should_request_redraw(&event, repaint) {
            session.presenter.window().request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Err(message) = session.presenter.resize(&session.gpu, size.width, size.height) {
                    session.state.set_preview_error(Some(message));
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(message) = session.redraw() {
                    self.fail(event_loop, message);
                } else if session.smoke_finished() {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(session) = self.session.as_mut() else { return };
        match session.next_repaint {
            Some(at) if at <= Instant::now() => {
                session.next_repaint = None;
                session.presenter.window().request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// Whether an event asks for another frame. egui-winit reports a repaint for the redraw event
/// itself, and answering that with another redraw request would keep the loop busy forever.
fn should_request_redraw(event: &WindowEvent, repaint: bool) -> bool {
    repaint && !matches!(event, WindowEvent::RedrawRequested)
}

pub fn run() -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("creating the event loop failed: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = EditorApp::default();
    event_loop.run_app(&mut app).map_err(|e| format!("the event loop failed: {e}"))?;
    app.error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redraw_event_never_asks_for_another_redraw() {
        assert!(!should_request_redraw(&WindowEvent::RedrawRequested, true));
    }

    #[test]
    fn other_events_ask_only_when_egui_wants_a_repaint() {
        assert!(should_request_redraw(&WindowEvent::Focused(true), true));
        assert!(!should_request_redraw(&WindowEvent::Focused(true), false));
    }
}
