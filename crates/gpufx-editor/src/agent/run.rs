//! Runs one request against an editor state. Work that reads the disk goes in [`prepare`], so an
//! open window does it away from its UI thread; [`execute`] is quick and runs where the state lives.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use base64::Engine as _;

use d3d12_backend::{probe_media, read_package_info, MediaProbe, ProjectPaths};
use engine::{clip_at, frame_count, frame_time, nearest_free_start, Clip, ClipKey, Effect, Project, Time, TrackKind, Transition};
use serde_json::{json, Map, Value};

use super::image::encode_png;
use super::protocol::{AgentEdit, AgentError, AgentRequest, AgentResult, CaptureTarget, ClipRef};
use crate::editor::{new_clip, EditorState, DEFAULT_CLIP_DURATION, DEFAULT_TRANSITION_DURATION};
use crate::packages::{filter_entries, scan, PackageEntry, PackageKind};
use crate::project_file::{load_project_file_for_agent, save_project_file};
use crate::preview::plan::frame_index;
use crate::project_lock;

/// A request with what had to be read from disk for it.
#[derive(Debug)]
pub struct Prepared {
    pub request: AgentRequest,
    loaded: Option<Result<Project, AgentError>>,
    probe: Option<Result<MediaProbe, AgentError>>,
    packages_problem: Option<AgentError>,
    package_list: Option<Value>,
}

/// What [`execute`] gives: the answer.
#[derive(Debug)]
pub enum Step {
    Done(AgentResult),
    /// A frame being rendered; [`PendingFrame::finish`] may block, so it runs off the UI thread.
    Frame(PendingFrame),
}

/// A capture on its way: the picture arrives on `rgba`.
#[derive(Debug)]
pub struct PendingFrame {
    rgba: Receiver<Result<Vec<u8>, AgentError>>,
    width: u32,
    height: u32,
    frame: i64,
    time: Time,
    revision: u64,
    max_width: u32,
}

impl PendingFrame {
    /// Waits for the picture and encodes it.
    pub fn finish(self) -> AgentResult {
        let rgba = self.rgba.recv().map_err(|_| AgentError::new("capture_failed", "the frame was never rendered"))??;
        let (png, width, height) = encode_png(&rgba, self.width, self.height, self.max_width).map_err(|message| AgentError::new("capture_failed", message))?;
        let png = base64::engine::general_purpose::STANDARD.encode(png);
        Ok(json!({"png": png, "width": width, "height": height, "frame": self.frame, "time": self.time, "revision": self.revision}))
    }
}

/// The side of a session that differs between a window and no window.
pub trait AgentHost {
    /// `file` or `live`.
    fn mode(&self) -> &'static str;
    fn state(&mut self) -> &mut EditorState;
    /// Renders output frame `frame` of `project` as RGBA of its output size, without changing anything shown.
    fn capture(&mut self, project: Arc<Project>, frame: i64) -> Receiver<Result<Vec<u8>, AgentError>>;
    /// Whether a dialog or a load of the window is in the way of replacing the project.
    fn tasks_busy(&self) -> bool {
        false
    }
    /// The project was replaced: what was built from the old one goes.
    fn project_replaced(&mut self) {}
}

/// Reads what `request` needs from disk.
pub fn prepare(request: AgentRequest, package_roots: &[PathBuf]) -> Prepared {
    let mut prepared = Prepared { request, loaded: None, probe: None, packages_problem: None, package_list: None };
    match &prepared.request {
        AgentRequest::ProjectOpen { path, .. } => prepared.loaded = Some(load(path, package_roots)),
        AgentRequest::PackagesList { filter } => prepared.package_list = Some(package_list(package_roots, filter.as_deref())),
        AgentRequest::Edit { edit: AgentEdit::ClipAdd { source, .. }, .. } => prepared.probe = Some(probe(source)),
        AgentRequest::Edit { edit: AgentEdit::EffectAdd { package, .. } | AgentEdit::TransitionAdd { package, .. }, .. } => prepared.packages_problem = package_problem(package, package_roots),
        _ => {}
    }
    prepared
}

fn load(path: &Path, package_roots: &[PathBuf]) -> Result<Project, AgentError> {
    let file = std::path::absolute(path).map_err(|e| AgentError::new("invalid_path", e.to_string()))?;
    load_project_file_for_agent(&file, package_roots).map_err(|message| AgentError::new("invalid_project", message))
}

fn probe(source: &Path) -> Result<MediaProbe, AgentError> {
    if !source.is_file() {
        return Err(AgentError::new("source_not_found", format!("{} does not exist", source.display())));
    }
    probe_media(source).map_err(|message| AgentError::new("invalid_source", message))
}

