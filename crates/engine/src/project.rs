//! Project model, matching the project JSON of the phase 1 and phase 2 specifications.

use crate::keyframe::{ClipMotion, Keyframe};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
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
    /// Still images and stickers, drawn like the clips of a video track.
    Sticker,
    /// Text clips only, drawn like the clips of a video track. Subtitle tracks are of this kind.
    Text,
}

impl TrackKind {
    /// Every kind, in the order the editor offers them.
    pub const ALL: [TrackKind; 4] = [TrackKind::Video, TrackKind::Audio, TrackKind::Sticker, TrackKind::Text];

    /// Whether clips of this kind are drawn into the picture: every kind but audio.
    pub fn shows_picture(self) -> bool {
        self != TrackKind::Audio
    }

    /// Whether a clip of `source` may go on a track of this kind, judged from the source alone;
    /// [`TEXT_SOURCE`] stands for a text clip. The one rule every caller shares:
    ///
    /// - video: video files and images, not stickers and not text;
    /// - audio: media files that are not images and not text (whether the file really has sound
    ///   is up to the resolver, see [`crate::SourceResolver::source_has_audio`]);
    /// - sticker: images and stickers;
    /// - text: text clips only.
    pub fn accepts_source(self, source: &str) -> bool {
        let text = source == TEXT_SOURCE;
        match self {
            TrackKind::Video => !text && !is_sticker_source(source),
            TrackKind::Audio => !text && !is_image_source(source) && !is_sticker_source(source),
            TrackKind::Sticker => !text && (is_image_source(source) || is_sticker_source(source)),
            TrackKind::Text => text,
        }
    }

    /// Whether `clip` may go on a track of this kind; see [`TrackKind::accepts_source`].
    pub fn accepts_clip(self, clip: &Clip) -> bool {
        if clip.is_text() {
            self == TrackKind::Text
        } else {
            self.accepts_source(&clip.source)
        }
    }
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
    /// Present on a subtitle track: a text track made by importing subtitles, with one text clip
    /// per cue. Absent on every other track, so older projects load and save unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<SubtitleTrack>,
}

/// What makes a track a subtitle track.
///
/// The style of its clips is copied into each clip's [`TextContent`], as any text style is, so the
/// project opens the same without the style's pack. `style` only remembers which style the track
/// was last given, for the editor to show and to give new cues.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubtitleTrack {
    /// `"<pack id>/<item id>"` of the subtitle style the track was last given; `None` when it has
    /// none yet. Must not be empty or blank when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
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
    /// Opacity, rotation and keyframes. Left out of the file when it is all default.
    #[serde(default, skip_serializing_if = "ClipMotion::is_default")]
    pub motion: ClipMotion,
    /// What a text clip shows and how. Present only on text clips, whose `source` is
    /// [`TEXT_SOURCE`]; absent on media clips, so older projects load and save unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextContent>,
    /// Preset animations of the picture of the clip (video, image, sticker or text). Left out of
    /// the file when there are none; ignored on an audio track.
    #[serde(default, skip_serializing_if = "ClipAnimation::is_empty")]
    pub animation: ClipAnimation,
}

/// The `source` of every text clip. It names no file and has no extension, so
/// [`is_image_source`] never mistakes it for an image.
pub const TEXT_SOURCE: &str = "text:";

