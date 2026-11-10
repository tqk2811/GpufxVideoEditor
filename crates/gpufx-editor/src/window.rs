//! The window and its event loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Fullscreen, Window, WindowId};

use d3d12_backend::{encoder_exists, probe_media, read_package_info, EncoderChoice, ExportJob, Gpu, ProjectPaths};
use engine::Project;
#[cfg(debug_assertions)]
use engine::{Time, TrackKind};

use crate::agent::gateway::{pipe_name, AgentGateway};
use crate::agent::{check_export, execute, AgentError, AgentHost};
use crate::editor::{EditorState, Status};
use crate::export::{encoder_options, export_blocker, finished_message, sibling_render_exe, ExportOutcome, ExportWorker};
use crate::i18n::{Language, Message};
use crate::packages::{scan, Catalog};
use crate::playback::device::Playback;
use crate::playback::plan::{frames_to_ms, TransportCommand};
use crate::presenter::Presenter;
use crate::preview::backend::{D3d12Producer, PreviewImage, PreviewRequest};
use crate::preview::plan::{frame_index, is_current, PlaybackFrames, PlaybackRun, RequestId, RequestTracker};
use crate::preview::worker::{Outcome, PreviewWorker};
use crate::project_file::{load_project_file, package_roots, project_paths, save_project_file};
use crate::project_lock::held_elsewhere;
use crate::tasks::{run_guarded, Action, Arrival, FileDialog, Prompt, Request, TaskId, Tasks};
use crate::ui::{self, UiRequest};

/// How often the playhead moves while playing.
const PLAYING_REPAINT: Duration = Duration::from_millis(16);
/// How often the export bar moves.
const EXPORT_REPAINT: Duration = Duration::from_millis(100);

const MEDIA_EXTENSIONS: [&str; 10] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a", "flac"];
const AUDIO_EXTENSIONS: [&str; 6] = ["mp3", "wav", "m4a", "flac", "aac", "ogg"];

/// Hidden check of the frame path: names a media file to open at one second, and the app reports
/// the first frame it shows on stdout and exits a few seconds later. Debug builds only.
#[cfg(debug_assertions)]
const SMOKE_VARIABLE: &str = "GPUFX_EDITOR_SMOKE";
#[cfg(debug_assertions)]
const SMOKE_LINGER: Duration = Duration::from_secs(4);
/// Stays open this many seconds instead, for looking at the window. Debug builds only.
#[cfg(debug_assertions)]
const SMOKE_LINGER_VARIABLE: &str = "GPUFX_EDITOR_SMOKE_LINGER_SECONDS";
/// With the smoke check, also play and seek once, and report the audio figures.
#[cfg(debug_assertions)]
const SMOKE_PLAY_VARIABLE: &str = "GPUFX_EDITOR_SMOKE_PLAY";
/// With the smoke check, starts in this language without touching the settings file.
#[cfg(debug_assertions)]
const SMOKE_LANGUAGE_VARIABLE: &str = "GPUFX_EDITOR_SMOKE_LANGUAGE";
#[cfg(debug_assertions)]
const SMOKE_SEEK_AFTER: Duration = Duration::from_millis(1500);
#[cfg(debug_assertions)]
const SMOKE_UI_FRAMES_IGNORED: u64 = 3;

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
    /// UI pass times while pictures were being made, and after.
    ui_frames: u64,
    ui_generating: UiTimes,
    ui_idle: UiTimes,
}

/// How long the UI passes of the smoke check took.
#[cfg(debug_assertions)]
#[derive(Default)]
struct UiTimes {
    frames: u64,
    total: Duration,
    longest: Duration,
}

#[cfg(debug_assertions)]
impl UiTimes {
    fn add(&mut self, time: Duration) {
        self.frames += 1;
        self.total += time;
        self.longest = self.longest.max(time);
    }

