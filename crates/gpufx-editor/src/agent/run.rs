//! Runs one request against an editor state. Work that reads the disk goes in [`prepare`], so an
//! open window does it away from its UI thread; [`execute`] is quick and runs where the state lives.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use base64::Engine as _;

use d3d12_backend::{animation_timing, is_asset_source, overwritten_input, probe_media, read_package_info, same_file, EncoderChoice, AnimationTiming, ExportJob, MediaProbe, ParamKind, ProjectPaths};
use engine::{clip_at, frame_count, frame_time, nearest_free_start, Clip, ClipKey, Effect, KeyframeTarget, Project, Time, TrackKind, Transition};
use serde_json::{json, Map, Value};

use super::image::encode_png;
use super::protocol::{AgentEdit, AgentError, AgentRequest, AgentResult, CaptureTarget, ClipRef};
use super::text::{animation_list, apply_animation_patch, apply_patch, check_font_pack, font_list, patched_animation, styled_base, subtitle_style};
use crate::beats::BeatStatus;
use crate::editor::read_subtitle_file;
use crate::subtitle_track::track_srt;
use crate::clip_animation_library::AnimatedClipKind;
use crate::editor::{even_side, new_clip, AgentExport, MAX_OUTPUT_SIDE, EditorState, DEFAULT_CLIP_DURATION, DEFAULT_IMAGE_DURATION, DEFAULT_TRANSITION_DURATION};
use crate::export::{export_blocker, ExportOutcome};
use crate::i18n::Language;
use crate::media::{choose_track, TrackChoice};
use crate::package_manager::{PackSource, PackTab};
use crate::packages::{filter_entries, scan, PackageEntry, PackageKind};
use crate::text_clip::{default_text, natural_placement};
use crate::text_shapes::available_families;
use crate::text_style_library::subtitle_placement;
use crate::sticker_library::animated_duration;
use crate::project_file::{load_project_file_for_agent, save_project_file};
use crate::preview::plan::frame_index;
use crate::project_lock;

/// A request with what had to be read from disk for it.
#[derive(Debug)]
pub struct Prepared {
    pub request: AgentRequest,
    loaded: Option<Result<Project, AgentError>>,
    probe: Option<Result<MediaProbe, AgentError>>,
    /// The loop of the picture a `clip_add` adds, when it is an animated WebP or GIF.
    animation: Option<AnimationTiming>,
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
    /// Starts exporting the project to `out`. `Err` when the job is refused before it starts.
    fn start_export(&mut self, out: PathBuf, encoder: EncoderChoice) -> Result<(), AgentError>;
    /// Asks the running export to stop.
    fn cancel_export(&mut self);
    /// Whether a dialog, a prompt or a load of the window is in the way.
    fn tasks_busy(&self) -> bool {
        false
    }
    /// How files of the open project resolve.
    fn paths(&self) -> Arc<ProjectPaths>;
    /// How files resolve for decoding pictures the way the preview shows them: with the proxies
    /// where there are some, so a tracker run reads the frames the player and its boxes show.
    fn decode_paths(&self) -> Arc<ProjectPaths> {
        self.paths()
    }
    /// The file the running export writes, if one runs.
    fn export_out(&self) -> Option<PathBuf>;
    /// The project was replaced: what was built from the old one goes.
    fn project_replaced(&mut self) {}
}

/// Reads what `request` needs from disk.
pub fn prepare(request: AgentRequest, package_roots: &[PathBuf]) -> Prepared {
    let mut prepared = Prepared { request, loaded: None, probe: None, animation: None, packages_problem: None, package_list: None };
    match &prepared.request {
        AgentRequest::ProjectOpen { path, .. } => prepared.loaded = Some(load(path, package_roots)),
        AgentRequest::PackagesList { filter } => prepared.package_list = Some(package_list(package_roots, filter.as_deref())),
        AgentRequest::InstalledPacks { kind } => prepared.package_list = Some(installed_packs(package_roots, kind.as_deref())),
        AgentRequest::Edit { edit: AgentEdit::ClipAdd { source, .. }, .. } => {
            prepared.probe = Some(probe(source, package_roots));
            prepared.animation = animation(source, package_roots);
        }
        AgentRequest::Edit { edit: AgentEdit::EffectAdd { package, .. } | AgentEdit::TransitionAdd { package, .. }, .. } => prepared.packages_problem = package_problem(package, package_roots),
        _ => {}
    }
    prepared
}

fn load(path: &Path, package_roots: &[PathBuf]) -> Result<Project, AgentError> {
    let file = std::path::absolute(path).map_err(|e| AgentError::new("invalid_path", e.to_string()))?;
    load_project_file_for_agent(&file, package_roots).map_err(|message| AgentError::new("invalid_project", message))
}

/// The loop of the picture at `source` when it is an animated WebP or GIF; `None` for anything
/// else or a file that cannot be read, which [`probe`] reports.
fn animation(source: &Path, package_roots: &[PathBuf]) -> Option<AnimationTiming> {
    let text = source.to_string_lossy();
    if !engine::is_image_source(&text) {
        return None;
    }
    let file = if is_asset_source(&text) { ProjectPaths::roots_only(Path::new("agent.json"), package_roots.to_vec()).asset_path(&text).ok()? } else { source.to_path_buf() };
    animation_timing(&file).ok().flatten()
}

fn probe(source: &Path, package_roots: &[PathBuf]) -> Result<MediaProbe, AgentError> {
    let text = source.to_string_lossy();
    if is_asset_source(&text) {
        let paths = ProjectPaths::roots_only(Path::new("agent.json"), package_roots.to_vec());
        let file = paths.asset_path(&text).map_err(|problem| AgentError::new("source_not_found", problem.to_string()))?;
        return probe_media(&file).map_err(|message| AgentError::new("invalid_source", message));
    }
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
    json!({"package": entry.package, "name": entry.name, "description": entry.description, "params": entry.params.iter().filter(|param| !crate::params::is_clip_clock(&param.name)).map(|param| &param.name).collect::<Vec<_>>()})
}

fn package_list(package_roots: &[PathBuf], filter: Option<&str>) -> Value {
    let catalog = scan(package_roots, read_package_info);
    let filter = filter.unwrap_or("");
    let list = |kind| filter_entries(catalog.entries(kind), filter).iter().map(entry_json).collect::<Vec<_>>();
    json!({"effects": list(PackageKind::Effect), "transitions": list(PackageKind::Transition)})
}

/// The installed packs, turned off or not, of `kind` or of every kind.
fn installed_packs(package_roots: &[PathBuf], kind: Option<&str>) -> Value {
    let user_root = crate::project_file::user_package_root();
    let tab = kind.and_then(PackTab::from_code);
    if kind.is_some() && tab.is_none() {
        return Value::Null;
    }
    let packs: Vec<Value> = crate::package_manager::list_installed(package_roots, user_root.as_deref())
        .into_iter()
        .filter(|pack| tab.is_none_or(|tab| pack.tab == tab))
        .map(|pack| {
            json!({
                "kind": pack.tab.code(),
                "key": pack.key,
                "id": pack.id,
                "name": pack.name,
                "version": pack.version,
                "source": match pack.source { PackSource::Shipped => "shipped", PackSource::User => "user" },
                "items": pack.items,
                "bytes": pack.bytes,
                "enabled": pack.enabled,
                "hidden": pack.hidden,
                "shadowed": pack.shadowed,
                "problem": pack.problem,
            })
        })
        .collect();
    json!({"packs": packs})
}

fn busy(reason: &str) -> AgentError {
    AgentError::new("busy", reason)
}

const TASKS_BUSY: &str = "a dialog, a prompt or a load is in progress";

/// Checks an export job, telling a project that cannot be rendered from an output that may not be written.
pub fn check_export(job: &ExportJob) -> Result<(), AgentError> {
    job.check_project().map_err(|message| AgentError::new("invalid_project", message))?;
    job.check_outputs("the output", "the hash file").map_err(|message| AgentError::new("invalid_output", message))
}

fn writable<H: AgentHost>(host: &mut H, expected: u64) -> Result<(), AgentError> {
    if host.tasks_busy() {
        return Err(busy(TASKS_BUSY));
    }
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
fn added_clip(state: &EditorState, track: usize, source: &Path, start: Time, source_in: Option<Time>, duration: Option<Time>, speed: Option<f64>, probe: Option<Result<MediaProbe, AgentError>>, animation: Option<AnimationTiming>) -> Result<Clip, AgentError> {
    let probe = probe.unwrap_or_else(|| Err(AgentError::new("internal", "the source was not probed")))?;
    let Some(target) = state.project().tracks.get(track) else {
        return Err(AgentError::from_edit(&engine::EditError::TrackNotFound { track }));
    };
    let (present, stated, stream, problem) = match target.kind {
        TrackKind::Video | TrackKind::Sticker | TrackKind::Text => (probe.has_video, probe.video_duration, "video", probe.video_problem),
        TrackKind::Audio => (probe.has_audio, probe.audio_duration, "audio", probe.audio_problem),
    };
    if !present {
        return Err(AgentError::new("invalid_source", format!("{} has no usable {stream} stream{}", source.display(), problem.map_or_else(String::new, |reason| format!(": {reason}")))));
    }
    let stored = source.to_string_lossy().into_owned();
    let stored = if is_asset_source(&stored) { stored } else { std::path::absolute(source).map_err(|e| AgentError::new("invalid_path", e.to_string()))?.to_string_lossy().into_owned() };
    let source_in = source_in.unwrap_or(Time::ZERO);
    let known = stated.filter(|length| *length > Time::ZERO);
    let remaining = match known {
        Some(length) if source_in >= length => return Err(AgentError::new("out_of_range", format!("source_in {source_in} is at or past the {length} of the {stream} stream"))),
        Some(length) => length.checked_sub(source_in),
        None => None,
    };
    // What the source still holds lasts longer or shorter on the timeline at another speed.
    let speed = speed.unwrap_or(1.0);
    check_speed(speed)?;
    let mut clip = new_clip(&stored, start, Time::ZERO);
    clip.source_in = source_in;
    if clip.is_image() {
        // A still has no source position or speed: it runs as long as asked, 5 seconds by default;
        // an animated one by default runs whole loops, as when it is dropped on the timeline.
        clip.source_in = Time::ZERO;
        clip.duration = duration.unwrap_or_else(|| animation.map_or(DEFAULT_IMAGE_DURATION, |timing| animated_duration(DEFAULT_IMAGE_DURATION, timing)));
        return Ok(clip);
    }
    clip.speed = engine::speed_milli(speed) as f64 / engine::SPEED_SCALE as f64;
    clip.duration = duration.or(remaining.map(|length| clip.timeline_ticks(length))).unwrap_or(DEFAULT_CLIP_DURATION);
    if let (Some(length), Some(duration)) = (known, duration) {
        if clip.source_end() > length {
            return Err(AgentError::new("out_of_range", format!("source_in {source_in} plus duration {duration} at speed {speed} reads past the {length} of the {stream} stream")));
        }
    }
    Ok(clip)
}

/// A speed the agent may not ask for is its mistake, told apart from a project that does not fit.
fn check_speed(speed: f64) -> Result<(), AgentError> {
    if engine::speed_in_range(speed) {
        Ok(())
    } else {
        Err(AgentError::new("out_of_range", format!("speed {speed} is outside {} to {}", engine::MIN_SPEED, engine::MAX_SPEED)))
    }
}

/// The clip `clip` refers to, refused like the engine commands refuse a missing one.
fn current_clip(project: &Project, clip: ClipRef) -> Result<&Clip, engine::EditError> {
    let key: ClipKey = clip.into();
    project.tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).ok_or(engine::EditError::ClipNotFound { key })
}