/// The content and style of a text clip. It is rasterized into a still picture, so placement,
/// motion, keyframes and opacity work as on an image clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextContent {
    /// The text, with a line feed between lines. Must not be empty.
    pub content: String,
    /// Which font draws it.
    #[serde(default)]
    pub font: FontRef,
    /// Font size, in parts of the output height. Must be above 0.
    pub size: f32,
    /// Fill colour.
    #[serde(default = "white")]
    pub color: Rgba,
    /// Outline around each glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<TextOutline>,
    /// Box drawn behind the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<TextBackground>,
    /// Drop shadow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<TextShadow>,
    /// Glow around the glyphs, under the shadow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<TextGlow>,
    /// Horizontal alignment of the lines.
    #[serde(default)]
    pub align: TextAlign,
    /// Line height as a multiple of the font size.
    #[serde(default = "unit_f32")]
    pub line_spacing: f32,
    /// Font weight, 100 (thin) to 900 (black); the nearest weight the font has draws it.
    #[serde(default = "normal_weight", skip_serializing_if = "is_normal_weight")]
    pub weight: u16,
    /// Italic; a font without an italic face is slanted instead.
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    /// Case change applied to the content before it is shaped.
    #[serde(default, skip_serializing_if = "TextTransform::is_none")]
    pub transform: TextTransform,
    /// Extra space after each character, in parts of the font size; may be negative.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub letter_spacing: f32,
    /// On a subtitle track: the clip keeps its own style when the track is given another one. The
    /// editor sets it when a style is applied to this clip alone.
    #[serde(default, skip_serializing_if = "is_false")]
    pub own_style: bool,
    /// Marks each word as it is spoken. Absent on texts without karaoke, so older projects load
    /// and save unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub karaoke: Option<crate::karaoke::TextKaraoke>,
}

impl TextContent {
    /// Plain centred white text in the default font.
    pub fn new(content: impl Into<String>, size: f32) -> TextContent {
        TextContent {
            content: content.into(),
            font: FontRef::Default,
            size,
            color: white(),
            outline: None,
            background: None,
            shadow: None,
            glow: None,
            align: TextAlign::Center,
            line_spacing: 1.0,
            weight: NORMAL_WEIGHT,
            italic: false,
            transform: TextTransform::None,
            letter_spacing: 0.0,
            own_style: false,
            karaoke: None,
        }
    }

    /// The content as it is shaped, with [`Self::transform`] applied.
    pub fn shaped_content(&self) -> std::borrow::Cow<'_, str> {
        match self.transform {
            TextTransform::None => std::borrow::Cow::Borrowed(&self.content),
            TextTransform::Uppercase => std::borrow::Cow::Owned(self.content.to_uppercase()),
            TextTransform::Lowercase => std::borrow::Cow::Owned(self.content.to_lowercase()),
        }
    }

    /// Changes the content. Karaoke word times set for the old content go, since the new one has
    /// other words: its cue is shared by characters again until new times are set.
    pub fn set_content(&mut self, content: impl Into<String>) {
        let content = content.into();
        if content == self.content {
            return;
        }
        self.content = content;
        if let Some(karaoke) = self.karaoke.as_mut() {
            karaoke.words.clear();
        }
    }

    /// Changes the case change. Karaoke word times go if the shaped text no longer has one word
    /// per time (no case change in use splits or joins words today, so this is a guard).
    pub fn set_transform(&mut self, transform: TextTransform) {
        self.transform = transform;
        let words = crate::karaoke::word_ranges(&self.shaped_content()).len();
        if let Some(karaoke) = self.karaoke.as_mut().filter(|karaoke| !karaoke.words.is_empty() && karaoke.words.len() != words) {
            karaoke.words.clear();
        }
    }
}

/// The weight of regular text.
pub const NORMAL_WEIGHT: u16 = 400;
/// The lightest and heaviest font weights a text may ask for.
pub const TEXT_WEIGHT_RANGE: std::ops::RangeInclusive<u16> = 100..=900;
/// The most letter spacing, either way, as a part of the font size.
pub const MAX_LETTER_SPACING: f32 = 2.0;
/// The farthest a glow may reach past the ink, as a part of the font size.
pub const MAX_GLOW_RADIUS: f32 = 2.0;

fn normal_weight() -> u16 {
    NORMAL_WEIGHT
}

fn is_normal_weight(weight: &u16) -> bool {
    *weight == NORMAL_WEIGHT
}

fn is_zero_f32(value: &f32) -> bool {
    *value == 0.0
}