    fn report(&self, label: &str) {
        if self.frames > 0 {
            println!("smoke ui {label}: {} frames, mean {:?}, longest {:?}", self.frames, self.total / self.frames as u32, self.longest);
        }
    }
}

/// Sent to the event loop when the agent pipe has something, so a request is served at once and
/// does not wait for the next frame.
#[derive(Debug, Clone, Copy)]
struct AgentWake;

/// Field order matters: the export, then the worker, stop before the presenter and device are dropped.
struct Session {
    /// The pipe agents reach this window through, while the user allows it.
    gateway: Option<AgentGateway>,
    proxy: EventLoopProxy<AgentWake>,
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
    playback_frames: PlaybackFrames,
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
    tasks: Tasks,
    arrivals: Receiver<Arrival>,
    arrival_sender: Sender<Arrival>,
    /// Set when a finished prompt decided the app should close.
    exit_accepted: bool,
    #[cfg(debug_assertions)]
    smoke: Smoke,
}

impl Session {
    fn open(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<AgentWake>) -> Result<Self, String> {
        let gpu = Arc::new(Gpu::create()?);
        let attributes = Window::default_attributes().with_title("gpufx editor").with_inner_size(winit::dpi::LogicalSize::new(1280, 720)).with_min_inner_size(winit::dpi::LogicalSize::new(800, 500));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("creating the window failed: {e}"))?);
        let presenter = Presenter::new(&gpu, window.clone())?;
        let producer_gpu = gpu.clone();
        let window_for_scan = window.clone();
        let session_window = window.clone();
        let preview = PreviewWorker::spawn(move || D3d12Producer::new(producer_gpu), move || window.request_redraw());
        let mut state = EditorState::default();
        state.set_language(crate::settings::startup_language());
        state.ui.panel_sizes = crate::settings::startup_panel_sizes();
        state.ui.theme = crate::settings::startup_theme();
        #[cfg(debug_assertions)]
        let smoke = {
            let mut smoke = Smoke::default();
            if let Some(path) = std::env::var_os(SMOKE_VARIABLE) {
                smoke.enabled = true;
                smoke.play = std::env::var_os(SMOKE_PLAY_VARIABLE).is_some();
                if let Some(language) = std::env::var(SMOKE_LANGUAGE_VARIABLE).ok().as_deref().and_then(Language::from_code) {
                    state.set_language(language);
                }
                state.add_track(TrackKind::Video);
                state.add_clip_from_file(0, std::path::Path::new(&path));
                state.set_playhead(Time::from_ticks(engine::TICKS_PER_SECOND));
            }
            smoke
        };
        let package_roots = package_roots();
        let paths = Arc::new(project_paths(None, &package_roots));
        state.visuals.get_mut().set_paths(paths.clone());
        let visuals_window = session_window.clone();
        state.visuals.get_mut().set_waker(Arc::new(move || visuals_window.request_redraw()));
        let previews_window = session_window.clone();
        state.previews.get_mut().start_for_editor(package_roots.clone(), Arc::new(move || previews_window.request_redraw()));
        let catalog = spawn_catalog_scan(package_roots.clone(), window_for_scan);
        state.encoders = encoder_options(encoder_exists, cfg!(feature = "ffmpeg-gpl"));
        let (arrival_sender, arrivals) = channel();
        let mut session = Self {
            gateway: None,
            proxy,
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
            playback_frames: PlaybackFrames::default(),
            shown: None,
            snapshot: None,
            surface_error: None,
            audio_revision: 0,
            package_roots,
            paths,
            paths_file: None,
            title: String::new(),
            catalog: Some(catalog),
            tasks: Tasks::default(),
            arrivals,
            arrival_sender,
            exit_accepted: false,
            #[cfg(debug_assertions)]
            smoke,
        };
        if crate::settings::startup_agent_access() {
            session.set_agent_access(true);
        }
        Ok(session)
    }

    /// Opens or closes the pipe. A pipe that cannot open leaves the access off and says why.
    fn set_agent_access(&mut self, enabled: bool) {
        self.gateway = None;
        self.state.agent_connected = false;
        self.state.agent_access = false;
        if !enabled {
            return;
        }
        let proxy = self.proxy.clone();
        let wake = move || {
            let _ = proxy.send_event(AgentWake);
        };
        match pipe_name().and_then(|name| AgentGateway::start(&name, self.package_roots.clone(), wake)) {
            Ok(gateway) => {
                self.gateway = Some(gateway);
                self.state.agent_access = true;
            }
            Err(message) => self.state.report_error(Message::AgentAccessFailed(message)),
        }
    }

    /// Runs what the agent asked, on this thread like any other edit, and applies what follows from it.
    fn serve_agent(&mut self) {
        let Some(gateway) = self.gateway.take() else { return };
        while let Some(inbound) = gateway.next_request() {
            let (prepared, reply) = inbound.split();
            reply.respond(execute(self, prepared));
            // Before the next request: a capture right after an open needs the paths of the new file.
            self.sync_project_paths();
            self.sync_audio_project();
            self.apply_transport();
        }
        self.state.agent_connected = gateway.connected();
        self.gateway = Some(gateway);
        self.window.request_redraw();
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
        // An error of this thread, the presenter's: the worker has its own, and the export is a process.
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
        let linger = std::env::var(SMOKE_LINGER_VARIABLE).ok().and_then(|text| text.parse().ok()).map_or(SMOKE_LINGER, Duration::from_secs);
        self.smoke.exit_at = Instant::now().checked_add(linger);
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
        let revision = self.state.audio_revision();
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
                                self.audio_revision = self.state.audio_revision();
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

    /// Starts an export of the current project to `out` in a child process. Playback and the
    /// preview stop first, so the child has the GPU memory and decoders; it starts once the preview
    /// has let go.
    fn start_export(&mut self, out: PathBuf, encoder: Option<EncoderChoice>) {
        if self.state.is_exporting() {
            self.state.report_refused(Message::ExportAlreadyRunning);
            return;
        }
        if let Some(reason) = export_blocker(self.state.project()) {
            self.state.report_error(reason);
            return;
        }
        let Some(choice) = encoder.or_else(|| self.state.encoders.get(self.state.encoder).map(|option| option.choice.clone())) else { return };
        let total = engine::frame_count(self.state.project());
        self.state.begin_export(total);
        self.playback = None;
        let preview_released = self.preview.suspend();
        let job = ExportJob { project: self.state.project().clone(), project_file: self.state.project_file().map(Path::to_path_buf), paths: self.paths.clone(), out: out.clone(), encoder: choice, hashes: None };
        let window = self.window.clone();
        match ExportWorker::start(job, self.package_roots.clone(), sibling_render_exe(), preview_released, move || window.request_redraw()) {
            Ok(worker) => {
                self.export = Some(worker);
                self.export_out = Some(out);
            }
            Err(message) => {
                self.state.end_export(Status::Error(Message::ExportFailed(message)));
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
        self.state.finish_agent_export(&outcome);
        let out = self.export_out.take().unwrap_or_default();
        let status = match outcome {
            ExportOutcome::Finished(report) => Status::Info(finished_message(&report, &out, self.state.project().output.audio.is_some())),
            ExportOutcome::Cancelled => Status::Info(Message::ExportCancelled),
            ExportOutcome::Failed(message) => Status::Error(message),
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
        let due = frame_index(self.state.project(), self.state.playhead());
        let run = self.playback.as_ref().filter(|_| self.state.is_playing()).map(|playback| PlaybackRun { started: playback.thread().shared().run(), revision });
        let frame = self.playback_frames.pick(run, due);
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
        self.collect_arrivals();
        self.poll_export();
        self.collect_preview();
        self.poll_playback();
        #[cfg(debug_assertions)]
        self.drive_smoke();
        if let Some(retried) = self.presenter.retry_resize(&self.gpu) {
            self.set_surface_error(retried.err());
        }
        self.state.ui.fullscreen = self.presenter.window().fullscreen().is_some();
        let mut requests = Vec::new();
        let state = &mut self.state;
        let mut ui_time = Duration::ZERO;
        let delay = self.presenter.render(&self.gpu, |root, preview| {
            let started = Instant::now();
            ui::show(root, state, &mut requests, preview);
            ui_time = started.elapsed();
        })?;
        let next_drop = self.tasks.next_file_drop(&mut self.state);
        self.perform(next_drop);
        #[cfg(debug_assertions)]
        self.record_smoke_ui(ui_time);
        #[cfg(not(debug_assertions))]
        let _ = ui_time;
        let delay = if self.state.is_exporting() {
            delay.min(EXPORT_REPAINT)
        } else if self.state.is_playing() || self.tasks.has_waiting_load() || !self.state.ui.drop_batches.is_empty() || self.presenter.resize_pending() {
            delay.min(PLAYING_REPAINT)
        } else {
            delay
        };
        self.next_repaint = self.smoke_exit_at().filter(|_| !self.state.is_playing()).or_else(|| Instant::now().checked_add(delay));
        // A request can change the status line, which only the next frame shows.
        if !requests.is_empty() {
            self.window.request_redraw();
        }
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

    #[cfg(debug_assertions)]
    fn record_smoke_ui(&mut self, time: Duration) {
        // The first frames measure font and panel setup, not the visuals.
        self.smoke.ui_frames += 1;
        if self.smoke.enabled && self.smoke.ui_frames > SMOKE_UI_FRAMES_IGNORED {
            let times = if self.state.visuals.get_mut().generating() { &mut self.smoke.ui_generating } else { &mut self.smoke.ui_idle };
            times.add(time);
        }
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
        match self.state.project_file().map(Path::to_path_buf) {
            Some(file) => {
                if !self.state.claim_project_lock(&file) {
                    self.state.report_refused(Message::ProjectInUse(file.display().to_string()));
                }
            }
            None => self.state.release_project_lock(),
        }
        self.paths_file = self.state.project_file().map(Path::to_path_buf);
        self.paths = Arc::new(project_paths(self.paths_file.as_deref(), &self.package_roots));
        self.state.visuals.get_mut().set_paths(self.paths.clone());
        // The audio played on since this frame polled it, so catch up first.
        self.poll_playback();
        self.playback = None;
        self.state.reopen_playback();
    }

    fn sync_title(&mut self) {
        let name = self.state.project_file().and_then(Path::file_name).map_or_else(|| self.state.language().t(Message::Untitled), |name| name.to_string_lossy().into_owned());
        let title = format!("{name}{} - gpufx editor", if self.state.is_dirty() { " *" } else { "" });
        if title != self.title {
            self.presenter.window().set_title(&title);
            self.title = title;
        }
    }

    fn save_project(&mut self, file: PathBuf) {
        if !self.state.project_file().is_some_and(|current| d3d12_backend::same_file(current, &file)) && held_elsewhere(&file) {
            self.state.report_refused(Message::ProjectInUse(file.display().to_string()));
            return;
        }
        match save_project_file(self.state.project(), &file) {
            Ok(()) => self.state.mark_saved(file),
            Err(message) => self.state.report_error(Message::SaveFailed(message)),
        }
    }

    /// Runs `work` on its own thread and wakes the loop when its result is in. The threads are
    /// never joined, so a dialog still up or a file still being read cannot hold up the exit. A
    /// panic in `work` is delivered as a failure of `task`, so its slot does not stay taken.
    fn spawn_task(&mut self, name: &'static str, task: TaskId, work: impl FnOnce() -> Arrival + Send + 'static) {
        let sender = self.arrival_sender.clone();
        let window = self.window.clone();
        let spawned = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            let _ = sender.send(run_guarded(name, task, work));
            window.request_redraw();
        });
        if let Err(e) = spawned {
            self.tasks.start_failed(&mut self.state, format!("starting the {name} thread failed: {e}"));
        }
    }

    fn show_file_dialog(&mut self, dialog: FileDialog) {
        let parent = self.window.clone();
        let base = || rfd::FileDialog::new().set_parent(parent.as_ref());
        let language = self.state.language();
        let picker = match dialog {
            FileDialog::PickClip { .. } | FileDialog::ImportMedia { audio_only: false } => base().add_filter(language.t(Message::FilterMedia), &MEDIA_EXTENSIONS).add_filter(language.t(Message::FilterAllFiles), &["*"]),
            FileDialog::ImportMedia { audio_only: true } => base().add_filter(language.t(Message::FilterAudio), &AUDIO_EXTENSIONS).add_filter(language.t(Message::FilterAllFiles), &["*"]),
            FileDialog::OpenProject => base().add_filter(language.t(Message::FilterProject), &["json"]),
            FileDialog::SaveProject => base().add_filter(language.t(Message::FilterProject), &["json"]).set_file_name("project.json"),
            FileDialog::Export => base().add_filter("MP4", &["mp4"]).set_file_name("export.mp4"),
        };
        self.spawn_task("file-dialog", TaskId::Modal, move || {
            let path = match dialog {
                FileDialog::PickClip { .. } | FileDialog::ImportMedia { .. } | FileDialog::OpenProject => picker.pick_file(),
                FileDialog::SaveProject | FileDialog::Export => picker.save_file(),
            };
            Arrival::FilePicked { dialog, path }
        });
    }

    fn show_prompt(&mut self, prompt: Prompt) {
        let dialog = rfd::MessageDialog::new()
            .set_parent(self.window.as_ref())
            .set_title("gpufx editor")
            .set_description(self.state.language().t(prompt.question()))
            .set_buttons(rfd::MessageButtons::YesNo);
        self.spawn_task("prompt", TaskId::Modal, move || Arrival::PromptAnswered { prompt, accepted: dialog.show() == rfd::MessageDialogResult::Yes });
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Nothing => {}
            Action::ShowFileDialog(dialog) => self.show_file_dialog(dialog),
            Action::ShowPrompt(prompt) => self.show_prompt(prompt),
            Action::ProbeClip { load, track, path } => self.spawn_task("clip-probe", TaskId::Load(load), move || {
                let result = probe_media(&path);
                Arrival::ClipProbed { load, track, path, result }
            }),
            Action::ProbeDrop { load, path } => self.spawn_task("drop-probe", TaskId::Load(load), move || {
                let result = probe_media(&path);
                Arrival::DropProbed { load, path, result }
            }),
            Action::ProbeMedia { load, path } => self.spawn_task("media-probe", TaskId::Load(load), move || {
                let result = probe_media(&path);
                Arrival::MediaProbed { load, path, result }
            }),
            Action::ReadProject { load, file } => {
                let roots = self.package_roots.clone();
                let own = self.state.project_file().is_some_and(|current| d3d12_backend::same_file(current, &file));
                let in_use = self.state.language().t(Message::ProjectInUse(file.display().to_string()));
                self.spawn_task("project-read", TaskId::Load(load), move || {
                    let result = if !own && held_elsewhere(&file) { Err(in_use) } else { load_project_file(&file, &roots) };
                    Arrival::ProjectRead { load, file, result }
                });
            }
            Action::SaveTo(path) => self.save_project(path),
            Action::StartExport(path) => self.start_export(path, None),
            Action::ProjectReplaced => self.playback = None,
            Action::Exit => self.exit_accepted = true,
        }
    }

    /// Takes the results of finished dialogs and loads, and applies a load that had to wait.
    fn collect_arrivals(&mut self) {
        while let Ok(arrival) = self.arrivals.try_recv() {
            let action = self.tasks.on_arrival(&mut self.state, arrival);
            self.perform(action);
        }
        let action = self.tasks.poll(&mut self.state);
        self.perform(action);
    }

    fn begin_task(&mut self, request: Request) {
        let action = self.tasks.begin(&mut self.state, request);
        self.perform(action);
    }

    fn close_requested(&mut self) {
        let action = self.tasks.close_requested(&mut self.state);
        self.perform(action);
    }

    fn set_language(&mut self, language: Language) {
        self.state.set_language(language);
        if let Err(message) = crate::settings::save_language(language) {
            self.state.report_error(Message::LanguageSaveFailed(message));
        }
    }

    fn set_theme(&mut self, choice: crate::ui::theme::ThemeChoice) {
        self.state.ui.theme = choice;
        if let Err(message) = crate::settings::save_theme(choice) {
            self.state.report_error(Message::ThemeSaveFailed(message));
        }
    }

    /// Runs outside the frame, so what it starts cannot disturb the UI pass.
    fn handle(&mut self, request: UiRequest) {
        if self.state.is_exporting() && !matches!(request, UiRequest::CancelExport | UiRequest::ToggleFullscreen | UiRequest::SavePanelSizes | UiRequest::SetAgentAccess(_)) {
            return;
        }
        match request {
            UiRequest::Export => {
                if let Some(reason) = export_blocker(self.state.project()) {
                    self.state.report_error(reason);
                    return;
                }
                self.begin_task(Request::Export);
            }
            UiRequest::CancelExport => {
                if let Some(export) = &self.export {
                    export.cancel();
                }
            }
            UiRequest::NewProject => self.begin_task(Request::NewProject),
            UiRequest::OpenProject => self.begin_task(Request::OpenProject),
            UiRequest::SaveProject => self.begin_task(Request::SaveProject),
            UiRequest::SaveProjectAs => self.begin_task(Request::SaveProjectAs),
            UiRequest::PickClipFile { track } => self.begin_task(Request::PickClip { track }),
            UiRequest::ImportMedia { audio_only } => self.begin_task(Request::ImportMedia { audio_only }),
            UiRequest::SetLanguage(language) => self.set_language(language),
            UiRequest::SetTheme(choice) => self.set_theme(choice),
            UiRequest::SetAgentAccess(enabled) => {
                self.set_agent_access(enabled);
                if let Err(message) = crate::settings::save_agent_access(self.state.agent_access) {
                    self.state.report_error(Message::AgentAccessFailed(message));
                }
            }
            UiRequest::SavePanelSizes => {
                if let Err(message) = crate::settings::save_panel_sizes(&self.state.ui.panel_sizes) {
                    self.state.report_error(Message::PanelSizesSaveFailed(message));
                }
            }
            // winit restores the placement it saved on entry, maximised or not, on leaving.
            UiRequest::ToggleFullscreen => {
                let leaving = self.presenter.window().fullscreen().is_some();
                // The export bar and its Cancel are not in the fullscreen player.
                if !leaving && self.state.is_exporting() {
                    self.state.report_refused(Message::ExportRunning);
                    return;
                }
                self.presenter.window().set_fullscreen(if leaving { None } else { Some(Fullscreen::Borderless(None)) });
            }
        }
    }
}

impl AgentHost for Session {
    fn mode(&self) -> &'static str {
        "live"
    }

    fn state(&mut self) -> &mut EditorState {
        &mut self.state
    }

    fn capture(&mut self, project: Arc<Project>, frame: i64) -> Receiver<Result<Vec<u8>, AgentError>> {
        let (sender, receiver) = channel();
        let paths = self.paths.clone();
        self.preview.run_on_thread(Box::new(move |producer: &mut D3d12Producer| {
            let _ = sender.send(producer.capture(&project, &paths, frame).map_err(|message| AgentError::new("capture_failed", message)));
        }));
        receiver
    }

    fn start_export(&mut self, out: PathBuf, encoder: EncoderChoice) -> Result<(), AgentError> {
        let job = ExportJob { project: self.state.project().clone(), project_file: self.state.project_file().map(Path::to_path_buf), paths: self.paths.clone(), out: out.clone(), encoder: encoder.clone(), hashes: None };
        check_export(&job)?;
        Session::start_export(self, out, Some(encoder));
        if self.state.is_exporting() {
            Ok(())
        } else {
            Err(AgentError::new("export_failed", "the export could not start"))
        }
    }

    fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
        }
    }

    fn tasks_busy(&self) -> bool {
        !self.tasks.is_idle()
    }

    fn paths(&self) -> Arc<ProjectPaths> {
        self.paths.clone()
    }

    fn export_out(&self) -> Option<PathBuf> {
        self.export_out.clone()
    }

    fn project_replaced(&mut self) {
        self.playback = None;
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

struct EditorApp {
    proxy: EventLoopProxy<AgentWake>,
    session: Option<Session>,
    error: Option<String>,
}

impl EditorApp {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.error = Some(message);
        event_loop.exit();
    }
}