/// Only a float parameter that a package declares, and not one a clock drives, can have keyframes.
/// A package that is not in the catalog is left to the render to judge.
/// Refuses a keyframe of a parameter the effect has no float of, and, when `value` is given, one
/// outside the limits the package states for it.
fn check_keyframe_target(state: &EditorState, clip: ClipRef, target: &KeyframeTarget, value: Option<f64>) -> Result<(), AgentError> {
    let KeyframeTarget::EffectParam { effect, param } = target else { return Ok(()) };
    let package = state.project().tracks.get(clip.track).and_then(|track| track.clips.get(clip.clip)).and_then(|found| found.effects.get(*effect)).map(|found| found.package.as_str());
    let Some(infos) = package.and_then(|package| state.catalog.params_of(package)) else { return Ok(()) };
    match infos.iter().find(|info| info.name == *param) {
        Some(info) if info.kind == ParamKind::Float && !crate::params::is_clip_clock(param) => match value {
            Some(value) if !value.is_finite() || info.min.is_some_and(|min| value < min) || info.max.is_some_and(|max| value > max) => {
                Err(AgentError::new("invalid_params", format!("{value} is outside the limits of parameter {param}")))
            }
            _ => Ok(()),
        },
        Some(_) => Err(AgentError::new("invalid_params", format!("parameter {param} cannot have keyframes: only float parameters can"))),
        None => Err(AgentError::new("invalid_params", format!("the package has no parameter {param}"))),
    }
}

