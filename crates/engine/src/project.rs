//! Project model: the project JSON, in its one current format version.

use crate::frame_rate::FrameRate;
use crate::keyframe::{ClipMotion, Keyframe};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// The project format version: the only one this engine reads and writes.
pub const SUPPORTED_VERSION: u32 = 1;

/// A whole project: output settings and tracks (bottom track first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Format version; must be [`SUPPORTED_VERSION`].
    pub version: u32,
    /// Output frame size and cadence.
    pub output: Output,
    /// Tracks in compositing order, index 0 at the bottom.
    pub tracks: Vec<Track>,
    /// Markers on the ruler, in timeline time, sorted by time. Left out of the file when there
    /// are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// The beats of the sound of each source file, keyed by the `source` of the clips that show it,
    /// in the time of that source. Every clip of the source (after a split, or the file placed
    /// again) shares the one list and plays the part of it inside what it shows, see
    /// [`Project::clip_beat_times`]. Behind an [`Arc`] so the snapshots of the undo history share
    /// it. Left out of the file when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub beats: BTreeMap<String, Arc<ClipBeats>>,
    /// The motion tracks of objects in the source files, which overlays and effect regions follow
    /// (see [`crate::tracking`]). Left out of the file when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motion_tracks: Vec<crate::tracking::MotionTrack>,
}

/// The longest label of a [`Marker`], in characters.
pub const MAX_MARKER_LABEL: usize = 200;

/// A point of the timeline the user marked, shown on the ruler. It belongs to the timeline, not
/// to a clip: moving clips leaves it where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Timeline position; never negative.
    pub at: Time,
    /// What the marker is for; at most [`MAX_MARKER_LABEL`] characters. Left out when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Its colour on the ruler; the editor's own marker colour when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgba>,
}

impl Marker {
    /// A marker at `at` with no label and the default colour.
    pub fn at(at: Time) -> Marker {
        Marker { at, label: String::new(), color: None }
    }
}

/// The most beats a clip may keep: a beat every 50 ms for well over an hour.
pub const MAX_BEATS: usize = 100_000;
/// The fastest tempo a clip's beats may state, in beats per minute.
pub const MAX_TEMPO: f64 = 1000.0;

/// One beat of the sound of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beat {
    /// Position in the source media of the clip, not on the timeline: the beat stays on its sound
    /// whatever is done to the clip.
    pub at: Time,
    /// The first beat of a bar, drawn stronger.
    #[serde(default, skip_serializing_if = "is_false")]
    pub downbeat: bool,
}

/// The beats found in the sound of a source file, in the time of that source; the project keeps
/// them per source in [`Project::beats`]. Every clip of the source plays them over moves, trims,
/// splits and speed changes; only those inside the part of the source a clip plays are shown and
/// snapped to, see [`Project::clip_beat_times`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClipBeats {
    /// The tempo the beats were found at, in beats per minute of the source; 0 when unknown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tempo: f64,
    /// The beats, in increasing source time.
    pub beats: Vec<Beat>,
}

impl ClipBeats {
    /// Whether there are no beats, so the file leaves them out.
    pub fn is_empty(&self) -> bool {
        self.beats.is_empty()
    }
}

/// Output settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Output frames per second as an exact fraction (`{"num":30000,"den":1001}` for 29.97 fps).
    pub frame_rate: FrameRate,
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
        let censor = source == CENSOR_SOURCE;
        match self {
            TrackKind::Video => !text && !censor && !is_sticker_source(source),
            TrackKind::Audio => !text && !censor && !is_image_source(source) && !is_sticker_source(source),
            TrackKind::Sticker => !text && (is_image_source(source) || is_sticker_source(source) || source == CENSOR_SOURCE),
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
    /// per cue. Absent on every other track.
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
    /// Length on the timeline, holds included (see [`Clip::hold_start`]).
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
    /// the file when 1.
    #[serde(default = "default_speed", skip_serializing_if = "is_unit_speed")]
    pub speed: f64,
    /// Where the picture sits in the frame. Absent: as large as fits, centred, in its own shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<ClipPlacement>,
    /// Opacity, rotation and keyframes. Left out of the file when it is all default.
    #[serde(default, skip_serializing_if = "ClipMotion::is_default")]
    pub motion: ClipMotion,
    /// What a text clip shows and how. Present only on text clips, whose `source` is
    /// [`TEXT_SOURCE`]; absent on media clips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextContent>,
    /// Preset animations of the picture of the clip (video, image, sticker or text). Left out of
    /// the file when there are none; ignored on an audio track.
    #[serde(default, skip_serializing_if = "ClipAnimation::is_empty")]
    pub animation: ClipAnimation,
    /// Noise reduction of the sound. Left out of the file when off.
    #[serde(default, skip_serializing_if = "ClipDenoise::is_off")]
    pub denoise: ClipDenoise,
    /// Pitch shift and voice effect of the sound. Left out of the file when neutral.
    #[serde(default, skip_serializing_if = "ClipVoice::is_neutral")]
    pub voice: ClipVoice,
    /// Whether the moving part plays backwards: the same source range, `source_in` to
    /// [`Clip::source_end`], from its last frame to its first. The start hold then shows the last
    /// source frame and the end hold the first. Ignored on a still. Left out of the file when off.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reverse: bool,
    /// AI background removal of the picture. Ignored on text clips and audio tracks. Left out of
    /// the file when off.
    #[serde(default, skip_serializing_if = "ClipMatte::is_off")]
    pub matte: ClipMatte,
    /// Moves the picture with a motion track seen through another clip. Absent: the clip stays
    /// where its place and keyframes put it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<crate::tracking::Follow>,
    /// Moves the whole picture against a motion track of its own source, so the tracked object
    /// stays where it was at the anchor while the rest of the frame moves. Absent: no lock. A clip
    /// cannot both follow and lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<crate::tracking::TrackLock>,
    /// What a censor region hides and how. Present only on censor clips, whose `source` is
    /// [`CENSOR_SOURCE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub censor: Option<CensorContent>,
}