/// A case change of the text before it is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
}

impl TextTransform {
    pub fn is_none(&self) -> bool {
        *self == TextTransform::None
    }
}

/// The most a text clip is rasterized larger than its font size asks, however wide it is placed.
/// Past this the glyph edges blur a little on screen, which costs less than the memory: at 4 a
/// line of 1080p text already comes to thousands of pixels.
pub const MAX_TEXT_SCALE: f32 = 4.0;
/// The least a text clip is rasterized at, however small it is placed.
pub const MIN_TEXT_SCALE: f32 = 0.125;

/// The scale to rasterize a text clip at so that it is never enlarged on screen: the picture is
/// `natural_width` pixels wide at scale 1, and at its widest it covers `widest` parts of a frame
/// `output_width` pixels wide. Kept within [`MIN_TEXT_SCALE`] and [`MAX_TEXT_SCALE`]; 1 when the
/// numbers say nothing.
///
/// Both `widest` and the result are rounded up to a step (see [`text_step_up`]), so the scale
/// depends on the widest only through its step: dragging a text wider draws it again only when the
/// widest crosses a step, and the decoder key ([`crate::DecoderSpec`]) holds just that step.
pub fn text_raster_scale(widest: f64, output_width: u32, natural_width: u32) -> f32 {
    let wanted = text_step_up(widest) * f64::from(output_width) / f64::from(natural_width);
    if natural_width == 0 || !wanted.is_finite() || wanted <= 0.0 {
        return 1.0;
    }
    (text_step_up(wanted) as f32).clamp(MIN_TEXT_SCALE, MAX_TEXT_SCALE)
}

/// `value` rounded up to the next step of a quarter octave (powers of 2^(1/4), about 19% apart),
/// never smaller than given. A value that is a step, give or take rounding, stays that step.
/// Anything that is not a positive finite number comes back as it is.
pub fn text_step_up(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return value;
    }
    let steps = (value.log2() * 4.0 - 1e-6).ceil();
    2f64.powf(steps / 4.0).max(value)
}

/// A colour as `[r, g, b, a]`, each 0 to 255, not premultiplied.
pub type Rgba = [u8; 4];

fn white() -> Rgba {
    [255, 255, 255, 255]
}

fn unit_f32() -> f32 {
    1.0
}

/// Which font a text clip uses. More kinds, such as a font from an installed package, may follow.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FontRef {
    /// The system UI font (Segoe UI, else Arial, else the first installed family).
    #[default]
    Default,
    /// A font installed on the system, by family name.
    System { family: String },
    /// The first face of a font file (.ttf, .otf, .ttc). The editor keeps it absolute in memory;
    /// a project file stores it relative to the project folder when it lies under it.
    File { path: String },
}

/// Horizontal alignment of text lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

/// An outline around each glyph.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextOutline {
    pub color: Rgba,
    /// Width, in parts of the font size.
    pub width: f32,
}

/// A box behind the text.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextBackground {
    pub color: Rgba,
    /// Space between the text and the box edge, in parts of the font size.
    pub padding: f32,
    /// Corner radius, in parts of the font size.
    pub radius: f32,
}

/// A drop shadow.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextShadow {
    pub color: Rgba,
    /// Offset `[x, y]`, in parts of the font size.
    pub offset: [f32; 2],
    /// Blur radius, in parts of the font size.
    pub blur: f32,
}

/// A glow around the glyphs (or around the outline when there is one), drawn from how far each
/// pixel is from the ink: full `intensity` at the ink, fading to nothing at `radius`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextGlow {
    /// Colour next to the ink.
    pub color: Rgba,
    /// Colour at the far edge; the glow blends from `color` to it with the distance. `None` keeps
    /// `color` all the way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_color: Option<Rgba>,
    /// How far the glow reaches past the ink, in parts of the font size.
    pub radius: f32,
    /// Opacity at the ink, 0 to [`MAX_GLOW_INTENSITY`]; above 1 the inner part stays solid longer.
    #[serde(default = "unit_f32")]
    pub intensity: f32,
    /// How fast it fades: the opacity goes as `(1 - distance / radius) ^ falloff`, so 1 fades
    /// evenly and larger numbers hug the ink.
    #[serde(default = "default_glow_falloff")]
    pub falloff: f32,
}

