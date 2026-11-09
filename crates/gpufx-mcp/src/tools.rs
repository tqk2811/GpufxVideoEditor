//! The tool table: name, description, parameter schema, and how the parameters become a request.

use std::path::PathBuf;
use std::sync::Arc;

use engine::Time;
use gpufx_editor::agent::protocol::ClipRef;
use gpufx_editor::agent::{AgentEdit, AgentError, AgentRequest};
use rmcp::model::{JsonObject, Tool};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{Map, Value};

/// A kind of track, as the tools name it.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum TrackKindParam {
    Video,
    Audio,
}

impl From<TrackKindParam> for engine::TrackKind {
    fn from(kind: TrackKindParam) -> Self {
        match kind {
            TrackKindParam::Video => Self::Video,
            TrackKindParam::Audio => Self::Audio,
        }
    }
}

fn ticks(value: i64) -> Time {
    Time::from_ticks(value)
}

/// `{track, clip}` of an existing clip.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
pub struct ClipParam {
    pub track: usize,
    pub clip: usize,
}

impl From<ClipParam> for ClipRef {
    fn from(clip: ClipParam) -> Self {
        ClipRef { track: clip.track, clip: clip.clip }
    }
}

macro_rules! params {
    ($name:ident { $($(#[$meta:meta])* $field:ident : $kind:ty),* $(,)? }) => {
        #[derive(Deserialize, JsonSchema)]
        pub struct $name { $($(#[$meta])* pub $field: $kind,)* }
    };
}

params!(NoParams {});
params!(ProjectNewParams { #[serde(default)] discard_changes: bool });
params!(ProjectOpenParams { path: PathBuf, #[serde(default)] discard_changes: bool });
params!(ProjectSaveParams { path: Option<PathBuf> });
params!(PackagesListParams { filter: Option<String> });
params!(TrackAddParams { expected_revision: u64, kind: TrackKindParam });
params!(TrackLockedParams { expected_revision: u64, track: usize, locked: bool });
params!(TrackHiddenParams { expected_revision: u64, track: usize, hidden: bool });
params!(ClipAddParams { expected_revision: u64, track: usize, source: PathBuf, start: i64, source_in: Option<i64>, duration: Option<i64> });
params!(ClipMoveParams { expected_revision: u64, clip: ClipParam, track: usize, start: i64 });
params!(ClipTrimStartParams { expected_revision: u64, clip: ClipParam, start: i64 });
params!(ClipTrimEndParams { expected_revision: u64, clip: ClipParam, end: i64 });
params!(ClipSplitParams { expected_revision: u64, clip: ClipParam, at: i64 });
params!(ClipOnlyParams { expected_revision: u64, clip: ClipParam });
params!(EffectAddParams { expected_revision: u64, clip: ClipParam, package: String, #[serde(default)] params: Map<String, Value> });
params!(EffectRemoveParams { expected_revision: u64, clip: ClipParam, effect: usize });
params!(TransitionAddParams { expected_revision: u64, track: usize, after_clip: usize, package: String, duration: Option<i64>, #[serde(default)] params: Map<String, Value> });
params!(TransitionRemoveParams { expected_revision: u64, track: usize, after_clip: usize });
params!(GainParams { expected_revision: u64, clip: ClipParam, gain_db: f64 });
params!(MutedParams { expected_revision: u64, clip: ClipParam, muted: bool });
params!(FadesParams { expected_revision: u64, clip: ClipParam, fade_in: i64, fade_out: i64 });
params!(RevisionParams { expected_revision: u64 });

/// One tool of the server.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    schema: fn() -> JsonObject,
    build: fn(Value) -> Result<AgentRequest, AgentError>,
}

impl ToolDef {
    /// The request of a call with these arguments, or `invalid_params`.
    pub fn request(&self, arguments: Value) -> Result<AgentRequest, AgentError> {
        (self.build)(arguments)
    }

    pub fn tool(&self) -> Tool {
        Tool::new(self.name, self.description, Arc::new((self.schema)()))
    }
}

fn schema_of<P: JsonSchema>() -> JsonObject {
    match serde_json::to_value(schemars::schema_for!(P)) {
        Ok(Value::Object(schema)) => schema,
        _ => JsonObject::new(),
    }
}

fn parsed<P: DeserializeOwned>(arguments: Value) -> Result<P, AgentError> {
    serde_json::from_value(arguments).map_err(|e| AgentError::new("invalid_params", e.to_string()))
}

macro_rules! tool {
    ($name:literal, $description:literal, $params:ty, $build:expr) => {
        ToolDef {
            name: $name,
            description: $description,
            schema: schema_of::<$params>,
            build: |arguments| {
                let params: $params = parsed(arguments)?;
                let build: fn($params) -> Result<AgentRequest, AgentError> = $build;
                build(params)
            },
        }
    };
}

fn edit(expected_revision: u64, edit: AgentEdit) -> Result<AgentRequest, AgentError> {
    Ok(AgentRequest::Edit { expected_revision, edit })
}

/// Every tool. Times are integer ticks of 1/90000 s.
pub fn all_tools() -> Vec<ToolDef> {
    vec![
        tool!("session_status", "The mode, project file, revision, and whether an export or a drag is going on.", NoParams, |_| Ok(AgentRequest::SessionStatus)),
        tool!("project_new", "Replaces the project with an empty one. Refused with unsaved_changes unless discard_changes is true.", ProjectNewParams, |p| Ok(AgentRequest::ProjectNew { discard_changes: p.discard_changes })),
        tool!("project_open", "Opens a project file. Refused with unsaved_changes unless discard_changes is true.", ProjectOpenParams, |p| Ok(AgentRequest::ProjectOpen { path: p.path, discard_changes: p.discard_changes })),
        tool!("project_save", "Saves the project, to path or to its own file.", ProjectSaveParams, |p| Ok(AgentRequest::ProjectSave { path: p.path })),
        tool!("project_get", "The project JSON, its duration and frame count. Clips are named by {track, clip} indexes, which change after an edit.", NoParams, |_| Ok(AgentRequest::ProjectGet)),
        tool!("packages_list", "Effect and transition packages, filtered by a text.", PackagesListParams, |p| Ok(AgentRequest::PackagesList { filter: p.filter })),
        tool!("track_add", "Adds an empty track on top; returns its index.", TrackAddParams, |p| edit(p.expected_revision, AgentEdit::TrackAdd { kind: p.kind.into() })),
        tool!("track_set_locked", "Locks or unlocks a track. Edits of a locked track fail with track_locked.", TrackLockedParams, |p| edit(p.expected_revision, AgentEdit::TrackSetLocked { track: p.track, locked: p.locked })),
        tool!("track_set_hidden", "Hides or shows a track in picture and sound.", TrackHiddenParams, |p| edit(p.expected_revision, AgentEdit::TrackSetHidden { track: p.track, hidden: p.hidden })),
        tool!("clip_add", "Adds a source file as a clip at start (ticks). duration defaults to the length of the stream the track plays. Returns the new clip.", ClipAddParams, |p| edit(p.expected_revision, AgentEdit::ClipAdd { track: p.track, source: p.source, start: ticks(p.start), source_in: p.source_in.map(ticks), duration: p.duration.map(ticks) })),
        tool!("clip_move", "Moves a clip to a track, as near start as the other clips allow. Returns where it landed.", ClipMoveParams, |p| edit(p.expected_revision, AgentEdit::ClipMove { clip: p.clip.into(), track: p.track, start: ticks(p.start) })),
        tool!("clip_trim_start", "Moves the start edge of a clip, keeping the frames under the end edge.", ClipTrimStartParams, |p| edit(p.expected_revision, AgentEdit::ClipTrimStart { clip: p.clip.into(), start: ticks(p.start) })),
        tool!("clip_trim_end", "Moves the end edge of a clip.", ClipTrimEndParams, |p| edit(p.expected_revision, AgentEdit::ClipTrimEnd { clip: p.clip.into(), end: ticks(p.end) })),
        tool!("clip_split", "Splits a clip at a time; returns the second half.", ClipSplitParams, |p| edit(p.expected_revision, AgentEdit::ClipSplit { clip: p.clip.into(), at: ticks(p.at) })),
        tool!("clip_delete", "Deletes a clip and the transitions touching it.", ClipOnlyParams, |p| edit(p.expected_revision, AgentEdit::ClipDelete { clip: p.clip.into() })),
        tool!("effect_add", "Appends an effect package to a clip.", EffectAddParams, |p| edit(p.expected_revision, AgentEdit::EffectAdd { clip: p.clip.into(), package: p.package, params: p.params })),
        tool!("effect_remove", "Removes an effect of a clip by index.", EffectRemoveParams, |p| edit(p.expected_revision, AgentEdit::EffectRemove { clip: p.clip.into(), effect: p.effect })),
        tool!("transition_add", "Adds a transition between clip after_clip and the next, which must touch.", TransitionAddParams, |p| edit(p.expected_revision, AgentEdit::TransitionAdd { track: p.track, after_clip: p.after_clip, package: p.package, duration: p.duration.map(ticks), params: p.params })),
        tool!("transition_remove", "Removes the transition after a clip.", TransitionRemoveParams, |p| edit(p.expected_revision, AgentEdit::TransitionRemove { track: p.track, after_clip: p.after_clip })),
        tool!("clip_set_gain", "Sets the gain of a clip in decibels.", GainParams, |p| edit(p.expected_revision, AgentEdit::ClipSetGain { clip: p.clip.into(), gain_db: p.gain_db })),
        tool!("clip_set_muted", "Mutes or unmutes a clip.", MutedParams, |p| edit(p.expected_revision, AgentEdit::ClipSetMuted { clip: p.clip.into(), muted: p.muted })),
        tool!("clip_set_fades", "Sets the audio fades of a clip, in ticks.", FadesParams, |p| edit(p.expected_revision, AgentEdit::ClipSetFades { clip: p.clip.into(), fade_in: ticks(p.fade_in), fade_out: ticks(p.fade_out) })),
        tool!("undo", "Undoes the last edit.", RevisionParams, |p| Ok(AgentRequest::Undo { expected_revision: p.expected_revision })),
        tool!("redo", "Redoes an undone edit.", RevisionParams, |p| Ok(AgentRequest::Redo { expected_revision: p.expected_revision })),
    ]
}
