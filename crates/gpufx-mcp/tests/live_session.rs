//! A [`LiveSession`] over a real pipe, against what the window does each frame: the gateway, the
//! editor state, the preview thread with the real producer and the real export worker. Only the
//! window itself is absent.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use d3d12_backend::{EncoderChoice, ExportJob, Gpu, ProjectPaths};
use engine::{frame_count, Project, Time, TrackKind};
use gpufx_editor::agent::gateway::AgentGateway;
use gpufx_editor::agent::{execute, AgentError, AgentHost};
use gpufx_editor::editor::{EditorState, Status};
use gpufx_editor::export::ExportWorker;
use gpufx_editor::preview::backend::{D3d12Producer, PreviewImage, PreviewRequest};
use gpufx_editor::preview::worker::{Outcome, PreviewWorker};
use gpufx_mcp::session::{EditorSession, FileSession, LiveSession};
use serde_json::{json, Value};

/// What the window holds, without the window.
struct Host {
    state: EditorState,
    paths: Arc<ProjectPaths>,
    roots: Vec<PathBuf>,
    gpu: Option<Arc<Gpu>>,
    preview: Option<PreviewWorker<D3d12Producer>>,
    export: Option<ExportWorker>,
}

impl AgentHost for Host {
    fn mode(&self) -> &'static str {
        "live"
    }

    fn state(&mut self) -> &mut EditorState {
        &mut self.state
    }

    fn capture(&mut self, project: Arc<Project>, frame: i64) -> Receiver<Result<Vec<u8>, AgentError>> {
        let (sender, receiver) = channel();
        let paths = self.paths.clone();
        self.preview.as_ref().expect("a test with a GPU").run_on_thread(Box::new(move |producer: &mut D3d12Producer| {
            let _ = sender.send(producer.capture(&project, &paths, frame).map_err(|message| AgentError::new("capture_failed", message)));
        }));
        receiver
    }

    fn start_export(&mut self, out: PathBuf, encoder: EncoderChoice) -> Result<(), AgentError> {
        let project = self.state.project().clone();
        let total = frame_count(&project);
        let job = ExportJob { project, project_file: None, paths: self.paths.clone(), out, encoder, hashes: None };
        let released = self.preview.as_ref().expect("a test with a GPU").suspend();
        let tool = Path::new(env!("CARGO_BIN_EXE_gpufx-mcp")).with_file_name("gpufx-render.exe");
        self.state.begin_export(total);
        self.export = Some(ExportWorker::start(job, self.roots.clone(), tool, released, || {}).map_err(|message| AgentError::new("export_failed", message))?);
        Ok(())
    }

    fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
        }
    }
}

type Poke = Box<dyn FnOnce(&mut Host) + Send>;