/// Why `package` may not be used: a package is shader code, so only the package roots supply it.
fn package_problem(package: &str, package_roots: &[PathBuf]) -> Option<AgentError> {
    let paths = ProjectPaths::roots_only(Path::new("agent.json"), package_roots.to_vec());
    paths.package_path(package).is_none().then(|| AgentError::new("package_not_found", format!("no package {package} in the package roots")))
}

fn entry_json(entry: &&PackageEntry) -> Value {
    json!({"package": entry.package, "name": entry.name, "description": entry.description, "params": entry.params.iter().map(|param| &param.name).collect::<Vec<_>>()})
}

fn package_list(package_roots: &[PathBuf], filter: Option<&str>) -> Value {
    let catalog = scan(package_roots, read_package_info);
    let filter = filter.unwrap_or("");
    let list = |kind| filter_entries(catalog.entries(kind), filter).iter().map(entry_json).collect::<Vec<_>>();
    json!({"effects": list(PackageKind::Effect), "transitions": list(PackageKind::Transition)})
}

fn busy(reason: &str) -> AgentError {
    AgentError::new("busy", reason)
}

fn writable<H: AgentHost>(host: &mut H, expected: u64) -> Result<(), AgentError> {
    let state = host.state();
    if let Some(reason) = state.agent_busy_reason() {
        return Err(busy(reason));
    }
    let revision = state.revision();
    if revision == expected {
        Ok(())
    } else {
        Err(AgentError::stale(revision, expected))
    }
}

fn clip_json(key: Option<ClipKey>) -> Value {
    key.map_or(Value::Null, |key| json!(ClipRef::from(key)))
}

/// The start a move lands on, as [`engine::move_clip`] works it out.
fn landing_start(project: &Project, clip: ClipRef, track: usize, start: Time) -> Option<Time> {
    let moved = project.tracks.get(clip.track)?.clips.get(clip.clip)?;
    let target = project.tracks.get(track)?;
    nearest_free_start(target, (track == clip.track).then_some(clip.clip), moved.duration, start)
}

/// The clip of a `clip_add`, checked against what the track plays.
fn added_clip(state: &EditorState, track: usize, source: &Path, start: Time, source_in: Option<Time>, duration: Option<Time>, probe: Option<Result<MediaProbe, AgentError>>) -> Result<Clip, AgentError> {
    let probe = probe.unwrap_or_else(|| Err(AgentError::new("internal", "the source was not probed")))?;
    let Some(target) = state.project().tracks.get(track) else {
        return Err(AgentError::from_edit(&engine::EditError::TrackNotFound { track }));
    };
    let (present, stated, stream, problem) = match target.kind {
        TrackKind::Video => (probe.has_video, probe.video_duration, "video", probe.video_problem),
        TrackKind::Audio => (probe.has_audio, probe.audio_duration, "audio", probe.audio_problem),
    };
    if !present {
        return Err(AgentError::new("invalid_source", format!("{} has no usable {stream} stream{}", source.display(), problem.map_or_else(String::new, |reason| format!(": {reason}")))));
    }
    let absolute = std::path::absolute(source).map_err(|e| AgentError::new("invalid_path", e.to_string()))?;
    let length = duration.or(stated.filter(|length| *length > Time::ZERO)).unwrap_or(DEFAULT_CLIP_DURATION);
    let mut clip = new_clip(&absolute.to_string_lossy(), start, length);
    clip.source_in = source_in.unwrap_or(Time::ZERO);
    Ok(clip)
}