/// The `source` of every censor clip. Like [`TEXT_SOURCE`] it names no file.
pub const CENSOR_SOURCE: &str = "censor:";

/// The shape of a censor region or of the region of an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegionShape {
    #[default]
    Rect,
    Ellipse,
}

/// The largest blur of a censor region, in pixels of a 1080-line frame.
pub const MAX_CENSOR_BLUR: f64 = 120.0;
/// The largest block of a pixelated censor region, in pixels of a 1080-line frame.
pub const MAX_CENSOR_BLOCK: f64 = 200.0;
/// The narrowest and widest shape (width over height) of a censor region.
pub const CENSOR_ASPECT_RANGE: std::ops::RangeInclusive<f64> = 0.01..=100.0;

/// How a censor region hides what is under it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum CensorMode {
    /// A blur of `radius` pixels of a 1080-line frame.
    Blur { radius: f64 },
    /// Blocks of `block` pixels of a 1080-line frame.
    Pixelate { block: f64 },
    /// A box of one colour.
    Solid { color: Rgba },
}

impl Default for CensorMode {
    fn default() -> Self {
        CensorMode::Blur { radius: 30.0 }
    }
}

impl CensorMode {
    /// Why the mode cannot be used, if it cannot.
    pub fn problem(self) -> Option<String> {
        match self {
            CensorMode::Blur { radius } if !(radius.is_finite() && (0.0..=MAX_CENSOR_BLUR).contains(&radius)) => Some(format!("blur radius {radius} not from 0 to {MAX_CENSOR_BLUR}")),
            CensorMode::Pixelate { block } if !(block.is_finite() && (1.0..=MAX_CENSOR_BLOCK).contains(&block)) => Some(format!("pixelate block {block} not from 1 to {MAX_CENSOR_BLOCK}")),
            _ => None,
        }
    }
}

/// A censor region: a clip of a sticker track that changes the picture of the tracks under it
/// inside its box, as an adjustment layer limited to that box. Its box is placed as a sticker is
/// ([`Clip::placement`], keyframes, [`Clip::follow`]), with the shape `aspect`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CensorContent {
    #[serde(default)]
    pub shape: RegionShape,
    /// Width over height of the box, in output pixels.
    pub aspect: f64,
    /// Softness of the edge, in parts of half the smaller side of the box, 0 to 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub feather: f64,
    #[serde(default)]
    pub mode: CensorMode,
}

impl Default for CensorContent {
    fn default() -> Self {
        CensorContent { shape: RegionShape::Rect, aspect: 16.0 / 9.0, feather: 0.2, mode: CensorMode::default() }
    }
}

impl CensorContent {
    /// Why the region cannot be drawn, if it cannot.
    pub fn problem(&self) -> Option<String> {
        if !(self.aspect.is_finite() && CENSOR_ASPECT_RANGE.contains(&self.aspect)) {
            return Some(format!("aspect {} not from {} to {}", self.aspect, CENSOR_ASPECT_RANGE.start(), CENSOR_ASPECT_RANGE.end()));
        }
        if !(self.feather.is_finite() && (0.0..=1.0).contains(&self.feather)) {
            return Some(format!("feather {} not from 0 to 1", self.feather));
        }
        self.mode.problem()
    }
}

/// The part of the picture of its clip an effect is limited to. The effect runs on the placed
/// picture and its result is mixed with the picture without it through this shape, so the region
/// stays on the clip through a split or a move. Only the hiding effects take one, see
/// [`EffectRegion::mode_of`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EffectRegion {
    #[serde(default)]
    pub shape: RegionShape,
    /// Middle, in parts of the picture width and height.
    pub cx: f64,
    pub cy: f64,
    /// Size, in parts of the picture width and height.
    pub w: f64,
    pub h: f64,
    /// Clockwise degrees.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub angle: f64,
    /// Softness of the edge, as [`CensorContent::feather`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub feather: f64,
    /// A motion track of the clip's own source the region moves with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<crate::tracking::TrackLink>,
    /// With a link to a planar track: the region's corners follow the object's four corners in
    /// perspective (the box warped by the homography from the object's corners at the anchor to
    /// its corners now), so the region lies on the tracked surface. Ignored for a track without
    /// four corners, which moves the box as before.
    #[serde(default, skip_serializing_if = "is_false")]
    pub perspective: bool,
    /// The corners of the region (top-left, top-right, bottom-right, bottom-left of the box, parts
    /// of the picture) once a perspective link has moved it, set only on the copy the renderer
    /// draws (see `render::regions_now`); never saved.
    #[serde(skip)]
    pub corners: Option<[[f64; 2]; 4]>,
}

/// The effect packages that take a region (by the last part of their name), with the parameter
/// that sets their strength in output pixels.
pub const REGION_EFFECTS: [(&str, &str); 3] = [("gaussian-blur", "radius"), ("box-blur", "radius"), ("pixelate", "block")];

impl EffectRegion {
    /// Why the region cannot be used, if it cannot.
    pub fn problem(&self) -> Option<String> {
        let finite = [self.cx, self.cy, self.w, self.h, self.angle].iter().all(|v| v.is_finite());
        if !finite || self.cx.abs() > MAX_PLACEMENT_OFFSET || self.cy.abs() > MAX_PLACEMENT_OFFSET || !(0.0..=MAX_PLACEMENT_WIDTH).contains(&self.w) || !(0.0..=MAX_PLACEMENT_WIDTH).contains(&self.h) {
            return Some("the box must be finite and within the placement limits".to_owned());
        }
        if !(self.feather.is_finite() && (0.0..=1.0).contains(&self.feather)) {
            return Some(format!("feather {} not from 0 to 1", self.feather));
        }
        self.link.and_then(|link| link.problem())
    }

