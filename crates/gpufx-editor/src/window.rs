//! The window and its event loop.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::editor::EditorState;
use crate::presenter::{Gpu, Presenter};
use crate::ui::{self, UiRequest};

const MEDIA_EXTENSIONS: [&str; 10] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a", "flac"];

struct Session {
    gpu: Gpu,
    presenter: Presenter,
    state: EditorState,
    next_repaint: Option<Instant>,
}

impl Session {
    fn open(event_loop: &ActiveEventLoop) -> Result<Self, String> {
        let gpu = Gpu::create()?;
        let attributes = Window::default_attributes().with_title("gpufx editor").with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("creating the window failed: {e}"))?);
        let presenter = Presenter::new(&gpu, window)?;
        Ok(Self { gpu, presenter, state: EditorState::default(), next_repaint: None })
    }

    fn redraw(&mut self) -> Result<(), String> {
        let mut requests = Vec::new();
        let state = &mut self.state;
        let delay = self.presenter.render(&self.gpu, |root| ui::show(root, state, &mut requests))?;
        self.next_repaint = Instant::now().checked_add(delay);
        for request in requests {
            self.handle(request);
        }
        Ok(())
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
            WindowEvent::Resized(size) => session.presenter.resize(&session.gpu, size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(message) = session.redraw() {
                    self.fail(event_loop, message);
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
