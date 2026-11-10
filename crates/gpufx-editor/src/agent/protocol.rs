//! The requests an agent can make and the errors it can get, as JSON. The same types go over the
//! pipe to an open window.

use engine::{ClipPlacement, Easing, EditError, KeyframeTarget, Time, TrackKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// One clip of the project, by its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipRef {
    pub track: usize,
    pub clip: usize,
}

impl From<ClipRef> for engine::ClipKey {
    fn from(clip: ClipRef) -> Self {
        Self { track: clip.track, clip: clip.clip }
    }
}

impl From<engine::ClipKey> for ClipRef {
    fn from(key: engine::ClipKey) -> Self {
        Self { track: key.track, clip: key.clip }
    }
}

/// A change of the timeline; each is one undo step, except locking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum AgentEdit {
    TrackAdd { kind: TrackKind },
    TrackSetLocked { track: usize, locked: bool },
    TrackSetHidden { track: usize, hidden: bool },
    /// `duration` defaults to the length of the stream the track plays.
    ClipAdd { track: usize, source: PathBuf, start: Time, source_in: Option<Time>, duration: Option<Time>, speed: Option<f64> },
    ClipMove { clip: ClipRef, track: usize, start: Time },
    ClipTrimStart { clip: ClipRef, start: Time },
    ClipTrimEnd { clip: ClipRef, end: Time },
    ClipSplit { clip: ClipRef, at: Time },
    ClipDelete { clip: ClipRef },
    EffectAdd { clip: ClipRef, package: String, params: Map<String, Value> },
    EffectRemove { clip: ClipRef, effect: usize },
    TransitionAdd { track: usize, after_clip: usize, package: String, duration: Option<Time>, params: Map<String, Value> },
    TransitionRemove { track: usize, after_clip: usize },
    ClipSetGain { clip: ClipRef, gain_db: f64 },
    ClipSetSpeed { clip: ClipRef, speed: f64 },
    ClipSetMuted { clip: ClipRef, muted: bool },
    ClipSetFades { clip: ClipRef, fade_in: Time, fade_out: Time },
    /// Sets the static place, opacity and rotation of a clip; `None` leaves that one alone.
    ClipSetTransform { clip: ClipRef, placement: Option<ClipPlacement>, opacity: Option<f64>, rotation: Option<f64> },
    /// `at` is in ticks from the start of the clip. `easing` of `None` keeps the one replaced.
    KeyframeSet { clip: ClipRef, target: KeyframeTarget, at: Time, value: f64, easing: Option<Easing> },
    KeyframeRemove { clip: ClipRef, target: KeyframeTarget, at: Time },
}

/// Which output frame to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTarget {
    Time(Time),
    Frame(i64),
}

/// One thing asked of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum AgentRequest {
    SessionStatus,
    ProjectNew { discard_changes: bool },
    ProjectOpen { path: PathBuf, discard_changes: bool },
    ProjectSave { path: Option<PathBuf>, overwrite: bool },
    ProjectGet,
    PackagesList { filter: Option<String> },
    Edit { expected_revision: u64, edit: AgentEdit },
    Undo { expected_revision: u64 },
    Redo { expected_revision: u64 },
    FrameCapture { target: CaptureTarget, max_width: u32 },
    ExportStart { out: PathBuf, overwrite: bool, encoder: Option<String> },
    ExportStatus { job: Option<u64> },
    ExportCancel { job: Option<u64> },
}

/// A refused or failed request. `code` is stable, `message` is for reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

/// What a request gives: a JSON object carrying `revision`, or an error.
pub type AgentResult = Result<Value, AgentError>;

impl AgentError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into(), data: Value::Null }
    }

    pub fn stale(current: u64, expected: u64) -> Self {
        Self { code: "stale_revision".to_owned(), message: format!("the project is at revision {current}, not {expected}: read it again"), data: json!({"revision": current}) }
    }

    pub fn from_edit(error: &EditError) -> Self {
        Self::new(edit_error_code(error), error.to_string())
    }
}

/// The stable code of a refusal. No `_` arm: a new reason must be given one.
pub fn edit_error_code(error: &EditError) -> &'static str {
    match error {
        EditError::TrackNotFound { .. } => "track_not_found",
        EditError::ClipNotFound { .. } => "clip_not_found",
        EditError::EffectNotFound { .. } => "effect_not_found",
        EditError::TransitionNotFound { .. } => "transition_not_found",
        EditError::TrackKindMismatch { .. } => "track_kind_mismatch",
        EditError::NoRoom { .. } => "no_room",
        EditError::Overlap { .. } => "overlap",
        EditError::TimeOverflow { .. } => "time_overflow",
        EditError::TransitionOnEdge { .. } => "transition_on_edge",
        EditError::BeforeSourceStart { .. } => "before_source_start",
        EditError::SplitOutsideClip { .. } => "split_outside_clip",
        EditError::SplitInsideFade { .. } => "split_inside_fade",
        EditError::SplitInsideTransition { .. } => "split_inside_transition",
        EditError::TrackLocked { .. } => "track_locked",
        EditError::SpeedOverlap { .. } => "speed_overlap",
        EditError::SpeedOfImage { .. } => "speed_of_image",
        EditError::KeyframeOutsideClip { .. } => "keyframe_outside_clip",
        EditError::KeyframeNotFound { .. } => "keyframe_not_found",
        EditError::NoPlacement { .. } => "no_placement",
        EditError::Invalid(_) => "invalid_project",
    }
}