fn run_edit(state: &mut EditorState, edit: AgentEdit, probe: Option<Result<MediaProbe, AgentError>>) -> Result<Value, AgentError> {
    let refused = |error: engine::EditError| AgentError::from_edit(&error);
    let mut result = Map::new();
    match edit {
        AgentEdit::TrackAdd { kind } => {
            state.apply_agent_command(|project| engine::add_track(project, kind)).map_err(refused)?;
            result.insert("track".to_owned(), json!(state.project().tracks.len() - 1));
        }
        AgentEdit::TrackSetLocked { track, locked } => state.set_track_locked(track, locked).map_err(refused)?,
        AgentEdit::TrackSetHidden { track, hidden } => state.apply_agent_command(|project| engine::set_track_hidden(project, track, hidden)).map_err(refused)?,
        AgentEdit::ClipAdd { track, source, start, source_in, duration } => {
            let clip = added_clip(state, track, &source, start, source_in, duration, probe)?;
            state.apply_agent_command(|project| engine::add_clip(project, track, clip)).map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(clip_at(state.project(), track, start)));
        }
        AgentEdit::ClipMove { clip, track, start } => {
            let landing = landing_start(state.project(), clip, track, start);
            state.apply_agent_command(|project| engine::move_clip(project, clip.into(), track, start)).map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(landing.and_then(|start| clip_at(state.project(), track, start))));
        }
        AgentEdit::ClipTrimStart { clip, start } => {
            state.apply_agent_command(|project| engine::trim_start(project, clip.into(), start)).map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(clip_at(state.project(), clip.track, start)));
        }
        AgentEdit::ClipTrimEnd { clip, end } => {
            state.apply_agent_command(|project| engine::trim_end(project, clip.into(), end)).map_err(refused)?;
            result.insert("clip".to_owned(), json!(clip));
        }
        AgentEdit::ClipSplit { clip, at } => {
            state.apply_agent_command(|project| engine::split_clip(project, clip.into(), at)).map_err(refused)?;
            result.insert("clip".to_owned(), json!(ClipRef { track: clip.track, clip: clip.clip + 1 }));
        }
        AgentEdit::ClipDelete { clip } => state.apply_agent_command(|project| engine::delete_clip(project, clip.into())).map_err(refused)?,
        AgentEdit::EffectAdd { clip, package, params } => state.apply_agent_command(|project| engine::add_effect(project, clip.into(), Effect { package, params })).map_err(refused)?,
        AgentEdit::EffectRemove { clip, effect } => state.apply_agent_command(|project| engine::remove_effect(project, clip.into(), effect)).map_err(refused)?,
        AgentEdit::TransitionAdd { track, after_clip, package, duration, params } => {
            let transition = Transition { after_clip, package, duration: duration.unwrap_or(DEFAULT_TRANSITION_DURATION), params };
            state.apply_agent_command(|project| engine::add_transition(project, track, transition)).map_err(refused)?;
        }
        AgentEdit::TransitionRemove { track, after_clip } => state.apply_agent_command(|project| engine::remove_transition(project, track, after_clip)).map_err(refused)?,
        AgentEdit::ClipSetGain { clip, gain_db } => state.apply_agent_command(|project| engine::set_gain(project, clip.into(), gain_db)).map_err(refused)?,
        AgentEdit::ClipSetMuted { clip, muted } => state.apply_agent_command(|project| engine::set_muted(project, clip.into(), muted)).map_err(refused)?,
        AgentEdit::ClipSetFades { clip, fade_in, fade_out } => state.apply_agent_command(|project| engine::set_fades(project, clip.into(), fade_in, fade_out)).map_err(refused)?,
    }
    Ok(Value::Object(result))
}

fn replaceable<H: AgentHost>(host: &mut H, discard_changes: bool) -> Result<(), AgentError> {
    if host.tasks_busy() {
        return Err(busy("a dialog or a load is in progress"));
    }
    let state = host.state();
    if let Some(reason) = state.agent_busy_reason() {
        return Err(busy(reason));
    }
    if state.is_dirty() && !discard_changes {
        return Err(AgentError::new("unsaved_changes", "the project has unsaved changes: save it or pass discard_changes"));
    }
    Ok(())
}

fn locked_error(file: &Path) -> AgentError {
    AgentError::new("project_locked", format!("{} is open in another process", file.display()))
}

fn project_open<H: AgentHost>(host: &mut H, path: &Path, discard_changes: bool, loaded: Option<Result<Project, AgentError>>) -> Result<Value, AgentError> {
    replaceable(host, discard_changes)?;
    let project = loaded.unwrap_or_else(|| Err(AgentError::new("internal", "the project was not read")))?;
    let file = std::path::absolute(path).map_err(|e| AgentError::new("invalid_path", e.to_string()))?;
    let state = host.state();
    if !state.claim_project_lock(&file) {
        return Err(locked_error(&file));
    }
    state.replace_project(project, Some(file));
    host.project_replaced();
    Ok(json!({}))
}

fn project_save<H: AgentHost>(host: &mut H, path: Option<&Path>) -> Result<Value, AgentError> {
    let state = host.state();
    if state.is_live() {
        return Err(busy("the user is dragging a control"));
    }
    let file = match path {
        Some(path) => std::path::absolute(path).map_err(|e| AgentError::new("invalid_path", e.to_string()))?,
        None => state.project_file().map(Path::to_path_buf).ok_or_else(|| AgentError::new("no_project_file", "the project has no file yet: give a path"))?,
    };
    if state.project_file() != Some(file.as_path()) && project_lock::held_elsewhere(&file) {
        return Err(locked_error(&file));
    }
    save_project_file(state.project(), &file).map_err(|message| AgentError::new("save_failed", message))?;
    state.claim_project_lock(&file);
    state.mark_saved(file.clone());
    Ok(json!({"path": file}))
}

