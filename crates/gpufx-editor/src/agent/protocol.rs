//! The requests an agent can make and the errors it can get, as JSON. The same types go over the
//! pipe to an open window.

use engine::{ClipPlacement, Easing, EditError, FontRef, KeyframeTarget, Rgba, TextAlign, AnimationRef, TextBackground, TextOutline, TextShadow, TextGlow, TextTransform, Time, TrackKind};
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
/// shadow, a glow or an animation.
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
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub glow: Option<Option<TextGlow>>,
    /// The whole fill, any kind of paint; not together with `color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<engine::TextPaint>,
    /// Every outline, replacing the text's (an empty list takes them off); not together with
    /// `outline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outlines: Option<Vec<TextOutline>>,
    /// Every shadow, replacing the text's; not together with `shadow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<Vec<TextShadow>>,
    /// Every inner shadow, replacing the text's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_shadows: Option<Vec<engine::TextInnerShadow>>,
    /// The bloom; `null` takes it off.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub bloom: Option<Option<engine::TextBloom>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<TextAlign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TextTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<TextAnimationPatch>,
    /// Changes the karaoke; `null` takes it off.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub karaoke: Option<Option<KaraokePatch>>,
    /// A style of an installed text or subtitle style pack, `"<pack id>/<item id>"`, applied
    /// before the other fields, which override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_id: Option<String>,
}

impl TextPatch {
    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == TextPatch::default()
    }
}

/// What to change of the karaoke of a text. A field that is absent stays; a look given is merged
/// into that state's look field by field, and `null` takes the state's look off. `words` gives one time per word of the text as shaped (an empty list
/// shares the cue by characters again).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KaraokePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<engine::KaraokeMode>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub active: Option<Option<engine::KaraokeLook>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub spoken: Option<Option<engine::KaraokeLook>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub upcoming: Option<Option<engine::KaraokeLook>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<Option<engine::KaraokeLook>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<engine::KaraokeWord>>,
}

/// What to change of the animation of a text, one group at a time, like [`TextPatch`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextAnimationPatch {
    #[serde(default, rename = "in", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub enter: Option<Option<AnimationRef>>,
    #[serde(default, rename = "out", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub exit: Option<Option<AnimationRef>>,
    #[serde(default, rename = "loop", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Option<AnimationRef>>,
}

