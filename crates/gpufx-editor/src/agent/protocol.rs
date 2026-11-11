//! The requests an agent can make and the errors it can get, as JSON. The same types go over the
//! pipe to an open window.

use engine::{ClipPlacement, Easing, EditError, FontRef, KeyframeTarget, Rgba, TextAlign, TextAnimationRef, TextBackground, TextOutline, TextShadow, Time, TrackKind};
use serde::{Deserialize, Deserializer, Serialize};
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

/// Reads a field where absent, `null` and a value mean different things: absent is `None`, `null`
/// is `Some(None)` and a value is `Some(Some(value))`.
pub fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// What to change of a text. A field that is absent stays; `null` takes off an outline, a box, a
/// shadow or an animation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<FontRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgba>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub outline: Option<Option<TextOutline>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub background: Option<Option<TextBackground>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Option<TextShadow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<TextAlign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<TextAnimationPatch>,
}

impl TextPatch {
    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == TextPatch::default()
    }
}

/// What to change of the animation of a text, one group at a time, like [`TextPatch`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextAnimationPatch {
    #[serde(default, rename = "in", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub enter: Option<Option<TextAnimationRef>>,
    #[serde(default, rename = "out", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub exit: Option<Option<TextAnimationRef>>,
    #[serde(default, rename = "loop", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Option<TextAnimationRef>>,
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
    /// A text clip with `content` in the style of `style` on top of a new text. `track` of `None`
    /// takes the first video track with room at `start`, or a new one.
    TextAdd { track: Option<usize>, start: Time, duration: Time, content: String, style: TextPatch },
    /// Changes the text of a text clip; its place follows the size of the picture.
    TextSet { clip: ClipRef, patch: TextPatch },
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
    /// The font families a text can name.
    FontList,
    /// The ids of the preset animations of a text, by group.
    TextAnimationList,
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
        EditError::NotText { .. } => "not_text",
        EditError::Invalid(_) => "invalid_project",
    }
}