fn run_edit(state: &mut EditorState, edit: AgentEdit, probe: Option<Result<MediaProbe, AgentError>>, animation: Option<AnimationTiming>) -> Result<Value, AgentError> {
    let refused = |error: engine::EditError| AgentError::from_edit(&error);
    let mut result = Map::new();
    match edit {
        AgentEdit::TrackAdd { kind } => {
            state.apply_agent_command(|project| engine::add_track(project, kind)).map_err(refused)?;
            result.insert("track".to_owned(), json!(state.project().tracks.len() - 1));
        }
        AgentEdit::TrackSetLocked { track, locked } => state.set_track_locked(track, locked).map_err(refused)?,
        AgentEdit::TrackSetHidden { track, hidden } => state.apply_agent_command(|project| engine::set_track_hidden(project, track, hidden)).map_err(refused)?,
        AgentEdit::ProjectSetOutput { width, height, frame_rate } => {
            if width.is_none() && height.is_none() && frame_rate.is_none() {
                return Err(AgentError::new("invalid_params", "give at least one of width, height and frame_rate"));
            }
            for side in [width, height].into_iter().flatten() {
                if !(2..=MAX_OUTPUT_SIDE).contains(&side) {
                    return Err(AgentError::new("out_of_range", format!("side {side} is outside 2 to {MAX_OUTPUT_SIDE}")));
                }
            }
            if let Some(frame_rate) = frame_rate {
                if !frame_rate.is_output_rate() {
                    return Err(AgentError::new("out_of_range", format!("frame_rate {frame_rate} is outside {} to {} fps or has a zero part", engine::FrameRate::MIN, engine::FrameRate::MAX)));
                }
            }
            state
                .apply_agent_command(|project| {
                    let output = &project.output;
                    let sized = engine::set_output_size(project, even_side(width.unwrap_or(output.width)), even_side(height.unwrap_or(output.height)))?;
                    engine::set_frame_rate(&sized, frame_rate.map_or(output.frame_rate, |rate| engine::FrameRate::new(rate.num, rate.den)))
                })
                .map_err(refused)?;
            let output = &state.project().output;
            result.insert("output".to_owned(), json!({"width": output.width, "height": output.height, "frame_rate": output.frame_rate}));
        }
        AgentEdit::ClipAdd { track, source, start, source_in, duration, speed } => {
            let clip = added_clip(state, track, &source, start, source_in, duration, speed, probe, animation)?;
            state.apply_agent_command(|project| engine::add_clip(project, track, clip)).map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(clip_at(state.project(), track, start)));
        }
        AgentEdit::TextAdd { track, start, duration, content, style } => {
            if duration <= Time::ZERO {
                return Err(AgentError::new("invalid_params", "duration must be above 0"));
            }
            let patch = super::protocol::TextPatch { content: Some(content), ..style };
            let styled = styled_base(&state.text_style_library, &available_families(), &state.font_library.fonts, patch.style_id.as_deref(), &default_text(String::new()), &engine::ClipAnimation::default())?;
            check_font_pack(&state.font_paths, patch.font.as_ref())?;
            let text = apply_patch(&styled.text, &patch)?;
            let animation = apply_animation_patch(&state.clip_animation_library, AnimatedClipKind::Text, &styled.animation, &patch)?.unwrap_or(styled.animation);
            let animation = Some(animation).filter(|animation| !animation.is_empty());
            let placement = match styled.layout {
                Some(layout) => subtitle_placement(state.text_pixels(&text), state.output_pixels(), &layout),
                None => natural_placement(state.text_pixels(&text), state.output_pixels()),
            };
            let (track, new_track) = match track {
                Some(track) => (track, false),
                None => match choose_track(state.project(), TrackKind::Text, duration, start) {
                    TrackChoice::Existing(track) => (track, false),
                    TrackChoice::New => (state.project().tracks.len(), true),
                },
            };
            state
                .apply_agent_command(|project| {
                    let with_track = if new_track { engine::add_track(project, TrackKind::Text)? } else { project.clone() };
                    let next = engine::add_text_clip(&with_track, track, start, duration, text)?;
                    let Some(key) = clip_at(&next, track, start) else { return Ok(next) };
                    let next = match animation {
                        Some(animation) => engine::set_animation(&next, key, animation)?,
                        None => next,
                    };
                    match placement {
                        Some(placement) => engine::set_placement(&next, key, Some(placement)),
                        None => Ok(next),
                    }
                })
                .map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(clip_at(state.project(), track, start)));
        }
        AgentEdit::TextSet { clip, patch } => {
            if patch.is_empty() {
                return Err(AgentError::new("invalid_params", "give at least one field to change"));
            }
            let key = ClipKey::from(clip);
            let found = state.project().tracks.get(clip.track).ok_or(engine::EditError::TrackNotFound { track: clip.track }).and_then(|track| track.clips.get(clip.clip).ok_or(engine::EditError::ClipNotFound { key })).map_err(|error| AgentError::from_edit(&error))?;
            let Some(old) = found.text.as_ref() else { return Err(AgentError::from_edit(&engine::EditError::NotText { key })) };
            let styled = styled_base(&state.text_style_library, &available_families(), &state.font_library.fonts, patch.style_id.as_deref(), old, &found.animation)?;
            check_font_pack(&state.font_paths, patch.font.as_ref())?;
            let mut text = apply_patch(&styled.text, &patch)?;
            // A cue of a subtitle track given a style of its own keeps it when the track's changes.
            // The track's own style is not a style of its own.
            if let Some(style_id) = patch.style_id.as_deref().filter(|_| state.project().tracks[clip.track].is_subtitle()) {
                let track_style = state.project().tracks[clip.track].subtitle.as_ref().and_then(|subtitle| subtitle.style.as_deref());
                text.own_style = !track_style.is_some_and(|own| own.eq_ignore_ascii_case(style_id));
            }
            let animation = apply_animation_patch(&state.clip_animation_library, AnimatedClipKind::of(found), &styled.animation, &patch)?.unwrap_or(styled.animation);
            let animation = Some(animation).filter(|animation| *animation != found.animation);
            let placement = state.styled_text_placement(key, &text, styled.layout.as_ref());
            state
                .apply_agent_command(|project| {
                    let next = engine::set_text(project, key, text)?;
                    let next = match animation {
                        Some(animation) => engine::set_animation(&next, key, animation)?,
                        None => next,
                    };
                    match placement {
                        Some(placement) => engine::set_placement(&next, key, Some(placement)),
                        None => Ok(next),
                    }
                })
                .map_err(refused)?;
            result.insert("clip".to_owned(), json!(clip));
        }
        AgentEdit::ClipSetAnimation { clip, animation } => {
            let key = ClipKey::from(clip);
            let track = state.project().tracks.get(clip.track).ok_or(engine::EditError::TrackNotFound { track: clip.track }).map_err(|error| AgentError::from_edit(&error))?;
            let found = track.clips.get(clip.clip).ok_or(engine::EditError::ClipNotFound { key }).map_err(|error| AgentError::from_edit(&error))?;
            if track.is_audio() {
                return Err(AgentError::new("invalid_params", format!("track {} is an audio track: its clips have no picture to animate", clip.track)));
            }
            let next = patched_animation(&state.clip_animation_library, AnimatedClipKind::of(found), &found.animation, &animation)?;
            let mut removed = Vec::new();
            state
                .apply_agent_command(|project| {
                    engine::set_animation_report(project, key, next).map(|(next, transitions)| {
                        removed = transitions;
                        next
                    })
                })
                .map_err(refused)?;
            result.insert("clip".to_owned(), json!(clip));
            // A hold takes the transition of its cut away; say which.
            let removed: Vec<Value> = removed.iter().map(|tr| json!({"track": clip.track, "after_clip": tr.after_clip, "package": tr.package})).collect();
            result.insert("removed_transitions".to_owned(), Value::Array(removed));
        }
        AgentEdit::ClipMove { clip, track, start } => {
            let landing = landing_start(state.project(), clip, track, start);
            state.apply_agent_command(|project| engine::move_clip(project, clip.into(), track, start)).map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(landing.and_then(|start| clip_at(state.project(), track, start))));
        }
        AgentEdit::ClipTrimStart { clip, start } => {
            state.apply_agent_command(|project| engine::trim_start(project, clip.into(), start)).map_err(refused)?;
            // A trim keeps the order of the clips, and at another speed lands near `start`.
            result.insert("clip".to_owned(), json!(clip));
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
        AgentEdit::SubtitleImport { path, content, style_id } => {
            let srt = match (path, content) {
                (Some(path), None) => read_subtitle_file(&path).map_err(|reason| AgentError::new("invalid_path", format!("{}: {reason}", path.display())))?,
                (None, Some(content)) => content,
                _ => return Err(AgentError::new("invalid_params", "give exactly one of path and content")),
            };
            let style = match style_id.as_deref() {
                Some(style_id) => Some(subtitle_style(&state.text_style_library, &available_families(), style_id)?),
                None => None,
            };
            let (clips, notes) = state.subtitle_clips(&srt, style.as_ref());
            let warnings: Vec<Value> = notes.iter().map(|note| json!({"skipped": note.skipped(), "message": note.to_string()})).collect();
            if clips.is_empty() {
                return Err(AgentError { code: "invalid_params".to_owned(), message: "the subtitles hold no cue that can be read".to_owned(), data: json!({"warnings": warnings}) });
            }
            let (track, cues) = (state.project().tracks.len(), clips.len());
            let subtitle = engine::SubtitleTrack { style: style.map(|style| style.reference) };
            state.apply_agent_command(|project| engine::add_subtitle_track(project, subtitle, clips)).map_err(refused)?;
            result.insert("track".to_owned(), json!(track));
            result.insert("cues".to_owned(), json!(cues));
            result.insert("warnings".to_owned(), Value::Array(warnings));
        }
        AgentEdit::SubtitleSetStyle { track, style_id, all } => {
            let style = subtitle_style(&state.text_style_library, &available_families(), &style_id)?;
            let target = state.project().tracks.get(track).ok_or(engine::EditError::TrackNotFound { track }).map_err(|error| AgentError::from_edit(&error))?;
            if !target.is_subtitle() {
                return Err(AgentError::new("invalid_params", format!("track {track} is not a subtitle track; subtitle_import_srt makes one")));
            }
            let restyled = state.subtitle_restyle(track, &style, all).map_err(refused)?;
            let count = restyled.len();
            let subtitle = Some(engine::SubtitleTrack { style: Some(style.reference.clone()) });
            state.apply_agent_command(|project| engine::restyle_track(project, track, subtitle, restyled)).map_err(refused)?;
            result.insert("track".to_owned(), json!(track));
            result.insert("restyled".to_owned(), json!(count));
        }
        AgentEdit::EffectAdd { clip, package, params } => {
            let params = crate::params::without_clip_clock(state.catalog.params_of(&package), params);
            state.apply_agent_command(|project| engine::add_effect(project, clip.into(), Effect { package, params, keyframes: Default::default(), region: None })).map_err(refused)?
        }
        AgentEdit::EffectRemove { clip, effect } => state.apply_agent_command(|project| engine::remove_effect(project, clip.into(), effect)).map_err(refused)?,
        AgentEdit::CensorAdd { track, start, duration, placement, censor } => {
            let placement = placement.unwrap_or_else(|| crate::editor::censor_placement(state.output_pixels(), &censor));
            let (track, new_track) = match track {
                Some(track) => (track, false),
                None => match choose_track(state.project(), TrackKind::Sticker, duration, start) {
                    TrackChoice::Existing(track) => (track, false),
                    TrackChoice::New => (state.project().tracks.len(), true),
                },
            };
            state
                .apply_agent_command(|project| {
                    let with_track = if new_track { engine::add_track(project, TrackKind::Sticker)? } else { project.clone() };
                    engine::add_censor_clip(&with_track, track, start, duration, censor, Some(placement))
                })
                .map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(clip_at(state.project(), track, start)));
        }
        AgentEdit::ClipFollowSet { clip, track_id, reference, at, follow_scale, follow_rotation, smooth } => state
            .apply_agent_command(|project| {
                let shown = current_clip(project, reference)?;
                let at = at.unwrap_or(shown.start);
                let anchor_source_time = shown.source_time_at(at.saturating_sub(shown.start)).max(Time::ZERO);
                let link = engine::TrackLink { track_id, anchor_source_time, follow_scale, follow_rotation, smooth };
                engine::set_follow(project, clip.into(), Some(engine::Follow { link, ref_track: reference.track, ref_clip: reference.clip, offset: [0.0, 0.0] }))
            })
            .map_err(refused)?,
        AgentEdit::ClipFollowClear { clip } => state.apply_agent_command(|project| engine::set_follow(project, clip.into(), None)).map_err(refused)?,
        AgentEdit::TrackSetSample { track_id, clip, sample } => {
            let id = match (track_id, clip) {
                (Some(id), _) => {
                    state.apply_agent_command(|project| engine::set_track_sample(project, id, sample)).map_err(refused)?;
                    id
                }
                (None, Some(clip)) => {
                    let shown = current_clip(state.project(), clip).map_err(|error| AgentError::from_edit(&error))?.clone();
                    let id = state.project().next_motion_track_id();
                    let track = engine::MotionTrack { id, media_path: shown.source.clone(), aspect: state.clip_aspect(&shown), samples: vec![sample] };
                    state.apply_agent_command(|project| engine::set_motion_track(project, track)).map_err(refused)?;
                    id
                }
                (None, None) => return Err(AgentError::new("invalid_params", "give track_id, or clip to start a new track")),
            };
            result.insert("track_id".to_owned(), Value::from(id));
        }
        AgentEdit::TrackCreate { clip, at, sample } => {
            let shown = current_clip(state.project(), clip).map_err(|error| AgentError::from_edit(&error))?.clone();
            let at = at.unwrap_or(shown.start);
            if at < shown.start || at >= shown.start.saturating_add(shown.duration) {
                return Err(AgentError::new("invalid_params", "at is outside the clip"));
            }
            let source_time = shown.source_time_at(at.saturating_sub(shown.start)).max(Time::ZERO);
            let id = state.project().next_motion_track_id();
            let track = engine::MotionTrack { id, media_path: shown.source.clone(), aspect: state.clip_aspect(&shown), samples: vec![engine::TrackSample { source_time, confidence: 1.0, manual: true, ..sample }] };
            state.apply_agent_command(|project| engine::set_motion_track(project, track)).map_err(refused)?;
            result.insert("track_id".to_owned(), Value::from(id));
            result.insert("source_time".to_owned(), Value::from(source_time.ticks()));
        }
        AgentEdit::TrackSmooth { track_id, smooth } => state.apply_agent_command(|project| engine::set_track_smooth(project, track_id, smooth)).map_err(refused)?,
        AgentEdit::TrackBakeKeyframes { clip } => {
            let aspect = current_clip(state.project(), clip).map(|shown| state.clip_aspect(shown)).map_err(|error| AgentError::from_edit(&error))?;
            state.apply_agent_command(|project| engine::bake_follow(project, clip.into(), aspect)).map_err(refused)?
        }
        AgentEdit::TransitionAdd { track, after_clip, package, duration, params } => {
            let transition = Transition { after_clip, package, duration: duration.unwrap_or(DEFAULT_TRANSITION_DURATION), params };
            let mut released = Vec::new();
            state
                .apply_agent_command(|project| {
                    engine::add_transition_report(project, track, transition).map(|(next, holds)| {
                        released = holds;
                        next
                    })
                })
                .map_err(refused)?;
            // The holds on the cut became plain animations; say which.
            let released: Vec<Value> = released
                .iter()
                .map(|hold| json!({"clip": {"track": hold.key.track, "clip": hold.key.clip}, "group": if hold.group == engine::AnimationGroup::Enter { "in" } else { "out" }}))
                .collect();
            result.insert("released_holds".to_owned(), Value::Array(released));
        }
        AgentEdit::TransitionRemove { track, after_clip } => state.apply_agent_command(|project| engine::remove_transition(project, track, after_clip)).map_err(refused)?,
        AgentEdit::ClipSetSpeed { clip, speed } => {
            check_speed(speed)?;
            state.apply_agent_command(|project| engine::set_speed(project, clip.into(), speed)).map_err(refused)?;
        }
        AgentEdit::ClipSetGain { clip, gain_db } => state.apply_agent_command(|project| engine::set_gain(project, clip.into(), gain_db)).map_err(refused)?,
        AgentEdit::ClipSetMuted { clip, muted } => state.apply_agent_command(|project| engine::set_muted(project, clip.into(), muted)).map_err(refused)?,
        AgentEdit::ClipSetReverse { clip, reverse } => state.apply_agent_command(|project| engine::set_reverse(project, clip.into(), reverse)).map_err(refused)?,
        AgentEdit::ClipSetFades { clip, fade_in, fade_out } => state.apply_agent_command(|project| engine::set_fades(project, clip.into(), fade_in, fade_out)).map_err(refused)?,
        AgentEdit::ClipSetDenoise { clip, mode, strength } => {
            if let Some(strength) = strength.filter(|strength| !(0.0..=1.0).contains(strength)) {
                return Err(AgentError::new("out_of_range", format!("denoise strength {strength} is outside 0 to 1")));
            }
            state
                .apply_agent_command(|project| {
                    let current = current_clip(project, clip)?.denoise;
                    let denoise = engine::ClipDenoise { mode: mode.unwrap_or(current.mode), strength: strength.unwrap_or(current.strength) };
                    engine::set_denoise(project, clip.into(), denoise)
                })
                .map_err(refused)?;
        }
        AgentEdit::ClipSetMatte { clip, model, threshold, feather, edge, invert, temporal } => {
            if state.project().tracks.get(clip.track).is_some_and(|track| !track.kind.shows_picture()) {
                return Err(AgentError::new("no_picture", format!("track {} shows no picture, so its clips have no background to remove", clip.track)));
            }
            let max = engine::MAX_MATTE_PIXELS;
            for (name, value, range) in [("threshold", threshold, 0.0..=1.0), ("feather", feather, 0.0..=max), ("edge", edge, -max..=max), ("temporal", temporal.map(f64::from), 0.0..=f64::from(engine::MAX_MATTE_TEMPORAL))] {
                if let Some(value) = value.filter(|value| !range.contains(value)) {
                    return Err(AgentError::new("out_of_range", format!("matte {name} {value} is outside {} to {}", range.start(), range.end())));
                }
            }
            state
                .apply_agent_command(|project| {
                    let current = current_clip(project, clip)?.matte;
                    let matte = engine::ClipMatte {
                        model: model.unwrap_or(current.model),
                        threshold: threshold.unwrap_or(current.threshold),
                        feather: feather.unwrap_or(current.feather),
                        edge: edge.unwrap_or(current.edge),
                        invert: invert.unwrap_or(current.invert),
                        temporal: temporal.unwrap_or(current.temporal),
                    };
                    engine::set_matte(project, clip.into(), matte)
                })
                .map_err(refused)?;
        }
        AgentEdit::ClipSetVoice { clip, semitones, effect } => {
            if let Some(semitones) = semitones.filter(|semitones| !(-engine::MAX_PITCH_SEMITONES..=engine::MAX_PITCH_SEMITONES).contains(semitones)) {
                return Err(AgentError::new("out_of_range", format!("semitones {semitones} is outside -{max} to {max}", max = engine::MAX_PITCH_SEMITONES)));
            }
            state
                .apply_agent_command(|project| {
                    let current = current_clip(project, clip)?.voice;
                    let voice = engine::ClipVoice { semitones: semitones.unwrap_or(current.semitones), effect: effect.unwrap_or(current.effect) };
                    engine::set_voice(project, clip.into(), voice)
                })
                .map_err(refused)?;
        }
        AgentEdit::ClipSetTransform { clip, placement, opacity, rotation } => state
            .apply_agent_command(|project| {
                let mut next = project.clone();
                if let Some(placement) = placement {
                    next = engine::set_placement(&next, clip.into(), Some(placement))?;
                }
                if let Some(opacity) = opacity {
                    next = engine::set_opacity(&next, clip.into(), opacity)?;
                }
                if let Some(rotation) = rotation {
                    next = engine::set_rotation(&next, clip.into(), rotation)?;
                }
                Ok(next)
            })
            .map_err(refused)?,
        AgentEdit::KeyframeSet { clip, target, at, value, easing } => {
            check_keyframe_target(state, clip, &target, Some(value))?;
            state.apply_agent_command(|project| engine::set_keyframe(project, clip.into(), &target, at, value, easing)).map_err(refused)?
        }
        // No check of the target: a keyframe of a parameter that has since changed kind must still go.
        AgentEdit::KeyframeRemove { clip, target, at } => {
            state.apply_agent_command(|project| engine::remove_keyframe(project, clip.into(), &target, at)).map_err(refused)?
        }
        AgentEdit::MarkerAdd { at, label, color } => {
            let label = label.unwrap_or_default();
            check_marker(at, &label)?;
            let mut index = 0;
            state
                .apply_agent_command(|project| {
                    engine::add_marker_report(project, engine::Marker { at, label, color }).map(|(next, at_index)| {
                        index = at_index;
                        next
                    })
                })
                .map_err(refused)?;
            result.insert("marker".to_owned(), json!(index));
        }
        AgentEdit::MarkerRemove { marker } => state.apply_agent_command(|project| engine::remove_marker(project, marker)).map_err(refused)?,
        AgentEdit::MarkerMove { marker, at } => {
            check_marker(at, "")?;
            let mut index = marker;
            state
                .apply_agent_command(|project| {
                    engine::move_marker_report(project, marker, at).map(|(next, at_index)| {
                        index = at_index;
                        next
                    })
                })
                .map_err(refused)?;
            result.insert("marker".to_owned(), json!(index));
        }
        AgentEdit::ClipBeatsClear { clip } => state.apply_agent_command(|project| engine::set_beats(project, clip.into(), engine::ClipBeats::default())).map_err(refused)?,
    }
    Ok(Value::Object(result))
}