    /// Whether an effect of `package` may have a region.
    pub fn takes_region(package: &str) -> bool {
        let name = package.rsplit(['/', '\\']).next().unwrap_or(package);
        REGION_EFFECTS.iter().any(|(known, _)| *known == name)
    }

    /// How the region hides, for an effect of `package` with `params`, or `None` when the package
    /// takes no region. The strength is the package's own parameter as the package takes it, in
    /// pixels of the output frame; the compositor turns it into pixels of a 1080-line frame for the
    /// region (see [`CensorMode`]).
    pub fn mode_of(package: &str, params: &Map<String, Value>) -> Option<CensorMode> {
        let name = package.rsplit(['/', '\\']).next().unwrap_or(package);
        let (_, param) = REGION_EFFECTS.iter().find(|(known, _)| *known == name)?;
        let value = params.get(*param).and_then(Value::as_f64);
        Some(if name == "pixelate" {
            CensorMode::Pixelate { block: value.unwrap_or(16.0).clamp(1.0, MAX_CENSOR_BLOCK) }
        } else {
            CensorMode::Blur { radius: value.unwrap_or(10.0).clamp(0.0, MAX_CENSOR_BLUR) }
        })
    }
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
    /// What fills the glyphs.
    #[serde(default)]
    pub fill: TextPaint,
    /// Outlines around each glyph, each measured from the glyph edge (not from the outline inside
    /// it). They are drawn widest first, so a narrower one shows on top of a wider one; at most
    /// [`MAX_TEXT_OUTLINES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<TextOutline>,
    /// Box drawn behind the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<TextBackground>,
    /// Drop shadows, under the outlines. The first is drawn on top and the last at the bottom, so
    /// a stack of shadows moving away step by step reads as the depth of a 3D text; at most
    /// [`MAX_TEXT_SHADOWS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadows: Vec<TextShadow>,
    /// Shadows inside the glyphs, over the fill, the first on top; at most [`MAX_TEXT_SHADOWS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inner_shadows: Vec<TextInnerShadow>,
    /// Glow around the glyphs, under the shadows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<TextGlow>,
    /// Soft light spread from the whole text, under everything else of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<TextBloom>,
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
    /// Marks each word as it is spoken. Absent on texts without karaoke.
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
            fill: TextPaint::default(),
            outlines: Vec::new(),
            background: None,
            shadows: Vec::new(),
            inner_shadows: Vec::new(),
            glow: None,
            bloom: None,
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

    /// Every paint of the layers: the fill, the outlines, each shadow and its outlines, the inner
    /// shadows.
    pub fn paints_mut(&mut self) -> Vec<&mut TextPaint> {
        let mut paints = vec![&mut self.fill];
        paints.extend(self.outlines.iter_mut().map(|outline| &mut outline.paint));
        for shadow in &mut self.shadows {
            paints.push(&mut shadow.paint);
            paints.extend(shadow.outlines.iter_mut().map(|outline| &mut outline.paint));
        }
        paints.extend(self.inner_shadows.iter_mut().map(|shadow| &mut shadow.paint));
        paints
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

/// Which font a text clip uses.
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
    /// The first face of font file `file` (a path with forward slashes inside the pack) of the
    /// installed font pack `pack`, found under `<root>/packages/font/<pack>` of the package roots
    /// as an `asset:font/<pack>/<file>` source would be.
    Pack { pack: String, file: String },
}

impl FontRef {
    /// How a missing font is named in a warning: the family, the path, or `<pack>/<file>`.
    pub fn label(&self) -> Option<String> {
        match self {
            FontRef::Default => None,
            FontRef::System { family } => Some(family.clone()),
            FontRef::File { path } => Some(path.clone()),
            FontRef::Pack { pack, file } => Some(format!("{pack}/{file}")),
        }
    }
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

/// The most outlines a text, or one of its shadows, may have.
pub const MAX_TEXT_OUTLINES: usize = 4;
/// The most shadows a text may have, and the most inner shadows.
pub const MAX_TEXT_SHADOWS: usize = 32;
/// The most colour stops a gradient may have.
pub const MAX_GRADIENT_STOPS: usize = 16;
/// The widest an outline may be, as a part of the font size.
pub const MAX_TEXT_OUTLINE_WIDTH: f32 = 0.5;
/// The farthest a shadow may lie from the text, as a part of the font size.
pub const MAX_TEXT_SHADOW_DISTANCE: f32 = 4.0;
/// The most a shadow may blur, as a part of the font size.
pub const MAX_TEXT_SHADOW_BLUR: f32 = 2.0;
/// The least and most a texture may be scaled.
pub const TEXT_TEXTURE_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.01..=100.0;
/// The most cells a [`TextureFit::Glyph`] picture may be cut into.
pub const MAX_TEXTURE_CELLS: u32 = 64;

/// What a text layer (the fill, an outline, a shadow) is painted with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TextPaint {
    /// One colour.
    Solid { color: Rgba },
    /// Colours blending along a line across the box of the whole text (the line boxes of all its
    /// lines), from the first stop to the last.
    Gradient {
        /// At least 2 and at most [`MAX_GRADIENT_STOPS`], positions rising from 0 to 1.
        stops: Vec<GradientStop>,
        /// Direction the colours run in, in degrees clockwise on screen from left to right: 0 runs
        /// left to right and 90 top to bottom. (Assumed to match the gradient `angle` of a CapCut
        /// text effect, where 90 puts the first colour at the top; not compared with CapCut.)
        #[serde(default)]
        angle: f32,
        /// Whether the gradient runs across each glyph's own box instead of the whole text's
        /// (CapCut gradient `mode: character`).
        #[serde(default, skip_serializing_if = "is_false")]
        per_glyph: bool,
    },
    /// A picture laid over the text.
    Texture(TextTexture),
}

impl Default for TextPaint {
    fn default() -> TextPaint {
        TextPaint::Solid { color: white() }
    }
}

impl TextPaint {
    pub fn solid(color: Rgba) -> TextPaint {
        TextPaint::Solid { color }
    }