/// The loop of the window, on a thread of its own because the state stays on one thread.
struct App {
    pipe: String,
    pokes: Sender<Poke>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn pipe_for_test() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(r"\\.\pipe\gpufx-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))
}

impl App {
    fn start(gpu: Option<Arc<Gpu>>) -> Self {
        let pipe = pipe_for_test();
        let (pokes, incoming) = channel::<Poke>();
        let (ready, started) = channel();
        let name = pipe.clone();
        let thread = std::thread::spawn(move || {
            let roots = vec![packages_root()];
            let gateway = AgentGateway::start(&name, roots.clone(), || {}).expect("open the pipe");
            let preview = gpu.as_ref().map(|gpu| {
                let gpu = gpu.clone();
                PreviewWorker::spawn(move || D3d12Producer::new(gpu), || {})
            });
            let paths = Arc::new(ProjectPaths::new(Path::new("unsaved.json"), roots.clone()));
            let mut host = Host { state: EditorState::default(), paths, roots, gpu, preview, export: None };
            ready.send(()).unwrap();
            loop {
                match incoming.try_recv() {
                    Ok(poke) => poke(&mut host),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                if let Some(export) = &mut host.export {
                    host.state.set_export_progress(export.progress());
                    if let Some(outcome) = export.poll() {
                        host.state.finish_agent_export(&outcome);
                        host.state.end_export(Status::Idle);
                        host.export = None;
                        host.preview.as_ref().unwrap().resume();
                    }
                }
                while let Some(inbound) = gateway.next_request() {
                    let (prepared, reply) = inbound.split();
                    reply.respond(execute(&mut host, prepared));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = host.gpu;
        });
        started.recv().unwrap();
        Self { pipe, pokes, thread: Some(thread) }
    }

    fn session(&self) -> LiveSession {
        LiveSession::on_pipe(self.pipe.clone())
    }

    /// Runs `work` on the loop, as a person's action would, and returns its result.
    fn poke<R: Send + 'static>(&self, work: impl FnOnce(&mut Host) -> R + Send + 'static) -> R {
        let (sender, receiver) = channel();
        self.pokes.send(Box::new(move |host| sender.send(work(host)).unwrap())).unwrap();
        receiver.recv_timeout(Duration::from_secs(30)).expect("the loop answers")
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let (closed, _) = channel();
        self.pokes = closed;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const SECOND: i64 = 90_000;

/// The edits of the comparison, with the revision threaded through.
fn build(session: &mut dyn EditorSession) -> Value {
    let (a, b) = clip_paths();
    let mut at = revision(&call_ok(session, "session_status", json!({})));
    let mut step = |session: &mut dyn EditorSession, tool: &str, mut arguments: Value| {
        arguments["expected_revision"] = json!(at);
        at = revision(&call_ok(session, tool, arguments));
    };
    step(session, "track_add", json!({"kind": "video"}));
    step(session, "clip_add", json!({"track": 0, "source": a, "start": 0, "duration": 2 * SECOND}));
    step(session, "clip_add", json!({"track": 0, "source": b, "start": 2 * SECOND, "duration": 2 * SECOND}));
    step(session, "clip_split", json!({"clip": {"track": 0, "clip": 0}, "at": SECOND}));
    step(session, "clip_set_gain", json!({"clip": {"track": 0, "clip": 1}, "gain_db": -3.0}));
    step(session, "transition_add", json!({"track": 0, "after_clip": 1, "package": "mix", "duration": 18000}));
    step(session, "track_set_hidden", json!({"track": 0, "hidden": false}));
    step(session, "undo", json!({}));
    step(session, "redo", json!({}));
    call_ok(session, "project_get", json!({}))["project"].clone()
}

#[test]
fn the_same_requests_give_equal_projects_in_both_modes() {
    let app = App::start(None);
    let live = build(&mut app.session());
    let file = build(&mut FileSession::with_roots(vec![packages_root()]));
    assert_eq!(live, file);
    assert_eq!(app.poke(|host| serde_json::to_value(host.state.project()).unwrap()), live, "the editor holds what the agent was told");
}

#[test]
fn an_agent_edit_is_one_undo_step_of_the_window_and_moves_the_revision_it_watches() {
    let app = App::start(None);
    let mut session = app.session();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    assert_eq!(call_ok(&mut session, "session_status", json!({}))["mode"], "live");
    let (revision_now, audio, tracks_after_undo) = app.poke(|host| {
        let seen = (host.state.revision(), host.state.audio_revision());
        host.state.undo();
        (seen.0, seen.1, host.state.project().tracks.len())
    });
    assert_eq!(revision_now, 1);
    assert_eq!(audio, 1, "playback follows");
    assert_eq!(tracks_after_undo, 0, "the person's Undo takes it back");
}

#[test]
fn an_edit_by_the_person_makes_the_agents_revision_stale() {
    let app = App::start(None);
    let mut session = app.session();
    let current = revision(&call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"})));
    app.poke(|host| host.state.add_track(TrackKind::Audio));
    let error = call_err(&mut session, "track_add", json!({"expected_revision": current, "kind": "video"}));
    assert_eq!(error.code, "stale_revision");
    assert_eq!(app.poke(|host| host.state.project().tracks.len()), 2, "nothing was added");
}

#[test]
fn a_held_control_makes_an_edit_wait_and_a_lock_is_refused_like_in_the_window() {
    let app = App::start(None);
    let mut session = app.session();
    let (a, _) = clip_paths();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let current = revision(&call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": a, "start": 0, "duration": SECOND})));
    let held = app.poke(|host| {
        host.state.select(Some(engine::ClipKey { track: 0, clip: 0 }));
        host.state.live_gain(-1.0);
        host.state.revision()
    });
    assert_eq!(call_err(&mut session, "track_add", json!({"expected_revision": held, "kind": "video"})).code, "busy");
    let after = app.poke(|host| {
        host.state.commit_live();
        host.state.toggle_track_locked(0);
        host.state.revision()
    });
    assert!(after > current);
    assert_eq!(call_err(&mut session, "clip_delete", json!({"expected_revision": after, "clip": {"track": 0, "clip": 0}})).code, "track_locked");
}

#[test]
fn no_window_is_an_error_of_the_tool_and_a_new_window_is_found_again() {
    let mut session = LiveSession::on_pipe(pipe_for_test());
    assert_eq!(call_err(&mut session, "session_status", json!({})).code, "no_app");
    let app = App::start(None);
    let mut session = app.session();
    call_ok(&mut session, "session_status", json!({}));
    let name = app.pipe.clone();
    drop(app);
    assert_eq!(call_err(&mut session, "session_status", json!({})).code, "no_app");
    assert!(!Path::new(&name).exists());
}

fn gpu() -> Option<Arc<Gpu>> {
    match Gpu::create_hardware() {
        Ok(Some(gpu)) => Some(Arc::new(gpu)),
        _ => {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "no GPU");
            eprintln!("skipping: no D3D12 hardware adapter is available");
            None
        }
    }
}

fn poll_frames(worker: &PreviewWorker<D3d12Producer>) -> Vec<(i64, bool)> {
    let mut seen = Vec::new();
    while let Some(Outcome::<PreviewImage> { id, result }) = worker.poll() {
        seen.push((id.frame, result.is_ok()));
    }
    seen
}

#[test]
fn capture_matches_the_file_mode_and_leaves_playhead_and_preview_alone() {
    let _gpu = gpu_lock();
    let Some(device) = gpu() else { return };
    let app = App::start(Some(device));
    let mut live = app.session();
    let (a, _) = clip_paths();
    let mut file = FileSession::with_roots(vec![packages_root()]);
    for session in [&mut live as &mut dyn EditorSession, &mut file] {
        call_ok(session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
        call_ok(session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": a, "start": 0, "duration": 150 * FRAME}));
    }
    let revision_before = app.poke(|host| {
        host.state.set_playhead(Time::from_ticks(10 * FRAME));
        host.state.revision()
    });
    let (live_capture, file_capture) = (call_ok(&mut live, "frame_capture", json!({"frame": 70, "max_width": 200})), call_ok(&mut file, "frame_capture", json!({"frame": 70, "max_width": 200})));
    assert_eq!(live_capture["frame"], json!(70));
    assert_eq!(revision(&live_capture), revision_before);
    assert_eq!(live_capture["png"], file_capture["png"], "the same picture in both modes");

    let (playhead, frames) = app.poke(|host| {
        let worker = host.preview.as_ref().unwrap();
        // Nothing the capture did may reach what the screen is given.
        let frames = poll_frames(worker);
        (host.state.playhead(), frames)
    });
    assert_eq!(playhead, Time::from_ticks(10 * FRAME));
    assert!(frames.is_empty(), "the capture published nothing: {frames:?}");

    // And the preview still renders after it, with its own request.
    app.poke(|host| {
        let project = Arc::new(host.state.project().clone());
        let request = PreviewRequest { revision: host.state.revision(), project, paths: host.paths.clone(), frame: 10 };
        host.preview.as_ref().unwrap().request(request);
    });
    let started = Instant::now();
    loop {
        let frames = app.poke(|host| poll_frames(host.preview.as_ref().unwrap()));
        if !frames.is_empty() {
            assert_eq!(frames, [(10, true)]);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the preview gave no frame");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(call_err(&mut live, "frame_capture", json!({"frame": 150})).code, "out_of_range");
}

#[test]
fn a_live_export_blocks_edits_then_reports_and_brings_the_preview_back() {
    let _gpu = gpu_lock();
    let Some(device) = gpu() else { return };
    let app = App::start(Some(device));
    let mut live = app.session();
    let (a, _) = clip_paths();
    call_ok(&mut live, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    call_ok(&mut live, "clip_add", json!({"expected_revision": 1, "track": 0, "source": a, "start": 0, "duration": 60 * FRAME}));
    let dir = scratch("live-export");
    let out = dir.join("out.mp4");
    let started = call_ok(&mut live, "export_start", json!({"out": out}));
    let current = revision(&started);
    assert_eq!(call_err(&mut live, "frame_capture", json!({"frame": 1})).code, "busy");
    assert_eq!(call_err(&mut live, "track_add", json!({"expected_revision": current, "kind": "audio"})).code, "busy");
    let began = Instant::now();
    let finished = loop {
        let status = call_ok(&mut live, "export_status", json!({"job": started["job"]}));
        if status["state"] != "running" {
            break status;
        }
        assert!(began.elapsed() < Duration::from_secs(120), "{status}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(finished["state"], "finished", "{finished}");
    assert!(out.is_file());
    assert!(!app.poke(|host| matches!(host.export, Some(_))));
    call_ok(&mut live, "frame_capture", json!({"frame": 1}));
}
