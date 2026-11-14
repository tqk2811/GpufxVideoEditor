//! Runs one request against an editor state. Work that reads the disk goes in [`prepare`], so an
//! open window does it away from its UI thread; [`execute`] is quick and runs where the state lives.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use base64::Engine as _;

use d3d12_backend::{is_asset_source, overwritten_input, probe_media, read_package_info, same_file, EncoderChoice, ExportJob, MediaProbe, ParamKind, ProjectPaths};
use engine::{clip_at, frame_count, frame_time, nearest_free_start, Clip, ClipKey, Effect, KeyframeTarget, Project, Time, TrackKind, Transition};
use serde_json::{json, Map, Value};

use super::image::encode_png;
use super::protocol::{AgentEdit, AgentError, AgentRequest, AgentResult, CaptureTarget, ClipRef};
use super::text::{animation_list, apply_animation_patch, apply_patch, font_list, patched_animation, styled_base, subtitle_style};
use crate::editor::read_subtitle_file;
use crate::subtitle_track::track_srt;
use crate::clip_animation_library::AnimatedClipKind;
use crate::editor::{new_clip, AgentExport, EditorState, DEFAULT_CLIP_DURATION, DEFAULT_IMAGE_DURATION, DEFAULT_TRANSITION_DURATION};
use crate::export::{export_blocker, ExportOutcome};
use crate::i18n::Language;
use crate::media::{choose_track, TrackChoice};
use crate::packages::{filter_entries, scan, PackageEntry, PackageKind};
use crate::text_clip::{default_text, natural_placement};
use crate::text_shapes::available_families;
use crate::text_style_library::subtitle_placement;
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
    /// The file the running export writes, if one runs.
    fn export_out(&self) -> Option<PathBuf>;
    /// The project was replaced: what was built from the old one goes.
    fn project_replaced(&mut self) {}
}

/// Reads what `request` needs from disk.
pub fn prepare(request: AgentRequest, package_roots: &[PathBuf]) -> Prepared {
    let mut prepared = Prepared { request, loaded: None, probe: None, packages_problem: None, package_list: None };
    match &prepared.request {
        AgentRequest::ProjectOpen { path, .. } => prepared.loaded = Some(load(path, package_roots)),
        AgentRequest::PackagesList { filter } => prepared.package_list = Some(package_list(package_roots, filter.as_deref())),
        AgentRequest::Edit { edit: AgentEdit::ClipAdd { source, .. }, .. } => prepared.probe = Some(probe(source, package_roots)),
        AgentRequest::Edit { edit: AgentEdit::EffectAdd { package, .. } | AgentEdit::TransitionAdd { package, .. }, .. } => prepared.packages_problem = package_problem(package, package_roots),
        _ => {}
    }
    prepared
}