fn project_summary(project: &Project) -> Value {
    let frames = frame_count(project);
    let duration = project.tracks.iter().flat_map(|track| &track.clips).filter_map(Clip::end).max().unwrap_or(Time::ZERO);
    json!({"project": project, "duration": duration, "frames": frames})
}

fn session_status<H: AgentHost>(host: &mut H) -> Value {
    let mode = host.mode();
    let state = host.state();
    json!({"mode": mode, "project_file": state.project_file(), "dirty": state.is_dirty(), "exporting": state.is_exporting(), "holding": state.is_holding(), "can_undo": state.can_undo(), "can_redo": state.can_redo()})
}

fn undo_or_redo<H: AgentHost>(host: &mut H, expected: u64, undo: bool) -> Result<Value, AgentError> {
    writable(host, expected)?;
    let state = host.state();
    let stepped = if undo { state.agent_undo() } else { state.agent_redo() };
    if stepped {
        Ok(json!({}))
    } else {
        Err(AgentError::new(if undo { "nothing_to_undo" } else { "nothing_to_redo" }, "no step to take"))
    }
}

fn frame_capture<H: AgentHost>(host: &mut H, target: CaptureTarget, max_width: u32) -> Result<PendingFrame, AgentError> {
    let state = host.state();
    if state.is_exporting() {
        return Err(busy("an export is running"));
    }
    let revision = state.revision();
    let project = Arc::new(state.project().clone());
    let frame = match target {
        CaptureTarget::Time(time) if time >= Time::ZERO => frame_index(&project, time),
        CaptureTarget::Time(_) => -1,
        CaptureTarget::Frame(frame) => frame,
    };
    let frames = frame_count(&project);
    if frame < 0 || frame >= frames {
        return Err(AgentError { code: "out_of_range".to_owned(), message: format!("the timeline has {frames} frames: nothing at {target:?}"), data: json!({"frames": frames}) });
    }
    let time = frame_time(&project, frame).ok_or_else(|| AgentError::new("out_of_range", "the time of the frame overflows"))?;
    let (width, height) = (project.output.width, project.output.height);
    let rgba = host.capture(project, frame);
    Ok(PendingFrame { rgba, width, height, frame, time, revision, max_width })
}

/// Runs a prepared request against the host's state. `revision` is added to every object it answers.
pub fn execute<H: AgentHost>(host: &mut H, prepared: Prepared) -> Step {
    let Prepared { request, loaded, probe, packages_problem, package_list } = prepared;
    let result = match request {
        AgentRequest::SessionStatus => Ok(session_status(host)),
        AgentRequest::ProjectNew { discard_changes } => replaceable(host, discard_changes).map(|()| {
            let state = host.state();
            state.new_project();
            state.release_project_lock();
            host.project_replaced();
            json!({})
        }),
        AgentRequest::ProjectOpen { path, discard_changes } => project_open(host, &path, discard_changes, loaded),
        AgentRequest::ProjectSave { path } => project_save(host, path.as_deref()),
        AgentRequest::ProjectGet => Ok(project_summary(host.state().project())),
        AgentRequest::PackagesList { .. } => Ok(package_list.unwrap_or_default()),
        AgentRequest::Edit { expected_revision, edit } => match packages_problem {
            Some(error) => Err(error),
            None => writable(host, expected_revision).and_then(|()| run_edit(host.state(), edit, probe)),
        },
        AgentRequest::Undo { expected_revision } => undo_or_redo(host, expected_revision, true),
        AgentRequest::Redo { expected_revision } => undo_or_redo(host, expected_revision, false),
        AgentRequest::FrameCapture { target, max_width } => match frame_capture(host, target, max_width) {
            Ok(frame) => return Step::Frame(frame),
            Err(error) => Err(error),
        },
        AgentRequest::ExportStart { .. } | AgentRequest::ExportStatus { .. } | AgentRequest::ExportCancel { .. } => Err(AgentError::new("internal", "not available yet")),
    };
    Step::Done(with_revision(result, host.state().revision()))
}

fn with_revision(result: AgentResult, revision: u64) -> AgentResult {
    result.map(|mut value| {
        if let Value::Object(object) = &mut value {
            object.insert("revision".to_owned(), json!(revision));
        }
        value
    })
}