    /// The colour of a solid paint.
    pub fn solid_color(&self) -> Option<Rgba> {
        match self {
            TextPaint::Solid { color } => Some(*color),
            _ => None,
        }
    }

    /// A colour that stands for the paint where only one can be shown (a swatch, a karaoke
    /// colour): the solid colour, the first stop of a gradient, white for a texture.
    pub fn main_color(&self) -> Rgba {
        match self {
            TextPaint::Solid { color } => *color,
            TextPaint::Gradient { stops, .. } => stops.first().map_or(white(), |stop| stop.color),
            TextPaint::Texture(texture) => texture.base.as_deref().map_or(white(), TextPaint::main_color),
        }
    }

    /// The picture a texture paint draws.
    pub fn texture_image(&self) -> Option<&TextImageRef> {
        match self {
            TextPaint::Texture(texture) => Some(&texture.image),
            _ => None,
        }
    }
}

/// A colour stop of a [`TextPaint::Gradient`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// Where the colour is, 0 at the start of the gradient and 1 at its end.
    pub position: f32,
    pub color: Rgba,
}

/// A picture painting a text layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextTexture {
    pub image: TextImageRef,
    /// How the picture covers the text.
    #[serde(default)]
    pub fit: TextureFit,
    /// The picture's size: with [`TextureFit::Tile`] one copy is `scale` font sizes tall; with
    /// [`TextureFit::Stretch`] it multiplies the size that covers the text. Within
    /// [`TEXT_TEXTURE_SCALE_RANGE`]. (CapCut's texture `scale` is taken as the same with
    /// `stretch`; a guess.)
    #[serde(default = "unit_f32")]
    pub scale: f32,
    /// Turn of the picture, in degrees clockwise.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub angle: f32,
    /// Opacity of the whole paint, 0 to 1: the picture and, under it, the [`Self::base`] (CapCut
    /// layer `alpha`).
    #[serde(default = "unit_f32")]
    pub opacity: f32,
    /// How much of the picture is laid over the [`Self::base`], 0 to 1 (CapCut texture `alpha`):
    /// the base shows through by the rest. Without a base it only fades the picture, as
    /// `opacity` does. Taken apart from `opacity` because CapCut fades a layer with its base as one
    /// (a layer `alpha` below 1 must not let more of the base through the picture).
    #[serde(default = "unit_f32", skip_serializing_if = "is_one_f32")]
    pub mix: f32,
    /// With [`TextureFit::Glyph`], how many cells side by side the picture is cut into, 1 to
    /// [`MAX_TEXTURE_CELLS`]; the glyphs take them in turn (CapCut texture `range`).
    #[serde(default = "unit_u32", skip_serializing_if = "is_one_u32")]
    pub cells: u32,
    /// Mirrors the picture left to right (within its cell).
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_x: bool,
    /// Mirrors the picture top to bottom.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_y: bool,
    /// A solid or gradient paint under the picture, showing where the picture is transparent
    /// (CapCut texture `blend: solid` / `gradient`). `None` leaves those parts bare. Never a
    /// texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Box<TextPaint>>,
}

impl TextTexture {
    /// A picture stretched over the text, unturned, opaque, with no cells, flip or base.
    pub fn new(image: TextImageRef) -> TextTexture {
        TextTexture { image, fit: TextureFit::Stretch, scale: 1.0, angle: 0.0, opacity: 1.0, mix: 1.0, cells: 1, flip_x: false, flip_y: false, base: None }
    }
}

fn unit_u32() -> u32 {
    1
}

fn is_one_f32(value: &f32) -> bool {
    *value == 1.0
}

fn is_one_u32(value: &u32) -> bool {
    *value == 1
}

/// How a texture covers the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextureFit {
    /// Stretched to cover the box of the whole text, keeping its shape (CapCut `stretchMode:
    /// fit`, assumed); repeats past its edges.
    #[default]
    Stretch,
    /// Repeated from the top left corner of the text box (CapCut `wrapMode: repeat`).
    Tile,
    /// Like `Tile`, every other copy mirrored so the copies join without a seam.
    Mirror,
    /// Each glyph covered by one cell of the picture (see [`TextTexture::cells`]), keeping its
    /// shape, `scale` zooming in, repeated past the cell's edges (CapCut texture with `range`).
    Glyph,

}

/// The picture file of a [`TextTexture`]: PNG or JPEG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TextImageRef {
    /// A picture file. The editor keeps it absolute in memory; a project file may store it relative
    /// to the project folder.
    File { path: String },
    /// The picture `file` (a path with forward slashes inside the pack) of the installed style
    /// pack `pack` of type `pack_type`, found under `<root>/packages/<pack_type>/<pack>` of the
    /// package roots.
    Pack {
        pack: String,
        file: String,
        #[serde(default, skip_serializing_if = "TexturePackType::is_text_style")]
        pack_type: TexturePackType,
    },
}

impl TextImageRef {
    /// The picture `file` of the text style pack `pack`.
    pub fn text_style_pack(pack: impl Into<String>, file: impl Into<String>) -> TextImageRef {
        TextImageRef::Pack { pack: pack.into(), file: file.into(), pack_type: TexturePackType::TextStyle }
    }

    /// How a missing picture is named in a warning: the path, `<pack>/<file>` for a text style
    /// pack, or `<pack type>/<pack>/<file>`.
    pub fn label(&self) -> String {
        match self {
            TextImageRef::File { path } => path.clone(),
            TextImageRef::Pack { pack, file, pack_type: TexturePackType::TextStyle } => format!("{pack}/{file}"),
            TextImageRef::Pack { pack, file, pack_type } => format!("{}/{pack}/{file}", pack_type.name()),
        }
    }
}