/// A change of the timeline; each is one undo step, except locking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum AgentEdit {
    TrackAdd { kind: TrackKind },
    TrackSetLocked { track: usize, locked: bool },
    TrackSetHidden { track: usize, hidden: bool },
    /// Changes the output frame size and frame rate; `None` keeps that one. An odd side is
    /// rounded down to even, as encoders of 4:2:0 video need. The rate is an exact fraction, such
    /// as 30000/1001 for 29.97 fps.
    ProjectSetOutput { width: Option<u32>, height: Option<u32>, frame_rate: Option<engine::FrameRate> },
    /// `duration` defaults to the length of the stream the track plays.
    ClipAdd { track: usize, source: PathBuf, start: Time, source_in: Option<Time>, duration: Option<Time>, speed: Option<f64> },
    /// A text clip with `content` in the style of `style` on top of a new text. `track` of `None`
    /// takes the first video track with room at `start`, or a new one.
    TextAdd { track: Option<usize>, start: Time, duration: Time, content: String, style: TextPatch },
    /// Changes the text of a text clip; its place follows the size of the picture.
    TextSet { clip: ClipRef, patch: TextPatch },
    /// Changes the preset animations of a clip with a picture, one group at a time.
    ClipSetAnimation { clip: ClipRef, animation: TextAnimationPatch },
    ClipMove { clip: ClipRef, track: usize, start: Time },
    ClipTrimStart { clip: ClipRef, start: Time },
    ClipTrimEnd { clip: ClipRef, end: Time },
    ClipSplit { clip: ClipRef, at: Time },
    ClipDelete { clip: ClipRef },
    EffectAdd { clip: ClipRef, package: String, params: Map<String, Value> },
    EffectRemove { clip: ClipRef, effect: usize },
    /// Limits effect `effect` of `clip` (a blur or pixelate) to `region`, in parts of the clip's
    /// picture. With `track_id` the region moves with that motion track of the clip's own source,
    /// anchored at the source time the clip shows at timeline time `at` (its start when absent);
    /// `follow_scale`, `follow_rotation` and `perspective` say how.
    EffectRegionSet { clip: ClipRef, effect: usize, region: engine::EffectRegion, track_id: Option<u32>, at: Option<Time>, follow_scale: bool, follow_rotation: bool, perspective: bool },
    /// Gives effect `effect` of `clip` the whole picture again.
    EffectRegionClear { clip: ClipRef, effect: usize },
    TransitionAdd { track: usize, after_clip: usize, package: String, duration: Option<Time>, params: Map<String, Value> },
    TransitionRemove { track: usize, after_clip: usize },
    ClipSetGain { clip: ClipRef, gain_db: f64 },
    ClipSetSpeed { clip: ClipRef, speed: f64 },
    ClipSetMuted { clip: ClipRef, muted: bool },
    ClipSetReverse { clip: ClipRef, reverse: bool },
    ClipSetFades { clip: ClipRef, fade_in: Time, fade_out: Time },
    /// Sets the noise reduction of a clip; `None` keeps the clip's current value.
    ClipSetDenoise { clip: ClipRef, mode: Option<engine::DenoiseMode>, strength: Option<f64> },
    /// Sets the AI background removal of a clip; `None` keeps the clip's current value.
    ClipSetMatte { clip: ClipRef, model: Option<engine::MatteModel>, threshold: Option<f64>, feather: Option<f64>, edge: Option<f64>, invert: Option<bool>, temporal: Option<u32> },
    /// Sets the pitch shift and voice effect of a clip; `None` keeps the clip's current value.
    ClipSetVoice { clip: ClipRef, semitones: Option<f64>, effect: Option<engine::VoiceEffect> },
    /// Sets the static place, opacity and rotation of a clip; `None` leaves that one alone.
    ClipSetTransform { clip: ClipRef, placement: Option<ClipPlacement>, opacity: Option<f64>, rotation: Option<f64> },
    /// `at` is in ticks from the start of the clip. `easing` of `None` keeps the one replaced.
    KeyframeSet { clip: ClipRef, target: KeyframeTarget, at: Time, value: f64, easing: Option<Easing> },
    KeyframeRemove { clip: ClipRef, target: KeyframeTarget, at: Time },
    /// Adds an SRT file, read from `path` or given as `content` (exactly one), as a new subtitle
    /// track with one text clip per cue, in the subtitle style `style_id` when given.
    SubtitleImport { path: Option<PathBuf>, content: Option<String>, style_id: Option<String> },
    /// Gives subtitle track `track` the subtitle style `style_id`. A cue with a style of its own
    /// keeps it unless `all`.
    SubtitleSetStyle { track: usize, style_id: String, all: bool },
    /// Adds a marker to the ruler at `at` (timeline ticks); answers the index it got.
    MarkerAdd { at: Time, label: Option<String>, color: Option<Rgba> },
    MarkerRemove { marker: usize },
    /// Moves a marker; answers the index it now has.
    MarkerMove { marker: usize, at: Time },
    /// Takes the beats off a clip.
    ClipBeatsClear { clip: ClipRef },
    /// A censor region from `start` for `duration` on sticker track `track`, or on the first
    /// sticker track with room or a new one. `placement` absent: a third of the frame wide in the
    /// middle.
    CensorAdd { track: Option<usize>, start: Time, duration: Time, placement: Option<ClipPlacement>, censor: engine::CensorContent },
    /// Makes `clip` follow motion track `track_id` through `reference`, the clip that shows the
    /// tracked file, anchored at the source time `reference` shows at timeline time `at` (its start
    /// when absent): there the clip stands where it is placed.
    ClipFollowSet { clip: ClipRef, track_id: u32, reference: ClipRef, at: Option<Time>, follow_scale: bool, follow_rotation: bool, smooth: f64, perspective: bool },
    /// Stops `clip` following a motion track.
    ClipFollowClear { clip: ClipRef },
    /// Locks the frame of video clip `clip` to motion track `track_id` of its own source, anchored
    /// at the source time it shows at timeline time `at` (its start when absent): the object stays
    /// where it is at the anchor and the frame moves against it. `zoom` absent: 1.
    ClipLockSet { clip: ClipRef, track_id: u32, at: Option<Time>, lock_scale: bool, lock_rotation: bool, zoom: Option<f64>, smooth: f64, perspective: bool },
    /// Ends the lock of `clip`.
    ClipLockClear { clip: ClipRef },
    /// Adds or replaces a sample of motion track `track_id`, or starts a new track of the source
    /// of `clip` with it when `track_id` is absent. Answers the track id.
    TrackSetSample { track_id: Option<u32>, clip: Option<ClipRef>, sample: engine::TrackSample },
    /// Starts a new motion track of the source of `clip` with one sample placed by hand: the box
    /// `sample` (its `source_time` ignored) at the source time `clip` shows at timeline time `at`
    /// (its start when absent). Answers the track id.
    TrackCreate { clip: ClipRef, at: Option<Time>, sample: engine::TrackSample },
    /// Sets the smoothing of everything that follows motion track `track_id`, in seconds of source.
    TrackSmooth { track_id: u32, smooth: f64 },
    /// Turns the follow of `clip`, or its lock when it has one, into keyframes and drops it.

    TrackBakeKeyframes { clip: ClipRef },
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
    /// The effects and transitions matching `filter`; a list tool keeps the first `limit` items of
    /// each list it returns, every one when `None`.
    PackagesList { filter: Option<String>, limit: Option<usize> },
    /// The installed packs of every kind, or of `kind`, as the package manager lists them. Read only.
    InstalledPacks { kind: Option<String> },
    /// The font families a text can name, only those matching `filter` (see [`crate::search`]).
    FontList { filter: Option<String>, limit: Option<usize> },
    /// The preset animations a clip with a picture can name: id, group and default duration, only
    /// those matching `filter`.
    ClipAnimationList { filter: Option<String>, limit: Option<usize> },
    /// The styles of the installed text (`text`) or subtitle (`subtitle`) style packs; both when
    /// `kind` is `None`, only those matching `filter`.
    TextStyleList { kind: Option<String>, filter: Option<String>, limit: Option<usize> },
    /// The items of the installed music, sfx and sticker packs, each with the `asset:` source
    /// `clip_add` takes; only `kind` when given, and only items matching every word of `filter`.
    AssetList { kind: Option<String>, filter: Option<String>, limit: Option<usize> },
    /// The text clips of subtitle track `track` as an SRT file, also written to `path` when given.
    SubtitleExport { track: usize, path: Option<PathBuf>, overwrite: bool },
    /// The markers of the ruler and the beats every clip plays, in timeline time.
    MarkersList,
    /// Detects the beats of the sound of the source of `clip` and writes them to that source as one
    /// undo step. A session without a window waits for the result; an open window detects in the
    /// background and answers `running` with the progress until a later call finds the result
    /// ready. A failed detection is reported, not run again, unless `retry` is set.
    BeatsDetect {
        expected_revision: u64,
        clip: ClipRef,
        #[serde(default)]
        retry: bool,
    },
    /// Runs the object tracker on motion track `track_id` from its sample at source time `from`
    /// (by default the last sample placed by hand going forward, the first going backward),
    /// `forward` (default) or `backward`, up to the next sample placed by hand, the end of the file
    /// or a lost frame; `rotation` (default true) follows the object's turn. The samples are written
    /// as one undo step when it ends. `both` tracks forwards, then backwards from the same sample. A session without a window waits and answers the outcome; an
    /// open window answers the job at once, for [`AgentRequest::TrackStatus`].
    TrackRun {
        expected_revision: u64,
        track_id: u32,
        from: Option<Time>,
        direction: Option<String>,
        rotation: Option<bool>,
        /// `fast` (classical, the default) or `robust` (NanoTrack).
        mode: Option<String>,
    },
    /// Where tracker job `job` (the last one when absent) stands.
    TrackStatus { job: Option<u64> },
    /// Stops tracker job `job` (the last one when absent); it writes the samples made so far.
    TrackCancel { job: Option<u64> },
    /// Finds every face in video `clip` (the part of its file it shows, the preview proxy when
    /// there is one) with the face detector, joins them into face tracks, and adds a motion track
    /// and a censor region following it per face, as one undo step when the job ends. `shape`,
    /// `feather` and `mode` style the regions (ellipse, 0.3 and a blur by default). A session
    /// without a window waits and answers the outcome; an open window answers the job at once, for
    /// [`AgentRequest::CensorFacesStatus`].
    CensorFaces {
        expected_revision: u64,
        clip: ClipRef,
        shape: Option<engine::RegionShape>,
        feather: Option<f64>,
        mode: Option<engine::CensorMode>,
    },
    /// Where face censor job `job` (the last one when absent) stands.
    CensorFacesStatus { job: Option<u64> },
    /// Stops face censor job `job` (the last one when absent); it then adds nothing.
    CensorFacesCancel { job: Option<u64> },
    Edit { expected_revision: u64, edit: AgentEdit },
    Undo { expected_revision: u64 },
    Redo { expected_revision: u64 },
    FrameCapture { target: CaptureTarget, max_width: u32 },
    /// `codec` is h264 (default) or hevc; `encoder` is auto (default) or an encoder name.
    ExportStart { out: PathBuf, overwrite: bool, codec: Option<String>, encoder: Option<String> },
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
        EditError::SplitInsideHold { .. } => "split_inside_hold",
        EditError::TrackLocked { .. } => "track_locked",
        EditError::SpeedOverlap { .. } => "speed_overlap",
        EditError::SpeedOfImage { .. } => "speed_of_image",
        EditError::ReverseOfStill { .. } => "reverse_of_still",
        EditError::KeyframeOutsideClip { .. } => "keyframe_outside_clip",
        EditError::KeyframeNotFound { .. } => "keyframe_not_found",
        EditError::NoPlacement { .. } => "no_placement",
        EditError::NotText { .. } => "not_text",
        EditError::NotCensor { .. } => "not_censor",
        EditError::MotionTrackNotFound { .. } => "motion_track_not_found",
        EditError::TrackRunStale { .. } => "track_run_stale",
        EditError::NotFollowing { .. } => "not_following",
        EditError::BakePerspective { .. } => "bake_perspective",
        EditError::NotLocked { .. } => "not_locked",
        EditError::MarkerNotFound { .. } => "marker_not_found",
        EditError::Invalid(_) => "invalid_project",
    }
}