/// Refuses a marker time before zero and a label longer than the engine keeps.
fn check_marker(at: Time, label: &str) -> Result<(), AgentError> {
    if at < Time::ZERO {
        return Err(AgentError::new("out_of_range", format!("marker time {at} is negative")));
    }
    let length = label.chars().count();
    if length > engine::MAX_MARKER_LABEL {
        return Err(AgentError::new("out_of_range", format!("the label has {length} characters, more than {}", engine::MAX_MARKER_LABEL)));
    }
    Ok(())
}

/// The beats a clip plays (from the list of its source), in timeline time, as the tools give them.
fn beats_json(project: &Project, clip: &Clip) -> Value {
    Value::Array(project.clip_beat_times(clip).map(|(at, beat)| json!({"at": at, "downbeat": beat.downbeat})).collect())
}

fn markers_list(project: &Project) -> Value {
    let markers: Vec<Value> = project.markers.iter().enumerate().map(|(index, marker)| json!({"marker": index, "at": marker.at, "label": marker.label, "color": marker.color})).collect();
    let mut beats = Vec::new();
    for (track_index, track) in project.tracks.iter().enumerate() {
        for (clip_index, clip) in track.clips.iter().enumerate() {
            if let Some(found) = project.clip_beats(clip) {
                beats.push(json!({"clip": {"track": track_index, "clip": clip_index}, "source": clip.source, "tempo": found.tempo, "beats": beats_json(project, clip)}));
            }
        }
    }
    json!({"markers": markers, "beats": beats})
}

/// Polls first and starts a detection only when nothing is known for the file (or a failure is to
/// be retried), so a live agent that calls again and again neither restarts a failed detection nor
/// decodes the file anew; a failure is reported as `detect_failed` on every call until `retry`.
fn beats_detect<H: AgentHost>(host: &mut H, expected: u64, clip: ClipRef, retry: bool) -> Result<Value, AgentError> {
    writable(host, expected)?;
    let paths = host.paths();
    let wait = host.mode() == "file";
    let state = host.state();
    let found = current_clip(state.project(), clip).map_err(|error| AgentError::from_edit(&error))?;
    if found.is_still() {
        return Err(AgentError::new("invalid_params", format!("track {}, clip {} is a still or text, which has no sound", clip.track, clip.clip)));
    }
    if state.is_track_locked(clip.track) {
        return Err(AgentError::from_edit(&engine::EditError::TrackLocked { track: clip.track }));
    }
    let source = found.source.clone();
    let path = paths.source_path(&source);
    let begin = match state.beats.status(&path) {
        BeatStatus::Idle => true,
        BeatStatus::Failed(_) => retry,
        BeatStatus::Running(_) => wait,
        BeatStatus::Ready(_) => false,
    };
    let status = match (begin, wait) {
        (true, true) => state.beats.wait(&path),
        (true, false) => state.beats.start(&path),
        (false, _) => state.beats.status(&path),
    };
    match status {
        BeatStatus::Idle => Ok(json!({"state": "running", "progress": 0.0, "clip": clip})),
        BeatStatus::Running(progress) => Ok(json!({"state": "running", "progress": progress, "clip": clip})),
        BeatStatus::Failed(message) => Err(AgentError::new("detect_failed", message)),
        BeatStatus::Ready(analysis) => {
            let beats = analysis.clip_beats();
            let tempo = beats.tempo;
            state.apply_agent_command(|project| engine::set_source_beats(project, &source, beats)).map_err(|error| AgentError::from_edit(&error))?;
            let placed = current_clip(state.project(), clip).map_err(|error| AgentError::from_edit(&error))?;
            let shown = beats_json(state.project(), placed);
            let count = shown.as_array().map_or(0, Vec::len);
            Ok(json!({"state": "done", "clip": clip, "tempo": tempo, "count": count, "beats": shown}))
        }
    }
}

/// Starts a tracker run; a session without a window waits for it and answers its outcome.
fn track_run<H: AgentHost>(host: &mut H, expected: u64, track_id: u32, from: Option<Time>, direction: Option<String>, rotation: Option<bool>, mode: Option<String>) -> Result<Value, AgentError> {
    writable(host, expected)?;
    let mode = match mode.as_deref() {
        None => crate::tracking::TrackMode::Fast,
        Some(code) => crate::tracking::TrackMode::from_code(code).ok_or_else(|| AgentError::new("invalid_params", format!("mode {code}: fast or robust")))?,
    };
    let both = direction.as_deref() == Some("both");
    let direction = match direction.as_deref() {
        None | Some("both") => crate::tracking::Direction::Forward,
        Some(code) => crate::tracking::Direction::from_code(code).ok_or_else(|| AgentError::new("invalid_params", format!("direction {code}: forward, backward or both")))?,
    };
    let paths = host.decode_paths();
    let wait = host.mode() == "file";
    let state = host.state();
    let Some(track) = state.project().motion_track(track_id) else {
        return Err(AgentError::from_edit(&engine::EditError::MotionTrackNotFound { id: track_id }));
    };
    if let Some(job) = state.tracking.running_on(track_id) {
        return Err(AgentError::new("busy", format!("motion track {track_id} is being tracked by job {job}")));
    }
    let path = paths.video_decode_path(&track.media_path);
    let rotation = rotation.unwrap_or(true);
    let started = if both { state.start_tracking_both(path, track_id, from, rotation, mode) } else { state.start_tracking(path, track_id, from, direction, rotation, mode) };
    let job = started.map_err(|message| AgentError::new("invalid_params", message))?;
    if wait {
        state.tracking.wait(job);
        if both {
            // Writes the forward run, which starts the backward one.
            state.poll_tracking();
            if let Some(back) = state.chained_track_jobs.get(&job).copied() {
                state.tracking.wait(back);
            }
        }
    }
    let mut answer = track_status(state, Some(job))?;
    if both {
        answer["backward"] = match state.chained_track_jobs.get(&job).copied() {
            Some(back) => track_status(state, Some(back))?,
            None if state.has_chained_run(job) => json!({"state": "queued"}),
            None if state.chained_track_stale.contains(&job) => json!({"state": "stale"}),
            None => json!({"state": "not_started"}),
        };
    }
    Ok(answer)
}

/// Where tracker job `job` (the last one when absent) stands; writes finished runs first.
fn track_status(state: &mut EditorState, job: Option<u64>) -> Result<Value, AgentError> {
    let Some(job) = job.or(state.last_track_job) else {
        return Err(AgentError::new("job_not_found", "no tracker job was started"));
    };
    state.poll_tracking();
    if let Some(status) = state.tracking.status(job) {
        // Finished but not written yet: edits wait for a drag or an export.
        let (done, total) = match status {
            crate::tracking::RunState::Running { done, total } => (done, total),
            _ => (1, 1),
        };
        return Ok(json!({"job": job, "state": "running", "done": done, "total": total}));
    }
    let Some(report) = state.track_reports.get(&job) else {
        return Err(AgentError::new("job_not_found", format!("no tracker job {job}")));
    };
    let mut answer = json!({"job": job, "state": report.outcome, "track_id": report.track_id, "count": report.count});
    if let Some(last) = report.last {
        answer["last"] = json!({"source_time": last.source_time, "cx": last.cx, "cy": last.cy, "w": last.w, "h": last.h, "angle": last.angle, "confidence": last.confidence});
    }
    if let Some(message) = &report.message {
        answer["message"] = Value::from(message.clone());
    }
    Ok(answer)
}