impl ApplicationHandler<AgentWake> for EditorApp {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: AgentWake) {
        if let Some(session) = self.session.as_mut() {
            session.serve_agent();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.session.is_some() {
            return;
        }
        match Session::open(event_loop, self.proxy.clone()) {
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
                session.close_requested();
                session.presenter.window().request_redraw();
                if session.exit_accepted {
                    event_loop.exit();
                }
            }
            WindowEvent::DroppedFile(path) => {
                // The fullscreen player has no timeline to resolve a drop against.
                if !session.state.ui.fullscreen {
                    let at = cursor_in_window(session.presenter.window());
                    session.state.ui.files_dropped.push((path, at));
                    session.presenter.window().request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                session.presenter.focus_lost(&mut session.state);
                session.presenter.window().request_redraw();
            }
            WindowEvent::Resized(size) => {
                // Nothing is said when nothing was tried (minimised, or the gate busy): an earlier
                // failure stays until a resize really applied.
                if let Some(resized) = session.presenter.resize(&session.gpu, size.width, size.height) {
                    session.set_surface_error(resized.err());
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(message) = session.redraw() {
                    self.fail(event_loop, message);
                } else if session.exit_accepted {
                    event_loop.exit();
                } else if session.smoke_finished() {
                    #[cfg(debug_assertions)]
                    {
                        session.smoke.ui_generating.report("while generating");
                        session.smoke.ui_idle.report("idle");
                        session.report_smoke_audio();
                    }
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

/// Where the pointer is in the window, in physical pixels. During a file drag the window gets no pointer
/// moves, so the last position egui knows is from before the drag.
#[cfg(windows)]
fn cursor_in_window(window: &winit::window::Window) -> Option<egui::Pos2> {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let mut cursor = POINT { x: 0, y: 0 };
    // SAFETY: `cursor` is a valid place for the call to write to.
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return None;
    }
    let inner = window.inner_position().ok()?;
    Some(egui::pos2((cursor.x - inner.x) as f32, (cursor.y - inner.y) as f32))
}

#[cfg(not(windows))]
fn cursor_in_window(_window: &winit::window::Window) -> Option<egui::Pos2> {
    None
}

/// Whether an event asks for another frame. egui-winit reports a repaint for the redraw event
/// itself, and answering that with another redraw request would keep the loop busy forever.
fn should_request_redraw(event: &WindowEvent, repaint: bool) -> bool {
    repaint && !matches!(event, WindowEvent::RedrawRequested)
}

pub fn run() -> Result<(), String> {
    let event_loop = EventLoop::<AgentWake>::with_user_event().build().map_err(|e| format!("creating the event loop failed: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = EditorApp { proxy: event_loop.create_proxy(), session: None, error: None };
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