/// The most a glow's intensity may be.
pub const MAX_GLOW_INTENSITY: f32 = 4.0;
/// The least and most a glow's falloff may be.
pub const GLOW_FALLOFF_RANGE: std::ops::RangeInclusive<f32> = 0.1..=8.0;

fn default_glow_falloff() -> f32 {
    2.0
}

/// The preset animations of a clip: one to enter, one to leave and one that repeats.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClipAnimation {
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub enter: Option<AnimationRef>,
    #[serde(default, rename = "out", skip_serializing_if = "Option::is_none")]
    pub exit: Option<AnimationRef>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub repeat: Option<AnimationRef>,
}

impl ClipAnimation {
    /// Whether no animation is set.
    pub fn is_empty(&self) -> bool {
        self.enter.is_none() && self.exit.is_none() && self.repeat.is_none()
    }
}

/// One preset animation and how long one run of it lasts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationRef {
    /// The preset id, such as `fade`.
    pub preset: String,
    pub duration: Time,
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
    /// Numeric parameters that change with time, by name. One of these ignores its static value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keyframes: BTreeMap<String, Vec<Keyframe>>,
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
                            // Version 1 has no keyframes, whatever such a field may say.
                            effects: clip.effects.into_iter().map(|effect| Effect { keyframes: BTreeMap::new(), ..effect }).collect(),
                            gain_db: 0.0,
                            muted: false,
                            fade_in: Time::ZERO,
                            fade_out: Time::ZERO,
                            speed: 1.0,
                            placement: None,
                            motion: ClipMotion::default(),
                            text: None,
                            animation: ClipAnimation::default(),
                        })
                        .collect(),
                    transitions: track.transitions,
                    locked: false,
                    hidden: false,
                    subtitle: None,
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

/// The extensions of the still images a clip may show, lower-cased.
pub const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// The start of the source of every sticker: `asset:sticker/<pack id>/<file>`.
pub const STICKER_SOURCE_PREFIX: &str = "asset:sticker/";

/// Whether `source` is a sticker of an installed sticker pack.
pub fn is_sticker_source(source: &str) -> bool {
    source.starts_with(STICKER_SOURCE_PREFIX)
}

/// Whether `source`, a path or an `asset:` source, names a still image, by its extension.
pub fn is_image_source(source: &str) -> bool {
    let name = source.rsplit(['/', '\\']).next().unwrap_or(source);
    name.rsplit_once('.').is_some_and(|(_, extension)| IMAGE_EXTENSIONS.iter().any(|known| extension.eq_ignore_ascii_case(known)))
}

impl Clip {
    /// Whether the clip shows a still image: it has no sound and no source length to run out of.
    pub fn is_image(&self) -> bool {
        is_image_source(&self.source)
    }

    /// Whether the clip shows text, rasterized into a still picture.
    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    /// Whether the clip shows the same picture all the time, an image or text: it has no sound,
    /// no speed and no source length to run out of.
    pub fn is_still(&self) -> bool {
        self.is_image() || self.is_text()
    }

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

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// A plain clip of one second at the start of the timeline.
    pub(crate) fn clip() -> Clip {
        Clip { source: "a.mp4".into(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(90_000), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: ClipMotion::default(), text: None, animation: ClipAnimation::default() }
    }
}

impl Track {
    /// Whether this track only contributes sound.
    pub fn is_audio(&self) -> bool {
        self.kind == TrackKind::Audio
    }

    /// Whether this track holds imported subtitles.
    pub fn is_subtitle(&self) -> bool {
        self.subtitle.is_some()
    }
}