/// Which type of style pack holds a texture picture: a text style pack, or a subtitle style pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TexturePackType {
    #[default]
    TextStyle,
    SubtitleStyle,
}

impl TexturePackType {
    /// The pack type, as a pack's `type` and its folder name say it.
    pub fn name(self) -> &'static str {
        match self {
            Self::TextStyle => "text-style",
            Self::SubtitleStyle => "subtitle-style",
        }
    }

    fn is_text_style(&self) -> bool {
        *self == Self::TextStyle
    }
}

/// An outline around each glyph: a ring from the glyph edge out to `width`, as CapCut draws a
/// stroke. It never covers the inside of the glyphs; that shows the paint of the layer the outline
/// belongs to (the text's fill, or a shadow's paint) or, where that paint is transparent, what lies
/// under the layer. Several outlines of one layer are drawn widest first, so each shows as a band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextOutline {
    /// Width past the glyph edge, in parts of the font size, 0 to [`MAX_TEXT_OUTLINE_WIDTH`].
    /// (CapCut's stroke `width` is taken as the same unit; a guess to check against CapCut.)
    pub width: f32,
    pub paint: TextPaint,
}

impl TextOutline {
    pub fn solid(color: Rgba, width: f32) -> TextOutline {
        TextOutline { width, paint: TextPaint::solid(color) }
    }
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

/// A drop shadow: the glyphs, grown by the shadow's own outlines, moved and blurred.
///
/// Units, assumed for CapCut text effects and not yet compared with CapCut: its `distance` is
/// taken as hundredths of the font size (divide by 100 for [`Self::distance`]), its `angle` as
/// degrees counter-clockwise from the right (so -45 lies down and to the right, as here). Its
/// `diffuse` (parts of the font size) is drawn as a [`Self::spread`] and a [`Self::blur`] of twice
/// it each: fitted to the halo of the CapCut neon covers, whose colour stays near full strength at
/// the glyph edge and fades over about four `diffuse` (a blur alone of any size stayed at half or
/// less). The covers also show that `diffuse` does not soften a shadow's outlines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextShadow {
    /// How far the shadow lies from the text, in parts of the font size, 0 to
    /// [`MAX_TEXT_SHADOW_DISTANCE`].
    #[serde(default)]
    pub distance: f32,
    /// Which way it lies, in degrees counter-clockwise from the right: 0 right, 90 up, -45 down
    /// and to the right.
    #[serde(default)]
    pub angle: f32,
    /// Blur radius, in parts of the font size, 0 to [`MAX_TEXT_SHADOW_BLUR`]. It softens the
    /// shadow's glyphs (grown by [`Self::spread`]), not its outlines.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub blur: f32,
    /// How far the shadow's glyphs are grown before the blur, in parts of the font size, 0 to
    /// [`MAX_TEXT_OUTLINE_WIDTH`]. A glow made of shadows keeps its full colour near the glyphs
    /// this way, where a blur alone would halve it at their edge.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub spread: f32,
    /// What the shadow's glyphs (inside its outline rings) are painted with. Transparent leaves
    /// only the rings, so a shadow at distance 0 with outlines adds strokes around the text.
    pub paint: TextPaint,
    /// Outlines of the shadow: rings measured from the glyph edge, drawn widest first like the
    /// text's and under nothing of the shadow itself (they lie outside its glyphs); at most
    /// [`MAX_TEXT_OUTLINES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<TextOutline>,
}

impl TextShadow {
    /// A plain shadow of one colour, moved by `offset` (`[x, y]`, y down, in parts of the font
    /// size).
    pub fn solid(color: Rgba, offset: [f32; 2], blur: f32) -> TextShadow {
        let (distance, angle) = offset_to_polar(offset);
        TextShadow { distance, angle, blur, spread: 0.0, paint: TextPaint::solid(color), outlines: Vec::new() }
    }

    /// The offset `[x, y]`, y down, in parts of the font size.
    pub fn offset(&self) -> [f32; 2] {
        polar_to_offset(self.distance, self.angle)
    }

    /// Sets the offset `[x, y]`, y down, in parts of the font size.
    pub fn set_offset(&mut self, offset: [f32; 2]) {
        (self.distance, self.angle) = offset_to_polar(offset);
    }
}

/// A shadow inside the glyphs, as CapCut draws it: the paint covers the glyphs where they overlap
/// themselves moved by the offset (`distance` towards `angle`, then blurred), so the fill stays
/// bare in a rim along the edges facing the other way, as if lit from there. Units as for
/// [`TextShadow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextInnerShadow {
    /// In parts of the font size, 0 to [`MAX_TEXT_SHADOW_DISTANCE`].
    #[serde(default)]
    pub distance: f32,
    /// In degrees counter-clockwise from the right.
    #[serde(default)]
    pub angle: f32,
    /// In parts of the font size, 0 to [`MAX_TEXT_SHADOW_BLUR`].
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub blur: f32,
    pub paint: TextPaint,
}

impl TextInnerShadow {
    /// The offset `[x, y]`, y down, in parts of the font size.
    pub fn offset(&self) -> [f32; 2] {
        polar_to_offset(self.distance, self.angle)
    }
}

/// `[x, y]` (y down) of `distance` towards `angle` degrees counter-clockwise from the right.
pub fn polar_to_offset(distance: f32, angle: f32) -> [f32; 2] {
    let radians = angle.to_radians();
    [distance * radians.cos(), -distance * radians.sin()]
}