fn replaceable<H: AgentHost>(host: &mut H, discard_changes: bool) -> Result<(), AgentError> {
    if host.tasks_busy() {
        return Err(busy(TASKS_BUSY));
    }
    // Unsaved work in an open window is the user's: an agent may not throw it away.
    let live = host.mode() == "live";
    let state = host.state();
    if let Some(reason) = state.agent_busy_reason() {
        return Err(busy(reason));
    }
    if state.is_dirty() && live {
        return Err(AgentError::new("unsaved_changes", "the open window has unsaved changes: the user must save or discard them"));
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

fn project_save<H: AgentHost>(host: &mut H, path: Option<&Path>, overwrite: bool) -> Result<Value, AgentError> {
    let (paths, running_out) = (host.paths(), host.export_out());
    let state = host.state();
    if state.is_live() {
        return Err(busy("the user is dragging a control"));
    }
    let file = match path {
        Some(path) => std::path::absolute(path).map_err(|e| AgentError::new("invalid_path", e.to_string()))?,
        None => state.project_file().map(Path::to_path_buf).ok_or_else(|| AgentError::new("no_project_file", "the project has no file yet: give a path"))?,
    };
    if let Some(what) = overwritten_input(state.project(), &paths, &file) {
        return Err(AgentError::new("invalid_output", format!("{} is {what}; it would be overwritten", file.display())));
    }
    if running_out.is_some_and(|out| same_file(&out, &file)) {
        return Err(AgentError::new("invalid_output", format!("{} is the output of the running export", file.display())));
    }
    if !state.project_file().is_some_and(|current| same_file(current, &file)) {
        if project_lock::held_elsewhere(&file) {
            return Err(locked_error(&file));
        }
        if file.exists() && !overwrite {
            return Err(AgentError::new("file_exists", format!("{} exists: pass overwrite to replace it", file.display())));
        }
    }
    save_project_file(state.project(), &file).map_err(|message| AgentError::new("save_failed", message))?;
    state.claim_project_lock(&file);
    state.mark_saved(file.clone());
    Ok(json!({"path": file}))
}

/// The text clips of subtitle track `track` as SRT, also written to `path` when given. A file that
/// is there is refused unless `overwrite`, and a file the project reads always is.
/// The installed music, sfx and sticker items matching `filter`, as `asset_list` answers.
fn asset_list(state: &EditorState, kind: Option<&str>, filter: &str) -> Result<Value, AgentError> {
    if let Some(name) = kind {
        if !matches!(name, "music" | "sfx" | "sticker") {
            return Err(AgentError::new("invalid_params", format!("type must be music, sfx or sticker, not '{name}'")));
        }
    }
    let wanted = |name: &str| kind.is_none_or(|kind| kind == name);
    let mut items = Vec::new();
    for (pack, sounds) in state.audio_library.matching(filter) {
        if !wanted(pack.kind.name()) {
            continue;
        }
        for sound in sounds {
            items.push(json!({"type": pack.kind.name(), "source": sound.source, "name": sound.name, "category": sound.category, "tags": sound.tags, "duration": sound.duration, "loop": sound.looped}));
        }
    }
    if wanted("sticker") {
        for sticker in state.sticker_library.matching(filter, None) {
            items.push(json!({"type": "sticker", "source": sticker.source, "name": sticker.name, "category": sticker.category, "tags": sticker.tags, "size": sticker.size}));
        }
    }
    let loaded = state.audio_library.loaded && state.sticker_library.loaded;
    let problems: Vec<Value> = state.audio_library.problems.iter().chain(&state.sticker_library.problems).map(|problem| json!({"pack": problem.pack, "message": problem.message})).collect();
    Ok(json!({"state": if loaded { "ready" } else { "loading" }, "items": items, "problems": problems}))
}

fn subtitle_export<H: AgentHost>(host: &mut H, track: usize, path: Option<&Path>, overwrite: bool) -> Result<Value, AgentError> {
    let paths = host.paths();
    let state = host.state();
    let target = state.project().tracks.get(track).ok_or_else(|| AgentError::from_edit(&engine::EditError::TrackNotFound { track }))?;
    if !target.is_subtitle() {
        return Err(AgentError::new("invalid_params", format!("track {track} is not a subtitle track")));
    }
    let srt = track_srt(target);
    let cues = target.clips.iter().filter(|clip| clip.is_text()).count();
    let file = match path {
        Some(path) => {
            let file = subtitle_export_file(state.project_file(), path)?;
            if let Some(what) = overwritten_input(state.project(), &paths, &file) {
                return Err(AgentError::new("invalid_output", format!("{} is {what}; it would be overwritten", file.display())));
            }
            if file.exists() && !overwrite {
                return Err(AgentError::new("file_exists", format!("{} exists: pass overwrite to replace it", file.display())));
            }
            std::fs::write(&file, &srt).map_err(|e| AgentError::new("save_failed", e.to_string()))?;
            Some(file)
        }
        None => None,
    };
    Ok(json!({"track": track, "cues": cues, "srt": srt, "path": file}))
}

/// The file a `subtitle_export_srt` writes: a `.srt` path, a relative one resolved against the
/// folder of the saved project. With no saved project there is nothing to resolve it against, and
/// the working directory of the editor is not something the agent can know, so it must be absolute.
fn subtitle_export_file(project_file: Option<&Path>, path: &Path) -> Result<PathBuf, AgentError> {
    if !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("srt")) {
        return Err(AgentError::new("invalid_params", format!("{} is not a .srt file", path.display())));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    match project_file.and_then(Path::parent) {
        Some(folder) => Ok(folder.join(path)),
        None => Err(AgentError::new("invalid_params", format!("{} is relative and the project is not saved: give an absolute path", path.display()))),
    }
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

fn export_json(state: &EditorState, record: &AgentExport) -> Value {
    let mut value = json!({"job": record.job, "out": record.out});
    let progress = state.export_progress();
    let fields = match &record.outcome {
        None => json!({"state": "running", "done": progress.map_or(0, |p| p.done), "total": progress.map_or(0, |p| p.total)}),
        Some(ExportOutcome::Finished(report)) => json!({"state": "finished", "report": report}),
        Some(ExportOutcome::Cancelled) => json!({"state": "cancelled"}),
        Some(ExportOutcome::Failed(message)) => json!({"state": "failed", "message": Language::English.text(message)}),
    };
    if let (Value::Object(target), Value::Object(fields)) = (&mut value, fields) {
        target.extend(fields);
    }
    value
}

fn export_start<H: AgentHost>(host: &mut H, out: &Path, overwrite: bool, codec: Option<String>, encoder: Option<String>) -> Result<Value, AgentError> {
    let encoder = EncoderChoice::parse(codec.as_deref(), encoder.as_deref()).map_err(|e| AgentError::new("invalid_encoder", e))?;
    if host.tasks_busy() {
        return Err(busy(TASKS_BUSY));
    }
    let state = host.state();
    if let Some(reason) = state.agent_busy_reason() {
        return Err(busy(reason));
    }
    if let Some(reason) = export_blocker(state.project()) {
        return Err(AgentError::new("nothing_to_export", Language::English.text(&reason)));
    }
    let out = std::path::absolute(out).map_err(|e| AgentError::new("invalid_path", e.to_string()))?;
    if out.exists() && !overwrite {
        return Err(AgentError::new("file_exists", format!("{} exists: pass overwrite to replace it", out.display())));
    }
    host.start_export(out.clone(), encoder)?;
    let job = host.state().begin_agent_export(out.clone());
    Ok(json!({"job": job, "out": out}))
}

/// The export `job` names, or the last one.
fn export_record(state: &EditorState, job: Option<u64>) -> Result<AgentExport, AgentError> {
    state.agent_export().filter(|record| job.is_none_or(|job| job == record.job)).cloned().ok_or_else(|| AgentError::new("job_not_found", "no such export"))
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
    let Prepared { request, loaded, probe, animation, packages_problem, package_list } = prepared;
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
        AgentRequest::ProjectSave { path, overwrite } => project_save(host, path.as_deref(), overwrite),
        AgentRequest::ProjectGet => Ok(project_summary(host.state().project())),
        AgentRequest::PackagesList { .. } => Ok(package_list.unwrap_or_default()),
        AgentRequest::InstalledPacks { kind } => match kind.as_deref().filter(|kind| PackTab::from_code(kind).is_none()) {
            Some(kind) => Err(AgentError::new("invalid_kind", format!("no pack kind {kind}; use one of {}", PackTab::ALL.map(PackTab::code).join(", ")))),
            None => Ok(package_list.unwrap_or_default()),
        },
        AgentRequest::FontList => Ok(font_list(&host.state().font_library)),
        AgentRequest::ClipAnimationList => Ok(animation_list(&host.state().clip_animation_library)),
        AgentRequest::TextStyleList { kind } => super::text::text_style_list(&host.state().text_style_library, kind.as_deref()),
        AgentRequest::AssetList { kind, filter } => asset_list(host.state(), kind.as_deref(), filter.as_deref().unwrap_or("")),
        AgentRequest::SubtitleExport { track, path, overwrite } => subtitle_export(host, track, path.as_deref(), overwrite),
        AgentRequest::MarkersList => Ok(markers_list(host.state().project())),
        AgentRequest::BeatsDetect { expected_revision, clip, retry } => beats_detect(host, expected_revision, clip, retry),
        AgentRequest::TrackRun { expected_revision, track_id, from, direction, rotation, mode } => track_run(host, expected_revision, track_id, from, direction, rotation, mode),
        AgentRequest::TrackStatus { job } => track_status(host.state(), job),
        AgentRequest::TrackCancel { job } => {
            let state = host.state();
            if let Some(job) = job.or(state.last_track_job) {
                state.tracking.cancel(job);
            }
            track_status(state, job)
        }
        AgentRequest::Edit { expected_revision, edit } => match packages_problem {
            Some(error) => Err(error),
            None => writable(host, expected_revision).and_then(|()| run_edit(host.state(), edit, probe, animation)),
        },
        AgentRequest::Undo { expected_revision } => undo_or_redo(host, expected_revision, true),
        AgentRequest::Redo { expected_revision } => undo_or_redo(host, expected_revision, false),
        AgentRequest::FrameCapture { target, max_width } => match frame_capture(host, target, max_width) {
            Ok(frame) => return Step::Frame(frame),
            Err(error) => Err(error),
        },
        AgentRequest::ExportStart { out, overwrite, codec, encoder } => export_start(host, &out, overwrite, codec, encoder),
        AgentRequest::ExportStatus { job } => {
            let state = host.state();
            export_record(state, job).map(|record| export_json(state, &record))
        }
        AgentRequest::ExportCancel { job } => match export_record(host.state(), job) {
            Ok(record) => {
                if record.outcome.is_none() {
                    host.cancel_export();
                }
                Ok(export_json(host.state(), &record))
            }
            Err(error) => Err(error),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    struct FakeHost {
        state: EditorState,
        mode: &'static str,
        busy: bool,
        running_out: Option<PathBuf>,
        started: Vec<PathBuf>,
        /// The paths pictures decode with, when they differ from the project's (a proxy).
        decode: Option<Arc<ProjectPaths>>,
    }

    impl FakeHost {
        fn new(mode: &'static str) -> Self {
            Self { state: EditorState::default(), mode, busy: false, running_out: None, started: Vec::new(), decode: None }
        }

        fn run(&mut self, request: AgentRequest) -> AgentResult {
            match execute(self, prepare(request, &[])) {
                Step::Done(result) => result,
                Step::Frame(_) => panic!("no capture in these tests"),
            }
        }
    }

    impl AgentHost for FakeHost {
        fn mode(&self) -> &'static str {
            self.mode
        }

        fn state(&mut self) -> &mut EditorState {
            &mut self.state
        }

        fn capture(&mut self, _project: Arc<Project>, _frame: i64) -> Receiver<Result<Vec<u8>, AgentError>> {
            channel().1
        }

        fn start_export(&mut self, out: PathBuf, _encoder: EncoderChoice) -> Result<(), AgentError> {
            self.started.push(out);
            Ok(())
        }

        fn cancel_export(&mut self) {}

        fn tasks_busy(&self) -> bool {
            self.busy
        }

        fn paths(&self) -> Arc<ProjectPaths> {
            Arc::new(ProjectPaths::new(Path::new("unsaved.json"), Vec::new()))
        }

        fn decode_paths(&self) -> Arc<ProjectPaths> {
            self.decode.clone().unwrap_or_else(|| self.paths())
        }

        fn export_out(&self) -> Option<PathBuf> {
            self.running_out.clone()
        }
    }

    fn code(result: AgentResult) -> String {
        result.expect_err("must be refused").code
    }

    #[test]
    fn markers_are_added_moved_listed_and_removed_as_undo_steps() {
        let mut host = FakeHost::new("file");
        let revision = |host: &mut FakeHost| host.state.revision();
        let add = |at: i64, label: Option<&str>| AgentEdit::MarkerAdd { at: Time::from_ticks(at), label: label.map(str::to_owned), color: None };
        let now = revision(&mut host);
        let first = host.run(AgentRequest::Edit { expected_revision: now, edit: add(90_000, Some("Drop")) }).expect("add");
        assert_eq!(first["marker"], 0);
        let now = revision(&mut host);
        let second = host.run(AgentRequest::Edit { expected_revision: now, edit: add(45_000, None) }).expect("add");
        assert_eq!(second["marker"], 0, "sorted by time");
        let now = revision(&mut host);
        let moved = host.run(AgentRequest::Edit { expected_revision: now, edit: AgentEdit::MarkerMove { marker: 0, at: Time::from_ticks(180_000) } }).expect("move");
        assert_eq!(moved["marker"], 1);
        let listed = host.run(AgentRequest::MarkersList).expect("list");
        assert_eq!(listed["markers"][0]["label"], "Drop");
        assert_eq!(listed["markers"][1]["at"], 180_000);
        assert_eq!(listed["beats"], json!([]));
        let now = revision(&mut host);
        assert_eq!(code(host.run(AgentRequest::Edit { expected_revision: now, edit: add(-1, None) })), "out_of_range");
        assert_eq!(code(host.run(AgentRequest::Edit { expected_revision: now, edit: AgentEdit::MarkerRemove { marker: 5 } })), "marker_not_found");
        host.run(AgentRequest::Edit { expected_revision: now, edit: AgentEdit::MarkerRemove { marker: 0 } }).expect("remove");
        assert_eq!(host.state.project().markers.len(), 1);
        let now = revision(&mut host);
        host.run(AgentRequest::Undo { expected_revision: now }).expect("undo");
        assert_eq!(host.state.project().markers.len(), 2);
    }

    #[test]
    fn a_tracker_run_reads_the_file_the_preview_decodes() {
        let mut host = FakeHost::new("file");
        host.decode = Some(Arc::new(ProjectPaths::new(Path::new("Z:/gpufx-preview-decodes/unsaved.json"), Vec::new())));
        let mut project = host.state.project().clone();
        let sample = engine::TrackSample { source_time: Time::ZERO, cx: 0.5, cy: 0.5, w: 0.2, h: 0.2, angle: 0.0, confidence: 1.0, manual: true };
        project.motion_tracks.push(engine::MotionTrack { id: 1, media_path: "missing.mp4".into(), aspect: 16.0 / 9.0, samples: vec![sample] });
        host.state.replace_project(project, None);
        let now = host.state.revision();
        let answer = host.run(AgentRequest::TrackRun { expected_revision: now, track_id: 1, from: None, direction: Some("forward".into()), rotation: Some(false), mode: None }).expect("an answer");
        assert_eq!(answer["state"], "failed", "{answer}");
        let message = answer["message"].as_str().unwrap_or_default();
        assert!(message.contains("gpufx-preview-decodes"), "opened through the decode paths: {message}");
        let now = host.state.revision();
        let refused = host.run(AgentRequest::TrackRun { expected_revision: now, track_id: 1, from: None, direction: None, rotation: None, mode: Some("slow".into()) });
        assert_eq!(code(refused), "invalid_params", "an unknown tracker mode");
    }

    /// Writes eight seconds of stereo 16-bit clicks at 120 BPM as a WAV file.
    fn click_wav(path: &Path) {
        let rate = 48_000_u32;
        let frames = rate as usize * 8;
        let mut samples = vec![0_i16; frames];
        for click in 0..15 {
            let first = click * rate as usize / 2 + rate as usize / 10;
            for n in 0..720 {
                let time = n as f64 / f64::from(rate);
                samples[first + n] = (20_000.0 * (-time / 0.004).exp() * (std::f64::consts::TAU * 1500.0 * time).sin()) as i16;
            }
        }
        let data = (frames * 4) as u32;
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(36 + data).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 0, 2, 0]);
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 4).to_le_bytes());
        bytes.extend_from_slice(&[4, 0, 16, 0]);
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn in_live_mode_beats_detect_runs_in_the_background_until_a_call_finds_it_done() {
        let folder = std::env::temp_dir().join(format!("gpufx-live-beats-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("clicks.wav");
        click_wav(&path);
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Audio);
        let project = engine::add_clip(host.state.project(), 0, new_clip(&path.to_string_lossy(), Time::ZERO, Time::from_ticks(8 * 90_000))).unwrap();
        host.state.replace_project(project, None);
        let clip = ClipRef { track: 0, clip: 0 };
        let first = host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: false }).expect("started");
        assert_eq!(first["state"], "running", "{first}");
        assert!(host.state.project().beats.is_empty(), "no edit while it runs");
        let started = std::time::Instant::now();
        let done = loop {
            let answer = host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: false }).expect("polled");
            if answer["state"] == "done" {
                break answer;
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(60), "detection hangs");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!((done["tempo"].as_f64().unwrap() - 120.0).abs() < 1.0, "{done}");
        assert!(host.state.project().clip_beats(&host.state.project().tracks[0].clips[0]).is_some());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn in_live_mode_a_failed_detection_is_reported_and_not_run_again() {
        let folder = std::env::temp_dir().join(format!("gpufx-live-beats-fail-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        // Not a sound file: probing it fails.
        let path = folder.join("broken.wav");
        std::fs::write(&path, b"not a wave file").unwrap();
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Audio);
        let project = engine::add_clip(host.state.project(), 0, new_clip(&path.to_string_lossy(), Time::ZERO, Time::from_ticks(90_000))).unwrap();
        host.state.replace_project(project, None);
        let clip = ClipRef { track: 0, clip: 0 };
        let started = std::time::Instant::now();
        let failed = loop {
            match host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: false }) {
                Ok(answer) => assert_eq!(answer["state"], "running", "{answer}"),
                Err(error) => break error,
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(30), "the failure is never reported");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(failed.code, "detect_failed", "{failed:?}");
        // A later call reports the same failure without decoding the file again.
        let again = host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: false }).expect_err("still failed");
        assert_eq!(again.code, "detect_failed");
        assert!(!host.state.beats.running(), "no new detection was started");
        assert!(host.state.project().beats.is_empty() && !host.state.can_undo());
        // An explicit retry runs it again.
        let retried = host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: true });
        assert!(retried.as_ref().is_ok_and(|answer| answer["state"] == "running") || retried.as_ref().is_err_and(|error| error.code == "detect_failed"), "{retried:?}");
        assert!(host.state.beats.running() || retried.is_err(), "the retry started a detection");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn in_live_mode_polling_after_done_adds_no_undo_step() {
        let folder = std::env::temp_dir().join(format!("gpufx-live-beats-again-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("clicks.wav");
        click_wav(&path);
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Audio);
        let project = engine::add_clip(host.state.project(), 0, new_clip(&path.to_string_lossy(), Time::ZERO, Time::from_ticks(8 * 90_000))).unwrap();
        host.state.replace_project(project, None);
        let clip = ClipRef { track: 0, clip: 0 };
        let started = std::time::Instant::now();
        while host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: false }).expect("polled")["state"] != "done" {
            assert!(started.elapsed() < std::time::Duration::from_secs(60), "detection hangs");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let revision = host.state.revision();
        for _ in 0..3 {
            let answer = host.run(AgentRequest::BeatsDetect { expected_revision: host.state.revision(), clip, retry: false }).expect("again");
            assert_eq!(answer["state"], "done");
        }
        assert_eq!(host.state.revision(), revision, "a repeated call changes nothing");
        host.state.undo();
        assert!(!host.state.can_undo(), "one undo step for the beats");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn beats_are_listed_in_timeline_time_and_cleared() {
        let mut host = FakeHost::new("file");
        host.state.add_track(TrackKind::Audio);
        let mut clip = new_clip("C:/media/song.mp3", Time::from_ticks(450_000), Time::from_ticks(180_000));
        clip.source_in = Time::from_ticks(90_000);
        let mut project = engine::add_clip(host.state.project(), 0, clip).unwrap();
        project.beats.insert("C:/media/song.mp3".into(), std::sync::Arc::new(engine::ClipBeats { tempo: 60.0, beats: (0..5).map(|second| engine::Beat { at: Time::from_ticks(second * 90_000), downbeat: second == 2 }).collect() }));
        host.state.replace_project(project, None);
        let listed = host.run(AgentRequest::MarkersList).expect("list");
        // The clip plays source 1 to 3 s from 5 s: the beats at source 1 and 2 s.
        assert_eq!(listed["beats"][0]["beats"], json!([{"at": 450_000, "downbeat": false}, {"at": 540_000, "downbeat": true}]));
        let clip = ClipRef { track: 0, clip: 0 };
        let now = host.state.revision();
        host.run(AgentRequest::Edit { expected_revision: now, edit: AgentEdit::ClipBeatsClear { clip } }).expect("clear");
        assert!(host.state.project().beats.is_empty());
        assert_eq!(host.run(AgentRequest::MarkersList).expect("list")["beats"], json!([]));
    }

    fn probed(length: Time) -> MediaProbe {
        MediaProbe { has_video: true, has_audio: false, video_duration: Some(length), audio_duration: None, video_problem: None, audio_problem: None }
    }

    fn added(source_in: Option<Time>, duration: Option<Time>, speed: Option<f64>) -> Result<Clip, AgentError> {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        added_clip(&state, 0, Path::new("a.mp4"), Time::ZERO, source_in, duration, speed, Some(Ok(probed(Time::from_ticks(9000)))), None)
    }

    #[test]
    fn a_clip_add_keeps_an_asset_source_and_refuses_an_escaping_or_missing_one() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Audio);
        let asset = Path::new("asset:sfx/p/audio/a.flac");
        let audio = MediaProbe { has_video: false, has_audio: true, video_duration: None, audio_duration: Some(Time::from_ticks(9000)), video_problem: None, audio_problem: None };
        let track = state.project().tracks.iter().rposition(|track| track.is_audio()).expect("an audio track was added");
        let clip = added_clip(&state, track, asset, Time::ZERO, None, None, None, Some(Ok(audio)), None).unwrap();
        assert_eq!(clip.source, "asset:sfx/p/audio/a.flac", "no absolute path in the project");
        let escaping = probe(Path::new("asset:sfx/../../secret.flac"), &[]).unwrap_err();
        assert_eq!(escaping.code, "source_not_found");
        let missing = probe(Path::new("asset:sfx/nothing/audio/a.flac"), &[]).unwrap_err();
        assert!(missing.message.contains("nothing") && missing.message.contains("not installed"), "{}", missing.message);
    }

    #[test]
    fn an_animated_sticker_added_without_a_duration_runs_whole_loops_as_a_drop_does() {
        let gif = Path::new(env!("CARGO_MANIFEST_DIR")).join("../d3d12-backend/tests/fixtures/animated_subframes.gif");
        let timing = animation(&gif, &[]).expect("an animated gif");
        assert_eq!(timing, AnimationTiming { loop_ms: 500, loops: 3 });
        assert_eq!(animation(Path::new("a.mp4"), &[]), None, "only a picture is read");
        let mut state = EditorState::default();
        state.add_track(TrackKind::Sticker);
        let track = state.project().tracks.iter().rposition(|track| track.kind == TrackKind::Sticker).expect("a sticker track");
        let add = |duration: Option<Time>, animation: Option<AnimationTiming>| added_clip(&state, track, &gif, Time::ZERO, None, duration, None, Some(Ok(probed(Time::ZERO))), animation).unwrap().duration;
        assert_eq!(add(None, Some(timing)), Time::from_ticks(1500 * 90), "three plays of 500 ms");
        assert_eq!(add(None, Some(AnimationTiming { loop_ms: 600, loops: 0 })), animated_duration(DEFAULT_IMAGE_DURATION, AnimationTiming { loop_ms: 600, loops: 0 }));
        assert_eq!(add(None, None), DEFAULT_IMAGE_DURATION, "a still keeps the default");
        assert_eq!(add(Some(Time::from_ticks(9000)), Some(timing)), Time::from_ticks(9000), "an asked duration wins");
    }

    #[test]
    fn the_default_length_of_a_clip_add_follows_its_speed_and_source_in() {
        assert_eq!(added(None, None, None).unwrap().duration, Time::from_ticks(9000));
        assert_eq!(added(None, None, Some(2.0)).unwrap().duration, Time::from_ticks(4500));
        let clip = added(Some(Time::from_ticks(3000)), None, Some(2.0)).unwrap();
        assert_eq!((clip.source_in, clip.duration), (Time::from_ticks(3000), Time::from_ticks(3000)));
    }

    #[test]
    fn an_explicit_duration_may_not_read_past_the_source() {
        // Exactly to the end is fine, at any speed.
        assert_eq!(added(Some(Time::from_ticks(3000)), Some(Time::from_ticks(3000)), Some(2.0)).unwrap().duration, Time::from_ticks(3000));
        assert_eq!(code(added(Some(Time::from_ticks(3000)), Some(Time::from_ticks(3002)), Some(2.0)).map(|_| json!({}))), "out_of_range");
        assert_eq!(code(added(None, Some(Time::from_ticks(9001)), None).map(|_| json!({}))), "out_of_range", "at 1x as well");
    }

    #[test]
    fn a_text_set_with_a_subtitle_style_leaves_a_place_with_keyframes_alone() {
        let mut host = FakeHost::new("live");
        host.state.text_style_library = crate::text_style_library::subtitle_library_for_test(&[]);
        host.state.add_text_at_playhead();
        let key = host.state.selection().expect("the new text is selected");
        host.state.toggle_keyframe(key, KeyframeTarget::Property(engine::ClipProperty::X), 0.2);
        let before = host.state.project().tracks[key.track].clips[key.clip].placement;
        let patch = super::super::protocol::TextPatch { style_id: Some("s/low".to_owned()), ..Default::default() };
        let edit = AgentEdit::TextSet { clip: ClipRef { track: key.track, clip: key.clip }, patch };
        let revision = host.state.revision();
        host.run(AgentRequest::Edit { expected_revision: revision, edit }).expect("text_set");
        let clip = &host.state.project().tracks[key.track].clips[key.clip];
        assert_eq!(clip.placement, before, "the layout does not overwrite a keyframed place");
        assert_eq!(clip.text.as_ref().map(|text| text.weight), Some(800));
    }

    #[test]
    fn a_subtitle_export_path_must_be_srt_and_absolute_unless_the_project_is_saved() {
        let saved = Path::new("C:/work/edit/p.json");
        let invalid = |result: Result<PathBuf, AgentError>| result.expect_err("refused").code;
        assert_eq!(invalid(subtitle_export_file(None, Path::new("out.srt"))), "invalid_params", "relative with no saved project");
        assert_eq!(invalid(subtitle_export_file(Some(saved), Path::new("out.txt"))), "invalid_params", "not .srt");
        assert_eq!(invalid(subtitle_export_file(Some(saved), Path::new("C:/work/p.json"))), "invalid_params", "the project file itself");
        assert_eq!(subtitle_export_file(Some(saved), Path::new("subs/out.SRT")).unwrap(), Path::new("C:/work/edit").join("subs/out.SRT"));
        let absolute = std::env::temp_dir().join("out.srt");
        assert_eq!(subtitle_export_file(None, &absolute).unwrap(), absolute);

        let mut host = FakeHost::new("live");
        let (clips, _) = host.state.subtitle_clips("1\n00:00:01,000 --> 00:00:02,000\nOne\n", None);
        host.state.apply_agent_command(|project| engine::add_subtitle_track(project, engine::SubtitleTrack { style: None }, clips)).unwrap();
        assert_eq!(code(host.run(AgentRequest::SubtitleExport { track: 0, path: Some(PathBuf::from("out.srt")), overwrite: false })), "invalid_params");
    }

    #[test]
    fn asset_list_names_sounds_and_stickers_by_type_and_filter() {
        use crate::asset_install::AudioPackKind;
        use crate::audio_library::{LibraryPack, LibrarySound};
        use crate::sticker_library::{LibrarySticker, StickerPack};
        let mut host = FakeHost::new("live");
        let sound = |name: &str| LibrarySound { source: format!("asset:music/m/{name}.mp3"), name: name.to_owned(), duration: None, tags: vec!["happy".to_owned()], category: String::new(), looped: false };
        host.state.audio_library.packs.push(LibraryPack { kind: AudioPackKind::Music, id: "m".to_owned(), name: "M".to_owned(), version: "1".to_owned(), sounds: vec![sound("Kids Fun"), sound("Calm")] });
        host.state.audio_library.loaded = true;
        host.state.sticker_library.packs.push(StickerPack { id: "s".to_owned(), name: "S".to_owned(), stickers: vec![LibrarySticker { source: "asset:sticker/s/star.png".to_owned(), name: "Star".to_owned(), tags: vec!["happy".to_owned()], category: String::new(), size: Some((64, 64)), animation: None }] });
        host.state.sticker_library.loaded = true;
        let sources = |result: AgentResult| result.expect("list")["items"].as_array().unwrap().iter().map(|item| item["source"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(sources(host.run(AgentRequest::AssetList { kind: None, filter: Some("happy".to_owned()) })).len(), 3);
        assert_eq!(sources(host.run(AgentRequest::AssetList { kind: Some("sticker".to_owned()), filter: None })), ["asset:sticker/s/star.png"]);
        assert_eq!(sources(host.run(AgentRequest::AssetList { kind: Some("music".to_owned()), filter: Some("kids".to_owned()) })), ["asset:music/m/Kids Fun.mp3"]);
        assert!(sources(host.run(AgentRequest::AssetList { kind: Some("sfx".to_owned()), filter: None })).is_empty());
        assert_eq!(code(host.run(AgentRequest::AssetList { kind: Some("video".to_owned()), filter: None })), "invalid_params");
    }

    #[test]
    fn project_set_output_changes_the_frame_in_one_undo_step() {
        let mut host = FakeHost::new("live");
        let before = host.state.project().output.clone();
        let revision = host.state.revision();
        let edit = AgentEdit::ProjectSetOutput { width: Some(1081), height: Some(1920), frame_rate: Some(engine::FrameRate { num: 48000, den: 2002 }) };
        let result = host.run(AgentRequest::Edit { expected_revision: revision, edit }).expect("set output");
        assert_eq!(result["output"], json!({"width": 1080, "height": 1920, "frame_rate": {"num": 24000, "den": 1001}}));
        let output = &host.state.project().output;
        assert_eq!((output.width, output.height, output.frame_rate), (1080, 1920, engine::FrameRate::NTSC_FILM));
        let revision = host.state.revision();
        host.run(AgentRequest::Undo { expected_revision: revision }).expect("undo");
        assert_eq!(host.state.project().output, before);
    }

    #[test]
    fn project_set_output_keeps_what_is_not_given_and_refuses_bad_values() {
        let mut host = FakeHost::new("live");
        let before = host.state.project().output.clone();
        let mut set = |width, height, frame_rate: Option<(u32, u32)>| {
            let revision = host.state.revision();
            let edit = AgentEdit::ProjectSetOutput { width, height, frame_rate: frame_rate.map(|(num, den)| engine::FrameRate { num, den }) };
            host.run(AgentRequest::Edit { expected_revision: revision, edit })
        };
        assert_eq!(code(set(None, None, None)), "invalid_params");
        assert_eq!(code(set(Some(0), None, None)), "out_of_range");
        assert_eq!(code(set(None, Some(MAX_OUTPUT_SIDE + 2), None)), "out_of_range");
        assert_eq!(code(set(None, None, Some((90, 1)))), "out_of_range");
        assert_eq!(code(set(None, None, Some((30, 0)))), "out_of_range");
        assert_eq!(code(set(None, None, Some((1, 2)))), "out_of_range");
        set(None, Some(720), None).expect("height alone");
        let output = &host.state.project().output;
        assert_eq!((output.width, output.height, output.frame_rate), (before.width, 720, before.frame_rate));
    }

    #[test]
    fn an_agent_adds_a_censor_region_that_follows_a_track_entered_by_hand() {
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Video);
        host.state.add_clip_from_file(0, Path::new("a.mp4"));
        let edit = |host: &mut FakeHost, edit: AgentEdit| {
            let revision = host.state.revision();
            host.run(AgentRequest::Edit { expected_revision: revision, edit })
        };
        let censor = engine::CensorContent { mode: engine::CensorMode::Pixelate { block: 20.0 }, ..Default::default() };
        let added = edit(&mut host, AgentEdit::CensorAdd { track: None, start: Time::ZERO, duration: Time::from_ticks(90_000), placement: None, censor }).expect("censor");
        let censor_clip = ClipRef { track: added["clip"]["track"].as_u64().unwrap() as usize, clip: added["clip"]["clip"].as_u64().unwrap() as usize };
        assert_eq!(host.state.project().tracks[censor_clip.track].kind, TrackKind::Sticker);
        assert!(host.state.project().tracks[censor_clip.track].clips[0].placement.is_some(), "placed in the middle");

        let video = ClipRef { track: 0, clip: 0 };
        let sample = |at: i64, cx: f64| engine::TrackSample { source_time: Time::from_ticks(at), cx, cy: 0.5, w: 0.1, h: 0.1, angle: 0.0, confidence: 1.0, manual: true };
        assert_eq!(code(edit(&mut host, AgentEdit::TrackSetSample { track_id: None, clip: None, sample: sample(0, 0.2) })), "invalid_params");
        let started = edit(&mut host, AgentEdit::TrackSetSample { track_id: None, clip: Some(video), sample: sample(0, 0.2) }).expect("new track");
        let id = started["track_id"].as_u64().unwrap() as u32;
        edit(&mut host, AgentEdit::TrackSetSample { track_id: Some(id), clip: None, sample: sample(90_000, 0.6) }).expect("second sample");
        assert_eq!(host.state.project().motion_tracks[0].samples.len(), 2);
        assert_eq!(code(edit(&mut host, AgentEdit::TrackSetSample { track_id: Some(id + 5), clip: None, sample: sample(0, 0.2) })), "motion_track_not_found");

        let follow = |track_id| AgentEdit::ClipFollowSet { clip: censor_clip, track_id, reference: video, at: None, follow_scale: false, follow_rotation: false, smooth: 0.0 };
        assert_eq!(code(edit(&mut host, follow(id + 5))), "invalid_project");
        edit(&mut host, follow(id)).expect("follow");
        let followed = host.state.project().tracks[censor_clip.track].clips[0].follow.expect("set");
        assert_eq!((followed.ref_track, followed.ref_clip, followed.link.anchor_source_time), (0, 0, Time::ZERO));
        edit(&mut host, AgentEdit::ClipFollowClear { clip: censor_clip }).expect("clear");
        assert!(host.state.project().tracks[censor_clip.track].clips[0].follow.is_none());
    }

    #[test]
    fn a_speed_out_of_range_is_out_of_range() {
        assert_eq!(code(added(None, None, Some(20.0)).map(|_| json!({}))), "out_of_range");
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Video);
        host.state.add_clip_from_file(0, Path::new("a.mp4"));
        let revision = host.state.revision();
        let edit = AgentEdit::ClipSetSpeed { clip: ClipRef { track: 0, clip: 0 }, speed: 0.01 };
        assert_eq!(code(host.run(AgentRequest::Edit { expected_revision: revision, edit })), "out_of_range");
    }

    #[test]
    fn an_agent_sets_the_matte_of_a_video_clip_but_not_of_an_audio_clip() {
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Video);
        host.state.add_clip_from_file(0, Path::new("a.mp4"));
        let set = |host: &mut FakeHost, edit: AgentEdit| {
            let revision = host.state.revision();
            host.run(AgentRequest::Edit { expected_revision: revision, edit })
        };
        let matte = |track| AgentEdit::ClipSetMatte { clip: ClipRef { track, clip: 0 }, model: Some(engine::MatteModel::Fast), threshold: None, feather: Some(3.0), edge: None, invert: None, temporal: None };
        set(&mut host, matte(0)).expect("video clip");
        assert_eq!(host.state.project().tracks[0].clips[0].matte, engine::ClipMatte { model: engine::MatteModel::Fast, feather: 3.0, ..Default::default() });
        host.state.add_track(TrackKind::Audio);
        let audio = host.state.project().tracks.iter().rposition(|track| track.is_audio()).expect("an audio track");
        assert_eq!(code(set(&mut host, matte(audio))), "no_picture");
    }

    #[test]
    fn an_agent_sets_the_denoise_and_voice_of_a_clip_keeping_what_it_leaves_out() {
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Video);
        host.state.add_clip_from_file(0, Path::new("a.mp4"));
        let clip = ClipRef { track: 0, clip: 0 };
        let now = |host: &FakeHost| host.state.project().tracks[0].clips[0].clone();
        let set = |host: &mut FakeHost, edit: AgentEdit| {
            let revision = host.state.revision();
            host.run(AgentRequest::Edit { expected_revision: revision, edit })
        };
        set(&mut host, AgentEdit::ClipSetDenoise { clip, mode: Some(engine::DenoiseMode::General), strength: Some(0.6) }).expect("denoise");
        let denoise = engine::ClipDenoise { mode: engine::DenoiseMode::General, strength: 0.6 };
        assert_eq!(now(&host).denoise, denoise);
        // What is left out keeps the clip's value.
        set(&mut host, AgentEdit::ClipSetDenoise { clip, mode: Some(engine::DenoiseMode::Voice), strength: None }).expect("mode only");
        assert_eq!(now(&host).denoise, engine::ClipDenoise { mode: engine::DenoiseMode::Voice, strength: 0.6 });
        set(&mut host, AgentEdit::ClipSetDenoise { clip, mode: None, strength: Some(0.3) }).expect("strength only");
        assert_eq!(now(&host).denoise, engine::ClipDenoise { mode: engine::DenoiseMode::Voice, strength: 0.3 });
        set(&mut host, AgentEdit::ClipSetVoice { clip, semitones: None, effect: Some(engine::VoiceEffect::Robot) }).expect("effect");
        set(&mut host, AgentEdit::ClipSetVoice { clip, semitones: Some(9.0), effect: None }).expect("semitones");
        assert_eq!(now(&host).voice, engine::ClipVoice { semitones: 9.0, effect: engine::VoiceEffect::Robot });
        // Each is one undo step.
        host.state.undo();
        assert_eq!(now(&host).voice, engine::ClipVoice { semitones: 0.0, effect: engine::VoiceEffect::Robot });
        let loud = AgentEdit::ClipSetDenoise { clip, mode: None, strength: Some(1.5) };
        assert_eq!(code(set(&mut host, loud)), "out_of_range");
        let high = AgentEdit::ClipSetVoice { clip, semitones: Some(13.0), effect: None };
        assert_eq!(code(set(&mut host, high)), "out_of_range");
        let missing = AgentEdit::ClipSetVoice { clip: ClipRef { track: 0, clip: 5 }, semitones: Some(1.0), effect: None };
        assert!(set(&mut host, missing).is_err());
        // The wire form names the values as the project file does.
        let wire = serde_json::to_value(AgentEdit::ClipSetVoice { clip, semitones: None, effect: Some(engine::VoiceEffect::Robot) }).unwrap();
        assert_eq!(wire, json!({"edit": "clip_set_voice", "clip": {"track": 0, "clip": 0}, "semitones": null, "effect": "robot"}));
    }

    #[test]
    fn an_agent_sets_and_removes_a_keyframe_and_the_time_is_checked() {
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Video);
        host.state.add_clip_from_file(0, Path::new("a.mp4"));
        let clip = ClipRef { track: 0, clip: 0 };
        let target = KeyframeTarget::Property(engine::ClipProperty::Opacity);
        let revision = host.state.revision();
        let set = AgentEdit::KeyframeSet { clip, target: target.clone(), at: Time::from_ticks(900), value: 0.5, easing: None };
        host.run(AgentRequest::Edit { expected_revision: revision, edit: set }).expect("set");
        assert!(host.state.project().tracks[0].clips[0].motion.has_keyframes(engine::ClipProperty::Opacity));
        let revision = host.state.revision();
        let outside = AgentEdit::KeyframeSet { clip, target: target.clone(), at: Time::from_ticks(i64::MAX / 2), value: 0.5, easing: None };
        assert_eq!(code(host.run(AgentRequest::Edit { expected_revision: revision, edit: outside })), "keyframe_outside_clip");
        let remove = AgentEdit::KeyframeRemove { clip, target, at: Time::from_ticks(900) };
        host.run(AgentRequest::Edit { expected_revision: revision, edit: remove }).expect("remove");
        assert!(!host.state.project().tracks[0].clips[0].motion.has_keyframes(engine::ClipProperty::Opacity));
    }

    #[test]
    fn export_start_waits_for_a_dialog_or_a_load() {
        let mut host = FakeHost::new("live");
        host.busy = true;
        let result = host.run(AgentRequest::ExportStart { out: PathBuf::from("out.mp4"), overwrite: false, codec: None, encoder: None });
        assert_eq!(code(result), "busy");
        assert!(host.started.is_empty());
    }

    #[test]
    fn export_start_refuses_an_unknown_codec_or_an_encoder_of_another_codec() {
        let mut host = FakeHost::new("live");
        for (codec, encoder) in [("vp9", None), ("hevc", Some("h264_nvenc"))] {
            let result = host.run(AgentRequest::ExportStart { out: PathBuf::from("out.mp4"), overwrite: false, codec: Some(codec.to_owned()), encoder: encoder.map(str::to_owned) });
            assert_eq!(code(result), "invalid_encoder");
        }
        assert!(host.started.is_empty());
    }

    #[test]
    fn edit_undo_and_redo_wait_for_a_dialog_a_prompt_or_a_load() {
        let mut host = FakeHost::new("live");
        host.state.add_track(TrackKind::Video);
        let revision = host.state.revision();
        host.busy = true;
        assert_eq!(code(host.run(AgentRequest::Edit { expected_revision: revision, edit: AgentEdit::TrackAdd { kind: TrackKind::Audio } })), "busy");
        assert_eq!(code(host.run(AgentRequest::Undo { expected_revision: revision })), "busy");
        assert_eq!(code(host.run(AgentRequest::Redo { expected_revision: revision })), "busy");
        assert_eq!(host.state.project().tracks.len(), 1, "nothing changed");
    }

    #[test]
    fn a_live_window_keeps_unsaved_work_and_the_file_mode_can_discard() {
        let mut live = FakeHost::new("live");
        live.state.add_track(TrackKind::Video);
        assert_eq!(code(live.run(AgentRequest::ProjectNew { discard_changes: true })), "unsaved_changes");
        assert_eq!(live.state.project().tracks.len(), 1, "the user's work stays");
        let mut file = FakeHost::new("file");
        file.state.add_track(TrackKind::Video);
        file.run(AgentRequest::ProjectNew { discard_changes: true }).expect("the file mode keeps discard_changes");
        assert!(file.state.project().tracks.is_empty());
    }

    #[test]
    fn saving_over_the_output_of_the_running_export_is_refused() {
        let dir = std::env::temp_dir().join(format!("gpufx-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.mp4");
        let mut host = FakeHost::new("file");
        host.running_out = Some(out.clone());
        let upper = dir.join("OUT.MP4");
        assert_eq!(code(host.run(AgentRequest::ProjectSave { path: Some(upper), overwrite: true })), "invalid_output");
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
