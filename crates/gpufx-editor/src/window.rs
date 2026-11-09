//! The window and its event loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use d3d12_backend::{encoder_exists, read_package_info, ExportJob, Gpu, ProjectPaths};
use engine::Project;
#[cfg(debug_assertions)]
use engine::{Time, TrackKind};

use crate::editor::{EditorState, Status};
use crate::export::{encoder_options, export_blocker, finished_message, ExportOutcome, ExportWorker};
use crate::packages::{scan, Catalog};
use crate::playback::device::Playback;
use crate::playback::plan::{frames_to_ms, TransportCommand};
use crate::presenter::Presenter;
use crate::preview::backend::{D3d12Producer, PreviewImage, PreviewRequest};
use crate::preview::plan::{frame_index, is_current, RequestId, RequestTracker};
use crate::preview::worker::{Outcome, PreviewWorker};
use crate::project_file::{load_project_file, package_roots, project_paths, save_project_file};
use crate::ui::{self, UiRequest};

/// How often the playhead moves while playing.
const PLAYING_REPAINT: Duration = Duration::from_millis(16);
/// How often the export bar moves.
const EXPORT_REPAINT: Duration = Duration::from_millis(100);

const MEDIA_EXTENSIONS: [&str; 10] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a", "flac"];

/// Hidden check of the frame path: names a media file to open at one second, and the app reports
/// the first frame it shows on stdout and exits a few seconds later. Debug builds only.
#[cfg(debug_assertions)]
const SMOKE_VARIABLE: &str = "GPUFX_EDITOR_SMOKE";
#[cfg(debug_assertions)]
const SMOKE_LINGER: Duration = Duration::from_secs(4);
/// With the smoke check, also play and seek once, and report the audio figures.
#[cfg(debug_assertions)]
const SMOKE_PLAY_VARIABLE: &str = "GPUFX_EDITOR_SMOKE_PLAY";
#[cfg(debug_assertions)]
const SMOKE_SEEK_AFTER: Duration = Duration::from_millis(1500);

#[cfg(debug_assertions)]
#[derive(Default)]
struct Smoke {
    enabled: bool,
    reported: bool,
    exit_at: Option<Instant>,
    play: bool,
    /// When the one seek of the check is due.
    seek_due: Option<Instant>,
    underrun_before_seek: u64,
}

/// Field order matters: the export, then the worker, stop before the presenter and device are dropped.
struct Session {
    export: Option<ExportWorker>,
    /// The file the running export writes to.
    export_out: Option<PathBuf>,
    window: Arc<Window>,
    playback: Option<Playback>,
    preview: PreviewWorker<D3d12Producer>,
    gpu: Arc<Gpu>,
    presenter: Presenter,
    state: EditorState,
    next_repaint: Option<Instant>,
    tracker: RequestTracker,
    /// The request behind the frame on screen.
    shown: Option<RequestId>,
    snapshot: Option<(u64, Arc<Project>)>,
    /// A surface or resize failure that still applies; a following preview frame must not clear it.
    surface_error: Option<String>,
    /// The revision the audio thread has.
    audio_revision: u64,
    package_roots: Vec<PathBuf>,
    /// How files of the open project resolve; shared by preview, playback and export.
    paths: Arc<ProjectPaths>,
    /// The project file the paths were built for.
    paths_file: Option<PathBuf>,
    /// The title now on the window.
    title: String,
    catalog: Option<Receiver<Catalog>>,
    #[cfg(debug_assertions)]
    smoke: Smoke,
}