/// The distance and angle (degrees counter-clockwise from the right) of `[x, y]`, y down.
pub fn offset_to_polar(offset: [f32; 2]) -> (f32, f32) {
    let distance = offset[0].hypot(offset[1]);
    let angle = if distance == 0.0 { 0.0 } else { (-offset[1]).atan2(offset[0]).to_degrees() };
    (distance, angle)
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

/// A bloom: the glyph coverage blurred into a soft light of `color` under the text, as a lit sign
/// spreads light around it. Unlike [`TextGlow`], which follows the distance to the ink, it is a
/// blur of the whole text, so letters close together light each other and add up.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextBloom {
    /// Colour of the light.
    pub color: Rgba,
    /// How bright the light is, 0 to 1.
    #[serde(default = "default_bloom_strength")]
    pub strength: f32,
    /// How far the light spreads, 0 to 1; 1 blurs it by [`MAX_BLOOM_RADIUS`] font sizes.
    #[serde(default = "default_bloom_range")]
    pub range: f32,
    /// Moves the light across, -1 to 1, in parts of its spread; positive to the right.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub dir_x: f32,
    /// Moves the light down, -1 to 1, in parts of its spread; positive downwards.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub dir_y: f32,
}

/// The blur radius of a bloom at range 1, as a part of the font size.
pub const MAX_BLOOM_RADIUS: f32 = 1.0;
/// The most a bloom may be moved either way along each axis, in parts of its spread.
pub const MAX_BLOOM_DIR: f32 = 1.0;

fn default_bloom_strength() -> f32 {
    0.5
}

fn default_bloom_range() -> f32 {
    0.4
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
    /// How long one run lasts. Negative on an enter or exit makes it a hold of that length: a
    /// still of the first (enter) or last (exit) source frame is shown for that long before or
    /// after the moving part of the clip, the clip is that much longer on the timeline, and the
    /// preset plays over the still. See [`Clip::hold_start`]. Never 0, and never negative on a
    /// repeat.
    pub duration: Time,
}

impl AnimationRef {
    /// How long one run lasts, whether or not it is a hold.
    pub fn length(&self) -> Time {
        Time::from_ticks(self.duration.ticks().saturating_abs())
    }

    /// Whether the run is a hold (a negative duration).
    pub fn is_hold(&self) -> bool {
        self.duration < Time::ZERO
    }

    /// The length of the hold, 0 when it is not one.
    pub fn hold(&self) -> Time {
        if self.is_hold() { self.length() } else { Time::ZERO }
    }
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

/// How noise is removed from the sound of a clip. See [`crate::denoise`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DenoiseMode {
    /// The sound is left as it is.
    #[default]
    Off,
    /// A neural network trained on speech (RNNoise): keeps voices, removes the rest.
    Voice,
    /// Spectral gating against a tracked noise floor, for steady broadband noise such as hiss,
    /// hum or fans, on any material.
    General,
}

/// Noise reduction of a clip, applied to its source audio before anything else.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipDenoise {
    /// Which method, or none.
    #[serde(default)]
    pub mode: DenoiseMode,
    /// How much of it, from 0 (no change) to 1 (all of it). 1 when absent.
    #[serde(default = "unit_f64", skip_serializing_if = "is_unit_f64")]
    pub strength: f64,
}

impl Default for ClipDenoise {
    fn default() -> Self {
        Self { mode: DenoiseMode::Off, strength: 1.0 }
    }
}

impl ClipDenoise {
    /// Whether it leaves the sound as it is, so the file leaves it out.
    pub fn is_off(&self) -> bool {
        self.mode == DenoiseMode::Off
    }
}

/// An effect laid over the voice of a clip after its pitch shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceEffect {
    /// No effect.
    #[default]
    None,
    /// Ring modulation at [`ROBOT_HZ`]: the metallic voice of an old film robot.
    Robot,
}

/// The frequency of the ring modulation of [`VoiceEffect::Robot`].
pub const ROBOT_HZ: f64 = 50.0;
/// The largest pitch shift either way, in semitones: one octave.
pub const MAX_PITCH_SEMITONES: f64 = 12.0;

/// Voice change of a clip: a pitch shift that keeps the speed, then an effect.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ClipVoice {
    /// Pitch shift in semitones, from -12 to 12; 0 keeps the pitch.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub semitones: f64,
    /// Effect after the shift.
    #[serde(default, skip_serializing_if = "is_no_voice_effect")]
    pub effect: VoiceEffect,
}

impl ClipVoice {
    /// Whether it leaves the sound as it is, so the file leaves it out.
    pub fn is_neutral(&self) -> bool {
        self.semitones == 0.0 && self.effect == VoiceEffect::None
    }

    /// The pitch shift in whole cents, the unit the mixer works in.
    pub fn pitch_cents(&self) -> i32 {
        if self.semitones.is_finite() {
            (self.semitones.clamp(-MAX_PITCH_SEMITONES, MAX_PITCH_SEMITONES) * 100.0).round() as i32
        } else {
            0
        }
    }
}

/// Which neural network cuts the person out of a clip's picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatteModel {
    /// The picture is left as it is.
    #[default]
    Off,
    /// MODNet webcam: finer edges (hair), slower.
    Quality,
    /// PP-HumanSeg V2 Lite: coarser edges, fast enough for every preview frame.
    Fast,
}

impl MatteModel {
    /// The package id of the model under `gpufx_ai_matting`, none when off.
    pub fn package_id(self) -> Option<&'static str> {
        match self {
            MatteModel::Off => None,
            MatteModel::Quality => Some("modnet-webcam"),
            MatteModel::Fast => Some("pp-humanseg-v2-lite"),
        }
    }
}

/// The widest feather and the largest edge shrink or grow of a matte, in pixels of a 1080-line
/// frame (scaled with the output height).
pub const MAX_MATTE_PIXELS: f64 = 20.0;
/// The most source frames before the current one a temporal matte averages.
pub const MAX_MATTE_TEMPORAL: u32 = 4;