fn load(path: &Path, package_roots: &[PathBuf]) -> Result<Project, AgentError> {
    let file = std::path::absolute(path).map_err(|e| AgentError::new("invalid_path", e.to_string()))?;
    load_project_file_for_agent(&file, package_roots).map_err(|message| AgentError::new("invalid_project", message))
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
fn added_clip(state: &EditorState, track: usize, source: &Path, start: Time, source_in: Option<Time>, duration: Option<Time>, speed: Option<f64>, probe: Option<Result<MediaProbe, AgentError>>) -> Result<Clip, AgentError> {
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
        // A still has no source position or speed: it runs as long as asked, 5 seconds by default.
        clip.source_in = Time::ZERO;
        clip.duration = duration.unwrap_or(DEFAULT_IMAGE_DURATION);
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
        AgentEdit::ClipAdd { track, source, start, source_in, duration, speed } => {
            let clip = added_clip(state, track, &source, start, source_in, duration, speed, probe)?;
            state.apply_agent_command(|project| engine::add_clip(project, track, clip)).map_err(refused)?;
            result.insert("clip".to_owned(), clip_json(clip_at(state.project(), track, start)));
        }
        AgentEdit::TextAdd { track, start, duration, content, style } => {
            if duration <= Time::ZERO {
                return Err(AgentError::new("invalid_params", "duration must be above 0"));
            }
            let patch = super::protocol::TextPatch { content: Some(content), ..style };
            let styled = styled_base(&state.text_style_library, &available_families(), patch.style_id.as_deref(), &default_text(String::new()), &engine::ClipAnimation::default())?;
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
            let styled = styled_base(&state.text_style_library, &available_families(), patch.style_id.as_deref(), old, &found.animation)?;
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
            state.apply_agent_command(|project| engine::set_animation(project, key, next)).map_err(refused)?;
            result.insert("clip".to_owned(), json!(clip));
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
            state.apply_agent_command(|project| engine::add_effect(project, clip.into(), Effect { package, params, keyframes: Default::default() })).map_err(refused)?
        }
        AgentEdit::EffectRemove { clip, effect } => state.apply_agent_command(|project| engine::remove_effect(project, clip.into(), effect)).map_err(refused)?,
        AgentEdit::TransitionAdd { track, after_clip, package, duration, params } => {
            let transition = Transition { after_clip, package, duration: duration.unwrap_or(DEFAULT_TRANSITION_DURATION), params };
            state.apply_agent_command(|project| engine::add_transition(project, track, transition)).map_err(refused)?;
        }
        AgentEdit::TransitionRemove { track, after_clip } => state.apply_agent_command(|project| engine::remove_transition(project, track, after_clip)).map_err(refused)?,
        AgentEdit::ClipSetSpeed { clip, speed } => {
            check_speed(speed)?;
            state.apply_agent_command(|project| engine::set_speed(project, clip.into(), speed)).map_err(refused)?;
        }
        AgentEdit::ClipSetGain { clip, gain_db } => state.apply_agent_command(|project| engine::set_gain(project, clip.into(), gain_db)).map_err(refused)?,
        AgentEdit::ClipSetMuted { clip, muted } => state.apply_agent_command(|project| engine::set_muted(project, clip.into(), muted)).map_err(refused)?,
        AgentEdit::ClipSetFades { clip, fade_in, fade_out } => state.apply_agent_command(|project| engine::set_fades(project, clip.into(), fade_in, fade_out)).map_err(refused)?,
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
    }
    Ok(Value::Object(result))
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

fn export_start<H: AgentHost>(host: &mut H, out: &Path, overwrite: bool, encoder: Option<String>) -> Result<Value, AgentError> {
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
    let encoder = match encoder.as_deref() {
        None | Some("auto") => EncoderChoice::Auto,
        Some(name) => EncoderChoice::Named(name.to_owned()),
    };
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
        AgentRequest::ProjectSave { path, overwrite } => project_save(host, path.as_deref(), overwrite),
        AgentRequest::ProjectGet => Ok(project_summary(host.state().project())),
        AgentRequest::PackagesList { .. } => Ok(package_list.unwrap_or_default()),
        AgentRequest::FontList => Ok(font_list()),
        AgentRequest::ClipAnimationList => Ok(animation_list(&host.state().clip_animation_library)),
        AgentRequest::TextStyleList { kind } => super::text::text_style_list(&host.state().text_style_library, kind.as_deref()),
        AgentRequest::SubtitleExport { track, path, overwrite } => subtitle_export(host, track, path.as_deref(), overwrite),
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
        AgentRequest::ExportStart { out, overwrite, encoder } => export_start(host, &out, overwrite, encoder),
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
    }

    impl FakeHost {
        fn new(mode: &'static str) -> Self {
            Self { state: EditorState::default(), mode, busy: false, running_out: None, started: Vec::new() }
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

        fn export_out(&self) -> Option<PathBuf> {
            self.running_out.clone()
        }
    }

    fn code(result: AgentResult) -> String {
        result.expect_err("must be refused").code
    }

    fn probed(length: Time) -> MediaProbe {
        MediaProbe { has_video: true, has_audio: false, video_duration: Some(length), audio_duration: None, video_problem: None, audio_problem: None }
    }

    fn added(source_in: Option<Time>, duration: Option<Time>, speed: Option<f64>) -> Result<Clip, AgentError> {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        added_clip(&state, 0, Path::new("a.mp4"), Time::ZERO, source_in, duration, speed, Some(Ok(probed(Time::from_ticks(9000)))))
    }

    #[test]
    fn a_clip_add_keeps_an_asset_source_and_refuses_an_escaping_or_missing_one() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Audio);
        let asset = Path::new("asset:sfx/p/audio/a.flac");
        let audio = MediaProbe { has_video: false, has_audio: true, video_duration: None, audio_duration: Some(Time::from_ticks(9000)), video_problem: None, audio_problem: None };
        let track = state.project().tracks.iter().rposition(|track| track.is_audio()).expect("an audio track was added");
        let clip = added_clip(&state, track, asset, Time::ZERO, None, None, None, Some(Ok(audio))).unwrap();
        assert_eq!(clip.source, "asset:sfx/p/audio/a.flac", "no absolute path in the project");
        let escaping = probe(Path::new("asset:sfx/../../secret.flac"), &[]).unwrap_err();
        assert_eq!(escaping.code, "source_not_found");
        let missing = probe(Path::new("asset:sfx/nothing/audio/a.flac"), &[]).unwrap_err();
        assert!(missing.message.contains("nothing") && missing.message.contains("not installed"), "{}", missing.message);
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
        let result = host.run(AgentRequest::ExportStart { out: PathBuf::from("out.mp4"), overwrite: false, encoder: None });
        assert_eq!(code(result), "busy");
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
