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
    /// Whether every edit of this track is refused. See [`crate::EditError::TrackLocked`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Whether the track is left out of the picture and the sound. It still counts for the length
    /// of the project, so hiding a track does not change where anything ends.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
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
    /// Playback speed: a timeline tick reads `speed` source ticks, so `duration` is the length on
    /// the timeline and `duration * speed` is the source consumed. 1 when absent, and left out of
    /// the file when 1, so older projects load and save unchanged.
    #[serde(default = "default_speed", skip_serializing_if = "is_unit_speed")]
    pub speed: f64,
    /// Where the picture sits in the frame. Absent: as large as fits, centred, in its own shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<ClipPlacement>,
}

/// Where a clip sits in the output frame, in parts of the frame. The height follows from the shape
/// of the source, so the picture is never stretched and a new output size keeps these numbers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipPlacement {
    /// Left edge, in parts of the frame width.
    pub x: f64,
    /// Top edge, in parts of the frame height.
    pub y: f64,
    /// Width, in parts of the frame width.
    pub width: f64,
}

/// The narrowest a placed clip may be, in parts of the frame width.
pub const MIN_PLACEMENT_WIDTH: f64 = 0.01;
/// The widest a placed clip may be, in parts of the frame width.
pub const MAX_PLACEMENT_WIDTH: f64 = 8.0;
/// How far from the frame origin the edges of a placed clip may be, in parts of the frame.
pub const MAX_PLACEMENT_OFFSET: f64 = 8.0;

impl ClipPlacement {
    /// Whether every number is finite and within its limits.
    pub fn is_valid(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.x.abs() <= MAX_PLACEMENT_OFFSET && self.y.abs() <= MAX_PLACEMENT_OFFSET && (MIN_PLACEMENT_WIDTH..=MAX_PLACEMENT_WIDTH).contains(&self.width)
    }
}

/// The rectangle of a picture in the output frame, in parts of the frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlacedRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PlacedRect {
    /// Where a clip lands in a frame of `output_aspect`, for a source of `source_aspect`, both
    /// width over height.
    pub fn of(placement: Option<ClipPlacement>, output_aspect: f64, source_aspect: f64) -> PlacedRect {
        let relative = output_aspect / source_aspect;
        match placement {
            Some(placement) => PlacedRect { x: placement.x, y: placement.y, width: placement.width, height: placement.width * relative },
            None if relative <= 1.0 => PlacedRect { x: 0.0, y: (1.0 - relative) / 2.0, width: 1.0, height: relative },
            None => {
                let width = 1.0 / relative;
                PlacedRect { x: (1.0 - width) / 2.0, y: 0.0, width, height: 1.0 }
            }
        }
    }

    /// Whether the picture lies on the whole frame of `width` x `height` pixels, to a pixel: a
    /// source of the same shape as an output whose size is rounded (854x480 for 16:9) is off by
    /// less than that, and moving it would only resample the picture for nothing.
    pub fn fills_frame(self, width: u32, height: u32) -> bool {
        let (columns, rows) = (f64::from(width), f64::from(height));
        (self.x * columns).abs() < 1.0 && (self.y * rows).abs() < 1.0 && ((self.width - 1.0) * columns).abs() < 1.0 && ((self.height - 1.0) * rows).abs() < 1.0
    }
}

/// The slowest playback speed of a clip.
pub const MIN_SPEED: f64 = 0.1;
/// The fastest playback speed of a clip.
pub const MAX_SPEED: f64 = 10.0;
/// The shortest output frame in ticks: 60 fps is the highest output rate.
pub const MIN_FRAME_DURATION: Time = Time::from_ticks(1500);
/// A speed is used as a whole number of these parts: the mapping is exact in integers and never
/// accumulates rounding, however long the clip. See [`Clip::source_offset`].
pub const SPEED_SCALE: i64 = 1000;

fn default_speed() -> f64 {
    1.0
}

fn is_unit_speed(speed: &f64) -> bool {
    *speed == 1.0
}

/// Whether a clip may use this `speed`.
pub fn speed_in_range(speed: f64) -> bool {
    speed.is_finite() && (MIN_SPEED..=MAX_SPEED).contains(&speed)
}

/// `speed` rounded to the nearest whole part of [`SPEED_SCALE`], limited to the allowed range.
pub fn speed_milli(speed: f64) -> i64 {
    if speed.is_finite() {
        ((speed * SPEED_SCALE as f64).round() as i64).clamp(100, 10_000)
    } else {
        SPEED_SCALE
    }
}

/// Greatest common divisor of two positive numbers.
fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `value * numerator / denominator` rounded toward negative infinity, limited to the `Time` range.
fn scale_floor(value: i64, numerator: i64, denominator: i64) -> i64 {
    let product = i128::from(value) * i128::from(numerator);
    i64::try_from(product.div_euclid(i128::from(denominator))).unwrap_or(if product < 0 { i64::MIN } else { i64::MAX })
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
                            speed: 1.0,
                            placement: None,
                        })
                        .collect(),
                    transitions: track.transitions,
                    locked: false,
                    hidden: false,
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

    /// The speed as a whole number of [`SPEED_SCALE`] parts.
    pub fn speed_milli(&self) -> i64 {
        speed_milli(self.speed)
    }

    /// Whether the clip plays at anything but its original speed.
    pub fn has_speed(&self) -> bool {
        self.speed_milli() != SPEED_SCALE
    }

    /// The source ticks played in `offset` timeline ticks from the start of the clip:
    /// `floor(offset * speed)`, exact in integers, so picture and sound never drift. `offset` may be
    /// negative, inside a transition window.
    pub fn source_offset(&self, offset: Time) -> Time {
        Time::from_ticks(scale_floor(offset.ticks(), self.speed_milli(), SPEED_SCALE))
    }

    /// The first source position past what the clip plays.
    pub fn source_end(&self) -> Time {
        self.source_in.saturating_add(self.source_offset(self.duration))
    }

    /// Where a start trim asked for `new_start` lands: on the grid of [`Clip::speed_grid`], toward
    /// the end of the clip, so the frames under the end edge stay exactly where they are.
    pub fn snapped_start(&self, new_start: Time) -> Time {
        let grid = self.speed_grid();
        let delta = new_start.saturating_sub(self.start).ticks();
        let steps = delta.div_euclid(grid).saturating_add(i64::from(delta.rem_euclid(grid) != 0));
        self.start.saturating_add(Time::from_ticks(steps.saturating_mul(grid)))
    }

    /// The timeline ticks that play `source` source ticks, rounded down.
    pub fn timeline_ticks(&self, source: Time) -> Time {
        Time::from_ticks(scale_floor(source.ticks(), SPEED_SCALE, self.speed_milli()))
    }

    /// The step, in timeline ticks, of the edit positions whose source position is a whole tick,
    /// so that an edit at a multiple of it keeps every source position exactly. 1 at normal speed
    /// and at most [`SPEED_SCALE`] ticks (11 ms) otherwise.
    pub fn speed_grid(&self) -> i64 {
        SPEED_SCALE / gcd(self.speed_milli(), SPEED_SCALE)
    }
}

impl Track {
    /// Whether this track only contributes sound.
    pub fn is_audio(&self) -> bool {
        self.kind == TrackKind::Audio
    }
}