/// AI background removal of a clip: the alpha of its picture taken from a matting network,
/// then shaped. Applied to the source picture before the clip's effects.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipMatte {
    /// Which network, or none.
    #[serde(default)]
    pub model: MatteModel,
    /// 0 keeps the soft matte; above 0 the matte becomes hard at this level (0 to 1).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub threshold: f64,
    /// Softening of the edge, in pixels of a 1080-line frame, 0 to [`MAX_MATTE_PIXELS`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub feather: f64,
    /// Below 0 shrinks the kept part, above 0 grows it, in pixels of a 1080-line frame, at most
    /// [`MAX_MATTE_PIXELS`] either way.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edge: f64,
    /// Keeps the background and removes the person instead.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
    /// Temporal smoothing: the matte is the mean of the current source frame's and those of the
    /// `temporal` source frames before it, 0 (off) to [`MAX_MATTE_TEMPORAL`]. The window comes
    /// from the source alone, so preview and export agree whatever was played or sought before.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub temporal: u32,
}

impl Default for ClipMatte {
    fn default() -> Self {
        Self { model: MatteModel::Off, threshold: 0.0, feather: 0.0, edge: 0.0, invert: false, temporal: 0 }
    }
}

impl ClipMatte {
    /// Whether it leaves the picture as it is, so the file leaves it out.
    pub fn is_off(&self) -> bool {
        self.model == MatteModel::Off
    }
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Ready-made voice changes, for the editor's buttons and the agent tools. They are not stored:
/// choosing one sets [`ClipVoice`] to its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoicePreset {
    /// Back to the clip's own voice.
    Normal,
    /// Nine semitones up: small and quick.
    Chipmunk,
    /// Six semitones down: large and slow.
    Deep,
    /// The clip's pitch with [`VoiceEffect::Robot`].
    Robot,
}

impl VoicePreset {
    /// Every preset, in the order the editor shows them.
    pub const ALL: [VoicePreset; 4] = [VoicePreset::Normal, VoicePreset::Chipmunk, VoicePreset::Deep, VoicePreset::Robot];

    /// What the preset sets.
    pub fn voice(self) -> ClipVoice {
        match self {
            VoicePreset::Normal => ClipVoice::default(),
            VoicePreset::Chipmunk => ClipVoice { semitones: 9.0, effect: VoiceEffect::None },
            VoicePreset::Deep => ClipVoice { semitones: -6.0, effect: VoiceEffect::None },
            VoicePreset::Robot => ClipVoice { semitones: 0.0, effect: VoiceEffect::Robot },
        }
    }

    /// The preset whose values `voice` has, if any.
    pub fn of(voice: &ClipVoice) -> Option<VoicePreset> {
        Self::ALL.into_iter().find(|preset| preset.voice() == *voice)
    }
}

fn is_no_voice_effect(effect: &VoiceEffect) -> bool {
    *effect == VoiceEffect::None
}

fn unit_f64() -> f64 {
    1.0
}

fn is_unit_f64(value: &f64) -> bool {
    *value == 1.0
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
    /// Limits the effect to a part of the picture. Absent: the whole picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<EffectRegion>,
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
    /// Width over height of the output frame.
    pub fn output_aspect(&self) -> f64 {
        f64::from(self.output.width) / f64::from(self.output.height.max(1))
    }

    /// Parses a project and rejects unknown versions. Does not run [`crate::validate`].
    pub fn from_json(text: &str) -> Result<Project, LoadError> {
        /// Only the version, read first, so a file of another version is refused as such and not
        /// as a field that does not parse.
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        let versioned: Versioned = serde_json::from_str(text).map_err(LoadError::Json)?;
        if versioned.version != SUPPORTED_VERSION {
            return Err(LoadError::UnsupportedVersion(versioned.version));
        }
        serde_json::from_str(text).map_err(LoadError::Json)
    }

    /// The beats kept for the source of `clip`, unless it is a still or none were found.
    pub fn clip_beats(&self, clip: &Clip) -> Option<&ClipBeats> {
        if clip.is_still() {
            return None;
        }
        self.beats.get(&clip.source).map(|beats| &**beats).filter(|beats| !beats.is_empty())
    }

    /// The beats `clip` plays, each with its timeline time, in order: the list of its source seen
    /// through the clip's trim and speed, see [`Clip::beat_times`].
    pub fn clip_beat_times<'a>(&'a self, clip: &'a Clip) -> impl Iterator<Item = (Time, Beat)> + 'a {
        clip.beat_times(self.clip_beats(clip).map_or(&[][..], |beats| &beats.beats[..]))
    }

    /// Whether some clip of the project shows `source` with its sound (not as a still).
    pub fn plays_source_sound(&self, source: &str) -> bool {
        self.tracks.iter().flat_map(|track| &track.clips).any(|clip| clip.source == source && !clip.is_still())
    }
}