impl Session {
    fn open(event_loop: &ActiveEventLoop) -> Result<Self, String> {
        let gpu = Arc::new(Gpu::create()?);
        let attributes = Window::default_attributes().with_title("gpufx editor").with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("creating the window failed: {e}"))?);
        let presenter = Presenter::new(&gpu, window.clone())?;
        let producer_gpu = gpu.clone();
        let window_for_scan = window.clone();
        let session_window = window.clone();
        let preview = PreviewWorker::spawn(move || D3d12Producer::new(producer_gpu), move || window.request_redraw());
        let mut state = EditorState::default();
        #[cfg(debug_assertions)]
        let smoke = {
            let mut smoke = Smoke::default();
            if let Some(path) = std::env::var_os(SMOKE_VARIABLE) {
                smoke.enabled = true;
                smoke.play = std::env::var_os(SMOKE_PLAY_VARIABLE).is_some();
                state.add_track(TrackKind::Video);
                state.add_clip_from_file(0, std::path::Path::new(&path));
                state.set_playhead(Time::from_ticks(engine::TICKS_PER_SECOND));
            }
            smoke
        };
        let package_roots = package_roots();
        let paths = Arc::new(project_paths(None, &package_roots));
        let catalog = spawn_catalog_scan(package_roots.clone(), window_for_scan);
        state.encoders = encoder_options(encoder_exists, cfg!(feature = "ffmpeg-gpl"));
        Ok(Self {
            export: None,
            export_out: None,
            window: session_window,
            playback: None,
            preview,
            gpu,
            presenter,
            state,
            next_repaint: None,
            tracker: RequestTracker::default(),
            shown: None,
            snapshot: None,
            surface_error: None,
            audio_revision: 0,
            package_roots,
            paths,
            paths_file: None,
            title: String::new(),
            catalog: Some(catalog),
            #[cfg(debug_assertions)]
            smoke,
        })
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
                self.state.set_preview_error(self.surface_error.clone());
                if let Some(old) = self.presenter.set_frame(&self.gpu, frame) {
                    self.preview.give_back(old);
                }
                #[cfg(debug_assertions)]
                self.report_smoke_frame();
            }
            Err(message) => {
                #[cfg(debug_assertions)]
                if self.smoke.enabled {
                    println!("smoke error: {message}");
                }
                self.state.set_preview_error(Some(message));
            }
        }
    }

    /// Takes the package list once its scan has finished.
    fn collect_catalog(&mut self) {
        if let Some(Ok(catalog)) = self.catalog.as_ref().map(Receiver::try_recv) {
            self.state.catalog = catalog;
            self.catalog = None;
        }
    }

    fn collect_preview(&mut self) {
        if let Some(outcome) = self.preview.poll() {
            self.apply_outcome(outcome);
        }
        if let Some(message) = self.preview.spawn_error() {
            let message = message.to_owned();
            self.state.set_preview_error(Some(message));
        }
        // An error of this thread, the presenter's: the worker and the export have their own.
        if let Some(message) = self.gpu.take_error() {
            self.state.set_preview_error(Some(format!("wgpu error: {message}")));
        }
        if let Some(message) = self.gpu.device_lost() {
            self.state.set_preview_error(Some(format!("the GPU device was lost: {message}")));
        }
    }

    #[cfg(debug_assertions)]
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
        if self.smoke.play {
            self.state.toggle_playback();
            self.smoke.seek_due = Instant::now().checked_add(SMOKE_SEEK_AFTER);
        }
    }

    /// Seeks once, part way through the check, and remembers the gaps so far.
    #[cfg(debug_assertions)]
    fn drive_smoke(&mut self) {
        if self.smoke.seek_due.is_some_and(|at| at <= Instant::now()) {
            self.smoke.seek_due = None;
            self.smoke.underrun_before_seek = self.playback.as_ref().map_or(0, |playback| playback.thread().shared().stats().underrun);
            self.state.scrub_to(Time::from_ticks(engine::TICKS_PER_SECOND));
            self.state.end_scrub();
        }
    }

    #[cfg(debug_assertions)]
    fn report_smoke_audio(&self) {
        if let (true, Some(playback)) = (self.smoke.play, &self.playback) {
            let stats = playback.thread().shared().stats();
            let after_seek = stats.underrun - self.smoke.underrun_before_seek;
            println!("smoke audio played {} underrun {} (before the seek {}, after it {}) delay {} frames, {} ms", stats.played, stats.underrun, self.smoke.underrun_before_seek, after_seek, stats.delay, frames_to_ms(stats.delay));
        }
    }

    /// Follows the clock, and notices the end of the timeline or a failure.
    fn poll_playback(&mut self) {
        let Some(playback) = &self.playback else { return };
        if let Some(message) = playback.thread().take_error() {
            self.state.playback_ended(Some(message));
            self.playback = None;
            return;
        }
        self.state.set_silence_ms(frames_to_ms(playback.thread().shared().stats().gaps));
        if !self.state.is_playing() {
            return;
        }
        let reading = playback.thread().reading();
        self.state.follow_clock(reading.playhead());
        if reading.finished() {
            playback.thread().stop();
            self.state.playback_ended(None);
        }
    }

    fn sync_audio_project(&mut self) {
        let revision = self.state.revision();
        if revision == self.audio_revision {
            return;
        }
        self.audio_revision = revision;
        if let Some(playback) = &self.playback {
            playback.thread().set_project(Arc::new(self.state.project().clone()));
        }
    }

    fn apply_transport(&mut self) {
        for command in self.state.take_transport() {
            match command {
                TransportCommand::Play { from } => {
                    if self.playback.is_none() {
                        match Playback::open(self.state.project(), self.paths.clone()) {
                            Ok(playback) => {
                                self.audio_revision = self.state.revision();
                                self.state.set_no_sound(playback.no_sound().map(str::to_owned));
                                self.playback = Some(playback);
                            }
                            Err(message) => {
                                self.state.playback_ended(Some(message));
                                continue;
                            }
                        }
                    }
                    if let Some(playback) = &self.playback {
                        playback.thread().play(from);
                    }
                }
                TransportCommand::Stop => {
                    if let Some(playback) = &self.playback {
                        playback.thread().stop();
                    }
                }
                TransportCommand::Seek(time) => {
                    if let Some(playback) = &self.playback {
                        playback.thread().seek(time);
                    }
                }
            }
        }
    }

    /// Starts an export of the current project to a chosen file. Playback and the preview stop
    /// first, so the export has the GPU and decoders to itself; the thread waits for the preview to
    /// let go.
    fn start_export(&mut self) {
        if let Some(reason) = export_blocker(self.state.project()) {
            self.state.report_error(reason.to_owned());
            return;
        }
        let Some(out) = rfd::FileDialog::new().add_filter("MP4", &["mp4"]).set_file_name("export.mp4").set_parent(self.presenter.window()).save_file() else { return };
        let Some(option) = self.state.encoders.get(self.state.encoder).cloned() else { return };
        let total = engine::frame_count(self.state.project());
        self.state.begin_export(total);
        self.playback = None;
        let preview_released = self.preview.suspend();
        let job = ExportJob { project: self.state.project().clone(), project_file: self.state.project_file().map(Path::to_path_buf), paths: self.paths.clone(), out: out.clone(), encoder: option.choice, hashes: None };
        let window = self.window.clone();
        match ExportWorker::start(job, self.gpu.clone(), preview_released, move || window.request_redraw()) {
            Ok(worker) => {
                self.export = Some(worker);
                self.export_out = Some(out);
            }
            Err(message) => {
                self.state.end_export(Status::Error(message));
                self.resume_preview();
            }
        }
    }

    /// Follows the running export. When it ends, the preview comes back.
    fn poll_export(&mut self) {
        let Some(export) = &mut self.export else { return };
        self.state.set_export_progress(export.progress());
        let Some(outcome) = export.poll() else { return };
        self.export = None;
        let out = self.export_out.take().unwrap_or_default();
        let status = match outcome {
            ExportOutcome::Finished(report) => Status::Info(finished_message(&report, &out, self.state.project().output.audio.is_some())),
            ExportOutcome::Cancelled => Status::Info("Export cancelled; the partial file was removed".to_owned()),
            ExportOutcome::Failed(message) => Status::Error(format!("Export failed: {message}")),
        };
        self.state.end_export(status);
        self.resume_preview();
    }

    /// Brings the preview back after an export. The last wish was dropped with the suspension, so
    /// it is sent again.
    fn resume_preview(&mut self) {
        self.preview.resume();
        self.tracker = RequestTracker::default();
        self.shown = None;
    }

    /// Asks for the frame under the playhead when the wish changed since the last request.
    fn request_preview(&mut self) {
        if self.state.is_exporting() {
            return;
        }
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
            self.preview.request(PreviewRequest { revision, project: project.clone(), paths: self.paths.clone(), frame });
        }
    }

    /// Records the outcome of a resize. A failure is shown; once it is gone, so is its message.
    fn set_surface_error(&mut self, error: Option<String>) {
        let shown = self.state.preview_error().map(str::to_owned);
        if error.is_none() && shown == self.surface_error {
            self.state.set_preview_error(None);
        }
        if let Some(message) = &error {
            self.state.set_preview_error(Some(message.clone()));
        }
        self.surface_error = error;
    }

    fn redraw(&mut self) -> Result<(), String> {
        self.collect_catalog();
        self.poll_export();
        self.collect_preview();
        self.poll_playback();
        #[cfg(debug_assertions)]
        self.drive_smoke();
        if let Some(retried) = self.presenter.retry_resize(&self.gpu) {
            self.set_surface_error(retried.err());
        }
        let mut requests = Vec::new();
        let state = &mut self.state;
        let delay = self.presenter.render(&self.gpu, |root, preview| ui::show(root, state, &mut requests, preview))?;
        let delay = if self.state.is_exporting() {
            delay.min(EXPORT_REPAINT)
        } else if self.state.is_playing() {
            delay.min(PLAYING_REPAINT)
        } else {
            delay
        };
        self.next_repaint = self.smoke_exit_at().filter(|_| !self.state.is_playing()).or_else(|| Instant::now().checked_add(delay));
        for request in requests {
            self.handle(request);
        }
        self.sync_project_paths();
        self.sync_title();
        self.sync_audio_project();
        self.apply_transport();
        self.request_preview();
        Ok(())
    }

    /// When a running smoke check will end the app.
    #[cfg(debug_assertions)]
    fn smoke_exit_at(&self) -> Option<Instant> {
        self.smoke.exit_at
    }

    #[cfg(not(debug_assertions))]
    fn smoke_exit_at(&self) -> Option<Instant> {
        None
    }

    fn smoke_finished(&self) -> bool {
        self.smoke_exit_at().is_some_and(|at| at <= Instant::now())
    }

    /// Keeps the shared paths with the file the project now belongs to. Audio threads hold the old
    /// paths, so playback is closed here and, if it was running, reopened from the playhead.
    fn sync_project_paths(&mut self) {
        if self.paths_file.as_deref() == self.state.project_file() {
            return;
        }
        self.paths_file = self.state.project_file().map(Path::to_path_buf);
        self.paths = Arc::new(project_paths(self.paths_file.as_deref(), &self.package_roots));
        // The file dialog blocked this thread while the audio played on, so catch up first.
        self.poll_playback();
        self.playback = None;
        self.state.reopen_playback();
    }

    fn sync_title(&mut self) {
        let name = self.state.project_file().and_then(Path::file_name).map_or_else(|| "Untitled".to_owned(), |name| name.to_string_lossy().into_owned());
        let title = format!("{name}{} - gpufx editor", if self.state.is_dirty() { " *" } else { "" });
        if title != self.title {
            self.presenter.window().set_title(&title);
            self.title = title;
        }
    }

    /// A close stops a running export and removes its partial file, so that is asked first.
    fn close_accepted(&self) -> bool {
        if self.state.is_exporting() && !confirm(self.presenter.window(), "An export is running. Cancel it and close?") {
            return false;
        }
        self.discard_accepted("Close")
    }

    /// Whether the user accepts losing `action`'s unsaved changes; asked only when there are some.
    fn discard_accepted(&self, action: &str) -> bool {
        !self.state.is_dirty() || confirm(self.presenter.window(), &format!("The project has unsaved changes. {action} anyway and lose them?"))
    }

    fn project_dialog(&self) -> rfd::FileDialog {
        rfd::FileDialog::new().add_filter("Project", &["json"]).set_parent(self.presenter.window())
    }

    fn open_project(&mut self) {
        if !self.discard_accepted("Open another project") {
            return;
        }
        let Some(file) = self.project_dialog().pick_file() else { return };
        match load_project_file(&file, &self.package_roots) {
            Ok(project) => {
                self.state.replace_project(project, Some(file));
                self.playback = None;
            }
            Err(message) => self.state.report_error(message),
        }
    }

    fn save_project(&mut self, file: Option<PathBuf>) {
        let picked = file.or_else(|| self.project_dialog().set_file_name("project.json").save_file());
        let Some(file) = picked else { return };
        match save_project_file(self.state.project(), &file) {
            Ok(()) => self.state.mark_saved(file),
            Err(message) => self.state.report_error(message),
        }
    }

    /// Runs outside the frame, because the file dialog blocks this thread.
    fn handle(&mut self, request: UiRequest) {
        if self.state.is_exporting() && !matches!(request, UiRequest::CancelExport) {
            return;
        }
        match request {
            UiRequest::Export => self.start_export(),
            UiRequest::CancelExport => {
                if let Some(export) = &self.export {
                    export.cancel();
                }
            }
            UiRequest::NewProject => {
                if self.discard_accepted("Start a new project") {
                    self.state.new_project();
                    self.playback = None;
                }
            }
            UiRequest::OpenProject => self.open_project(),
            UiRequest::SaveProject => self.save_project(self.state.project_file().map(Path::to_path_buf)),
            UiRequest::SaveProjectAs => self.save_project(None),
            UiRequest::PickClipFile { track } => {
                let picked = rfd::FileDialog::new()
                    .add_filter("Media", &MEDIA_EXTENSIONS)
                    .add_filter("All files", &["*"])
                    .set_parent(self.presenter.window())
                    .pick_file();
                if let Some(path) = picked {
                    self.state.add_picked_file(track, &path, self.paths.as_ref());
                }
                self.presenter.window().request_redraw();
            }
        }
    }
}

/// Reads the package manifests away from the UI thread: some packages hold large textures.
fn spawn_catalog_scan(roots: Vec<PathBuf>, window: Arc<Window>) -> Receiver<Catalog> {
    let (sender, receiver) = channel();
    let scanner = std::thread::Builder::new().name("packages".to_owned()).spawn(move || {
        let _ = sender.send(scan(&roots, read_package_info));
        window.request_redraw();
    });
    if let Err(e) = scanner {
        eprintln!("gpufx-editor: the package scan could not start: {e}");
    }
    receiver
}

/// Asks a yes or no question over `window`.
fn confirm(window: &Window, question: &str) -> bool {
    rfd::MessageDialog::new().set_parent(window).set_title("gpufx editor").set_description(question).set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes
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
            WindowEvent::CloseRequested => {
                if session.close_accepted() {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                let resized = session.presenter.resize(&session.gpu, size.width, size.height);
                // A minimised window asks for nothing, so it says nothing about an earlier failure.
                if size.width != 0 && size.height != 0 {
                    session.set_surface_error(resized.err());
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(message) = session.redraw() {
                    self.fail(event_loop, message);
                } else if session.smoke_finished() {
                    #[cfg(debug_assertions)]
                    session.report_smoke_audio();
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
