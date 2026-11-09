//! Project model, matching the project JSON of the phase 1 specification.

use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// The only project format version this engine understands.
pub const SUPPORTED_VERSION: u32 = 1;

/// A whole project: output settings and tracks (bottom track first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Format version; must equal [`SUPPORTED_VERSION`].
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
}

/// One track: clips plus the transitions between neighbouring clips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
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
                    "unsupported project version {v} (supported: {SUPPORTED_VERSION})"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

impl Project {
    /// Parses a project and rejects unknown versions. Does not run [`crate::validate`].
    pub fn from_json(text: &str) -> Result<Project, LoadError> {
        let project: Project = serde_json::from_str(text).map_err(LoadError::Json)?;
        if project.version != SUPPORTED_VERSION {
            return Err(LoadError::UnsupportedVersion(project.version));
        }
        Ok(project)
    }
}

impl Clip {
    /// Timeline end (exclusive), or `None` on overflow.
    pub fn end(&self) -> Option<Time> {
        self.start.checked_add(self.duration)
    }
}
