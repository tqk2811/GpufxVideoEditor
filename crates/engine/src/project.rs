//! Project model, matching the project JSON of the phase 1 and phase 2 specifications.

use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// The newest project format version this engine understands.
pub const SUPPORTED_VERSION: u32 = 2;

/// The oldest project format version this engine still loads.
pub const MIN_SUPPORTED_VERSION: u32 = 1;

/// A whole project: output settings and tracks (bottom track first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Format version, from [`MIN_SUPPORTED_VERSION`] to [`SUPPORTED_VERSION`].
    pub version: u32,
    /// Output frame size and cadence.
    pub output: Output,
    /// Tracks in compositing order, index 0 at the bottom.
    pub tracks: Vec<Track>,
}

/// Output settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Duration of one output frame in ticks (3003 for 30000/1001 fps).
    pub frame_duration: Time,
    /// Audio stream settings. Without them the output has no audio stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioOutput>,
}

/// Settings of the output audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOutput {
    /// Samples per second; must be 48000.
    pub sample_rate: u32,
    /// Channel count; must be 2.
    pub channels: u32,
    /// Encoder bit rate in bits per second.
    pub bitrate: u32,
}

/// What a track holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    /// Clips with pictures, which also carry their own sound.
    #[default]
    Video,
    /// Clips that only contribute sound. The video scheduler ignores these tracks.
    Audio,
}

/// One track: clips plus the transitions between neighbouring clips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// What the track holds; `video` when absent.
    #[serde(default, skip_serializing_if = "is_video")]
    pub kind: TrackKind,
    /// Clips, which must not overlap.
    pub clips: Vec<Clip>,
    /// Transitions between clip `after_clip` and clip `after_clip + 1`.
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

/// A piece of a source file placed on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Source media path, resolved by the caller.
    pub source: String,
    /// Timeline position of the first frame.
    pub start: Time,
    /// Position inside the source that plays at `start`.
    pub source_in: Time,
    /// Length on the timeline.
    pub duration: Time,
    /// Per-clip effects with static parameters.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Audio gain in decibels; 0 leaves the level unchanged.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gain_db: f64,
    /// Whether the clip's sound is left out of the mix.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Length of the audio fade from the start of the clip.
    #[serde(default, skip_serializing_if = "is_zero_time")]
    pub fade_in: Time,
    /// Length of the audio fade to the end of the clip.
    #[serde(default, skip_serializing_if = "is_zero_time")]
    pub fade_out: Time,
}

fn is_video(kind: &TrackKind) -> bool {
    *kind == TrackKind::Video
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_time(value: &Time) -> bool {
    *value == Time::ZERO
}

/// An effect package applied with static parameters.
///
/// The specification does not fix the shape of an effect entry; this model uses
/// the same `package` and `params` fields as a transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    /// Effect package name.
    pub package: String,
    /// Static parameters, passed through to the package.
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// A transition between clip `after_clip` and the next clip of the same track.
///
/// The transition window is centred on the cut: it starts `duration / 2`
/// (rounded down) before the second clip's `start` and lasts `duration` ticks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Index of the first of the two clips.
    pub after_clip: usize,
    /// Transition package name.
    pub package: String,
    /// Length of the transition window in ticks.
    pub duration: Time,
    /// Static parameters, passed through to the package.
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// Failure to load a project from JSON.
#[derive(Debug)]
pub enum LoadError {
    /// The text is not valid project JSON.
    Json(serde_json::Error),
    /// The project declares a version this engine does not support.
    UnsupportedVersion(u32),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "invalid project JSON: {e}"),
            LoadError::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported project version {v} (supported: {MIN_SUPPORTED_VERSION} to {SUPPORTED_VERSION})"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// The version 1 format, frozen. Unknown fields are ignored, so a version 1 file that happens to
/// contain fields named like the version 2 ones still loads exactly as it always did.
#[derive(Deserialize)]
struct ProjectV1 {
    version: u32,
    output: OutputV1,
    tracks: Vec<TrackV1>,
}

#[derive(Deserialize)]
struct OutputV1 {
    width: u32,
    height: u32,
    frame_duration: Time,
}

#[derive(Deserialize)]
struct TrackV1 {
    clips: Vec<ClipV1>,
    #[serde(default)]
    transitions: Vec<Transition>,
}

#[derive(Deserialize)]
struct ClipV1 {
    source: String,
    start: Time,
    source_in: Time,
    duration: Time,
    #[serde(default)]
    effects: Vec<Effect>,
}

impl From<ProjectV1> for Project {
    fn from(project: ProjectV1) -> Project {
        Project {
            version: SUPPORTED_VERSION,
            output: Output {
                width: project.output.width,
                height: project.output.height,
                frame_duration: project.output.frame_duration,
                audio: None,
            },
            tracks: project
                .tracks
                .into_iter()
                .map(|track| Track {
                    kind: TrackKind::Video,
                    clips: track
                        .clips
                        .into_iter()
                        .map(|clip| Clip {
                            source: clip.source,
                            start: clip.start,
                            source_in: clip.source_in,
                            duration: clip.duration,
                            effects: clip.effects,
                            gain_db: 0.0,
                            muted: false,
                            fade_in: Time::ZERO,
                            fade_out: Time::ZERO,
                        })
                        .collect(),
                    transitions: track.transitions,
                })
                .collect(),
        }
    }
}

impl Project {
    /// Parses a project and rejects unknown versions. Does not run [`crate::validate`].
    pub fn from_json(text: &str) -> Result<Project, LoadError> {
        // Every version has at least the version 1 fields, so this also finds the version.
        let v1: ProjectV1 = serde_json::from_str(text).map_err(LoadError::Json)?;
        match v1.version {
            1 => Ok(v1.into()),
            SUPPORTED_VERSION => serde_json::from_str(text).map_err(LoadError::Json),
            other => Err(LoadError::UnsupportedVersion(other)),
        }
    }
}

impl Clip {
    /// Timeline end (exclusive), or `None` on overflow.
    pub fn end(&self) -> Option<Time> {
        self.start.checked_add(self.duration)
    }
}

impl Track {
    /// Whether this track only contributes sound.
    pub fn is_audio(&self) -> bool {
        self.kind == TrackKind::Audio
    }
}