/// The extensions of the still images a clip may show, lower-cased.
pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

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

    /// Whether the clip shows a picture that may be animated, a WebP or a GIF, so it keeps a source offset
    /// through a cut or a trim of its left edge as a video does (see [`crate::trim_start`]). Told
    /// by the extension, without reading the file on the edit thread: a still WebP or GIF shows the same
    /// picture at any offset, so it loses nothing. Provisional until the user settles it.
    pub fn may_be_animated(&self) -> bool {
        self.is_image() && !self.is_text() && self.source.rsplit_once('.').is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("webp") || extension.eq_ignore_ascii_case("gif"))
    }

    /// Whether the clip shows text, rasterized into a still picture.
    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    /// Whether the clip shows the same picture all the time, an image or text: it has no sound,
    /// no speed and no source length to run out of.
    pub fn is_still(&self) -> bool {
        self.is_image() || self.is_text() || self.is_censor()
    }

    /// Whether the clip is a censor region.
    pub fn is_censor(&self) -> bool {
        self.censor.is_some()
    }

    /// The still of the first source frame shown before the moving part, set by an enter
    /// animation with a negative duration. It is part of `duration`: the moving part starts this
    /// long after `start`. The audio is silent over it.
    pub fn hold_start(&self) -> Time {
        self.animation.enter.as_ref().map_or(Time::ZERO, AnimationRef::hold)
    }

    /// The still of the last source frame shown after the moving part, set by an exit animation
    /// with a negative duration. It is part of `duration`, at its end.
    pub fn hold_end(&self) -> Time {
        self.animation.exit.as_ref().map_or(Time::ZERO, AnimationRef::hold)
    }

    /// The timeline length of the moving part: `duration` without the holds.
    pub fn media_duration(&self) -> Time {
        self.duration.saturating_sub(self.hold_start()).saturating_sub(self.hold_end())
    }

    /// The timeline range of the moving part, where the source plays and the audio sounds.
    pub fn media_span(&self) -> (Time, Time) {
        let start = self.start.saturating_add(self.hold_start());
        (start, start.saturating_add(self.media_duration()))
    }

    /// The source time shown `offset` timeline ticks after `start` (negative inside a transition
    /// into the clip): `source_in` plus the scaled time since the moving part began. Inside a hold
    /// it stays on the first frame of the moving part, or on its last one (one tick before
    /// [`Clip::source_end`]).
    ///
    /// A reversed clip (see [`Clip::plays_reversed`]) mirrors that inside its source range: it
    /// shows `source_end - 1 tick - played`, so its first moving frame is the last source frame and
    /// its last one is the frame at `source_in`. Past its end, inside a transition, it goes on
    /// backwards before `source_in`.
    pub fn source_time_at(&self, offset: Time) -> Time {
        let hold_start = self.hold_start();
        let mut into = offset.saturating_sub(hold_start);
        if hold_start > Time::ZERO {
            into = into.max(Time::ZERO);
        }
        let mut played = self.source_offset(into);
        let last = self.source_span().saturating_sub(Time::from_ticks(1)).max(Time::ZERO);
        if self.hold_end() > Time::ZERO {
            played = played.min(last);
        }
        if self.plays_reversed() {
            return self.source_in.saturating_add(last.saturating_sub(played));
        }
        self.source_in.saturating_add(played)
    }

    /// Whether the clip plays its source backwards: [`Clip::reverse`] on a clip that is not a still.
    pub fn plays_reversed(&self) -> bool {
        self.reverse && !self.is_still()
    }

    /// The source ticks the moving part plays: `source_end - source_in`.
    pub fn source_span(&self) -> Time {
        self.source_offset(self.media_duration())
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
        self.source_in.saturating_add(self.source_offset(self.media_duration()))
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

    /// The timeline time at which the moving part of the clip plays source position `source`, by
    /// the clip's speed: the inverse of [`Clip::source_time_at`] outside the holds, rounded down.
    /// It does not check that the clip plays that position; see [`Clip::plays_source`].
    /// On a reversed clip the time falls as `source` grows.
    pub fn timeline_time_of(&self, source: Time) -> Time {
        let into = source.saturating_sub(self.source_in);
        let played = if self.plays_reversed() { self.source_span().saturating_sub(Time::from_ticks(1)).saturating_sub(into) } else { into };
        self.start.saturating_add(self.hold_start()).saturating_add(self.timeline_ticks(played))
    }

    /// Whether the moving part of the clip plays source position `source`.
    pub fn plays_source(&self, source: Time) -> bool {
        source >= self.source_in && source < self.source_end()
    }

    /// The beats of `beats` (the list of the clip's source) that the clip plays, each with its
    /// timeline time, in timeline order. A beat outside the part of the source the clip plays
    /// (trimmed away, or in the other half of a split) is left out. Finds the range by binary
    /// search. A reversed clip plays its beats from the last to the first, so they come out in
    /// the opposite order of `beats`.
    pub fn beat_times<'a>(&'a self, beats: &'a [Beat]) -> impl Iterator<Item = (Time, Beat)> + 'a {
        self.beats_in_order(self.played_beats(beats))
    }

    /// [`Clip::beat_times`] from timeline time `from` to `to`, both included, found by binary
    /// search: the timeline time of a beat grows with its source time (falls, on a reversed
    /// clip), so a clip on screen costs the beats it shows, not all those of its source.
    pub fn beat_times_between<'a>(&'a self, beats: &'a [Beat], from: Time, to: Time) -> impl Iterator<Item = (Time, Beat)> + 'a {
        let played = self.played_beats(beats);
        let at = |beat: &Beat| self.timeline_time_of(beat.at);
        let (low, high) = if self.plays_reversed() {
            (played.partition_point(|beat| at(beat) > to), played.partition_point(|beat| at(beat) >= from))
        } else {
            (played.partition_point(|beat| at(beat) < from), played.partition_point(|beat| at(beat) <= to))
        };
        self.beats_in_order(&played[low..high.max(low)])
    }

    /// The beats of `beats` inside the source range the clip plays, in source order.
    fn played_beats<'a>(&self, beats: &'a [Beat]) -> &'a [Beat] {
        let first = beats.partition_point(|beat| beat.at < self.source_in);
        let end = beats.partition_point(|beat| beat.at < self.source_end());
        &beats[first..end.max(first)]
    }

    /// `played` (in source order) with timeline times, in timeline order.
    fn beats_in_order<'a>(&'a self, played: &'a [Beat]) -> impl Iterator<Item = (Time, Beat)> + 'a {
        let reversed = self.plays_reversed();
        let count = played.len();
        (0..count).map(move |i| played[if reversed { count - 1 - i } else { i }]).map(|beat| (self.timeline_time_of(beat.at), beat))
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// A plain clip of one second at the start of the timeline.
    pub(crate) fn clip() -> Clip {
        Clip { source: "a.mp4".into(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(90_000), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: ClipMotion::default(), text: None, animation: ClipAnimation::default(), denoise: Default::default(), voice: Default::default(), reverse: false, matte: Default::default(), follow: None, lock: None, censor: None }
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
