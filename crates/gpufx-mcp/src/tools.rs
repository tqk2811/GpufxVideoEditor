//! The tool table: name, description, parameter schema, and how the parameters become a request.

use std::path::PathBuf;
use std::sync::Arc;

use engine::Time;
use gpufx_editor::agent::protocol::{double_option, ClipRef, KaraokePatch, TextAnimationPatch, TextPatch};
use gpufx_editor::agent::{AgentEdit, AgentError, AgentRequest, CaptureTarget};
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
#[serde(deny_unknown_fields)]
pub struct ClipParam {
    pub track: usize,
    pub clip: usize,
}

impl From<ClipParam> for ClipRef {
    fn from(clip: ClipParam) -> Self {
        ClipRef { track: clip.track, clip: clip.clip }
    }
}

/// A place in the frame, in parts of the frame.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct PlacementParam {
    pub x: f64,
    pub y: f64,
    pub width: f64,
}

impl From<PlacementParam> for engine::ClipPlacement {
    fn from(p: PlacementParam) -> Self {
        Self { x: p.x, y: p.y, width: p.width }
    }
}

/// A property of a clip that can have keyframes.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ClipPropertyParam {
    X,
    Y,
    Width,
    Opacity,
    Rotation,
}

impl From<ClipPropertyParam> for engine::ClipProperty {
    fn from(property: ClipPropertyParam) -> Self {
        match property {
            ClipPropertyParam::X => Self::X,
            ClipPropertyParam::Y => Self::Y,
            ClipPropertyParam::Width => Self::Width,
            ClipPropertyParam::Opacity => Self::Opacity,
            ClipPropertyParam::Rotation => Self::Rotation,
        }
    }
}

/// How a keyframe moves on to the next one.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum EasingParam {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl From<EasingParam> for engine::Easing {
    fn from(easing: EasingParam) -> Self {
        match easing {
            EasingParam::Linear => Self::Linear,
            EasingParam::EaseIn => Self::EaseIn,
            EasingParam::EaseOut => Self::EaseOut,
            EasingParam::EaseInOut => Self::EaseInOut,
        }
    }
}

/// Which font a text uses.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum FontParam {
    /// The system UI font (Segoe UI, else Arial).
    Default,
    /// A font installed on the system, by a family font_list names.
    System { family: String },
    /// A font file (.ttf, .otf, .ttc) by absolute path on a local disk (not a network share), at
    /// most 50 MiB; its first face is used.
    File { path: String },
}

impl From<FontParam> for engine::FontRef {
    fn from(font: FontParam) -> Self {
        match font {
            FontParam::Default => Self::Default,
            FontParam::System { family } => Self::System { family },
            FontParam::File { path } => Self::File { path },
        }
    }
}

/// Horizontal alignment of the lines of a text.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum AlignParam {
    Left,
    Center,
    Right,
}

impl From<AlignParam> for engine::TextAlign {
    fn from(align: AlignParam) -> Self {
        match align {
            AlignParam::Left => Self::Left,
            AlignParam::Center => Self::Center,
            AlignParam::Right => Self::Right,
        }
    }
}

/// A case change of a text before it is drawn.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum TransformParam {
    None,
    Uppercase,
    Lowercase,
}

impl From<TransformParam> for engine::TextTransform {
    fn from(transform: TransformParam) -> Self {
        match transform {
            TransformParam::None => Self::None,
            TransformParam::Uppercase => Self::Uppercase,
            TransformParam::Lowercase => Self::Lowercase,
        }
    }
}

/// An outline around each glyph.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct OutlineParam {
    /// [r, g, b, a], each 0 to 255.
    pub color: [u8; 4],
    /// In parts of the font size, 0 or more.
    pub width: f32,
}

/// A box behind the text.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct BackgroundParam {
    /// [r, g, b, a], each 0 to 255.
    pub color: [u8; 4],
    /// Space between the text and the edge of the box, in parts of the font size, 0 or more.
    pub padding: f32,
    /// Corner radius, in parts of the font size, 0 or more.
    pub radius: f32,
}

/// A drop shadow.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct ShadowParam {
    /// [r, g, b, a], each 0 to 255.
    pub color: [u8; 4],
    /// [x, y], in parts of the font size.
    pub offset: [f32; 2],
    /// Blur radius, in parts of the font size, 0 or more.
    pub blur: f32,
}

/// A glow around the glyphs, fading with the distance from them.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct GlowParam {
    /// [r, g, b, a] next to the glyphs, each 0 to 255.
    pub color: [u8; 4],
    /// [r, g, b, a] at the far edge; the glow blends from color to it. Absent keeps color.
    pub outer_color: Option<[u8; 4]>,
    /// How far it reaches past the glyphs (or the outline), in parts of the font size, 0 to 2.
    pub radius: f32,
    /// Opacity next to the glyphs, 0 to 4 (1 by default); above 1 the inner part stays solid longer.
    pub intensity: Option<f32>,
    /// Fade: opacity goes as (1 - distance / radius) ^ falloff, 0.1 to 8 (2 by default).
    pub falloff: Option<f32>,
}

/// How karaoke marks the spoken word.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum KaraokeModeParam {
    /// No marking.
    None,
    /// Each word takes the colour and opacity of its state.
    Color,
    /// As color, with a box behind the active word (active.background, else a yellow box, never the word colour).
    Box,
    /// Scaling the active word is not drawn yet: draws as color.
    Scale,
}

impl From<KaraokeModeParam> for engine::KaraokeMode {
    fn from(mode: KaraokeModeParam) -> Self {
        match mode {
            KaraokeModeParam::None => Self::None,
            KaraokeModeParam::Color => Self::Color,
            KaraokeModeParam::Box => Self::Box,
            KaraokeModeParam::Scale => Self::Scale,
        }
    }
}

/// The look of the words in one karaoke state; an absent field keeps the text's own.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct KaraokeLookParam {
    /// [r, g, b, a], each 0 to 255.
    pub color: Option<[u8; 4]>,
    /// [r, g, b, a] of the box behind the word in box mode.
    pub background: Option<[u8; 4]>,
    /// Above 0, at most 4. Kept; not drawn yet.
    pub scale: Option<f32>,
    /// 0 to 1.
    pub opacity: Option<f32>,
}

impl From<KaraokeLookParam> for engine::KaraokeLook {
    fn from(look: KaraokeLookParam) -> Self {
        Self { color: look.color, background: look.background, scale: look.scale, opacity: look.opacity }
    }
}

/// When one word is spoken, in ticks of the clip's source time (0 is where the cue starts).
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct KaraokeWordParam {
    pub start: i64,
    pub end: i64,
    /// Drawn with the emphasis look on top of its state's.
    #[serde(default)]
    pub emphasis: bool,
}

/// Karaoke of a text. An absent field stays as it is.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct KaraokeParam {
    pub mode: Option<KaraokeModeParam>,
    /// The word being spoken. Merged into the look it has field by field; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    pub active: Option<Option<KaraokeLookParam>>,
    /// The words already spoken. Merged into the look it has field by field; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    pub spoken: Option<Option<KaraokeLookParam>>,
    /// The words still to come. Merged into the look it has field by field; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    pub upcoming: Option<Option<KaraokeLookParam>>,
    /// Words marked emphasis, on top of their state's look. Merged into the look it has field by field; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    pub emphasis: Option<Option<KaraokeLookParam>>,
    /// One time per word (words are runs without white space, after the case change), in order
    /// and not overlapping. An empty list shares the cue among the words by their characters,
    /// which is what a text without times does. New content without words drops the old times.
    pub words: Option<Vec<KaraokeWordParam>>,
}

impl From<KaraokeParam> for KaraokePatch {
    fn from(karaoke: KaraokeParam) -> Self {
        Self {
            mode: karaoke.mode.map(Into::into),
            active: karaoke.active.map(|look| look.map(Into::into)),
            spoken: karaoke.spoken.map(|look| look.map(Into::into)),
            upcoming: karaoke.upcoming.map(|look| look.map(Into::into)),
            emphasis: karaoke.emphasis.map(|look| look.map(Into::into)),
            words: karaoke.words.map(|words| words.into_iter().map(|word| engine::KaraokeWord { start: ticks(word.start), end: ticks(word.end), emphasis: word.emphasis }).collect()),
        }
    }
}

/// One preset animation of a clip with a picture.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct AnimationRefParam {
    /// An id clip_animation_list names for this group.
    pub preset: String,
    /// How long one run lasts, in ticks, above 0.
    pub duration: i64,
}

impl From<AnimationRefParam> for engine::AnimationRef {
    fn from(animation: AnimationRefParam) -> Self {
        Self { preset: animation.preset, duration: ticks(animation.duration) }
    }
}

/// The animations of a clip with a picture. A group that is absent stays as it is; null takes its animation off.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct AnimationParam {
    /// Plays as the clip appears.
    #[serde(default, rename = "in", deserialize_with = "double_option")]
    pub enter: Option<Option<AnimationRefParam>>,
    /// Plays as the clip goes away.
    #[serde(default, rename = "out", deserialize_with = "double_option")]
    pub exit: Option<Option<AnimationRefParam>>,
    /// Repeats the whole time the clip shows.
    #[serde(default, rename = "loop", deserialize_with = "double_option")]
    pub repeat: Option<Option<AnimationRefParam>>,
}

impl From<AnimationParam> for TextAnimationPatch {
    fn from(animation: AnimationParam) -> Self {
        let group = |value: Option<Option<AnimationRefParam>>| value.map(|value| value.map(Into::into));
        Self { enter: group(animation.enter), exit: group(animation.exit), repeat: group(animation.repeat) }
    }
}

/// The style of a text. Every field is optional.
#[derive(Deserialize, JsonSchema, Clone, Default)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct TextStyleParam {
    pub font: Option<FontParam>,
    /// Font size in parts of the output height (a new text has 0.08), above 0.
    pub size: Option<f32>,
    /// [r, g, b, a], each 0 to 255; white by default.
    pub color: Option<[u8; 4]>,
    /// null takes the outline off.
    #[serde(default, deserialize_with = "double_option")]
    pub outline: Option<Option<OutlineParam>>,
    /// null takes the box off.
    #[serde(default, deserialize_with = "double_option")]
    pub background: Option<Option<BackgroundParam>>,
    /// null takes the shadow off.
    #[serde(default, deserialize_with = "double_option")]
    pub shadow: Option<Option<ShadowParam>>,
    /// null takes the glow off.
    #[serde(default, deserialize_with = "double_option")]
    pub glow: Option<Option<GlowParam>>,
    pub align: Option<AlignParam>,
    /// Line height as a multiple of the font size, above 0.
    pub line_spacing: Option<f32>,
    /// Font weight, 100 to 900 (400 regular, 700 bold); the nearest weight the font has draws it.
    pub weight: Option<u16>,
    /// Italic; a font without an italic face is slanted.
    pub italic: Option<bool>,
    /// Case change before drawing; none by default.
    pub transform: Option<TransformParam>,
    /// Extra space after each character, in parts of the font size, -2 to 2; 0 by default.
    pub letter_spacing: Option<f32>,
    pub animation: Option<AnimationParam>,
    /// Marks each word as it is spoken; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    pub karaoke: Option<Option<KaraokeParam>>,
    /// A style of an installed text or subtitle style pack, as text_style_list names it
    /// ("<pack>/<item>"): its values are copied into the text first, and the other fields here
    /// override them. A subtitle style also moves the text where its layout says.
    pub style_id: Option<String>,
}

impl TextStyleParam {
    fn patch(self, content: Option<String>) -> TextPatch {
        TextPatch {
            content,
            font: self.font.map(Into::into),
            size: self.size,
            color: self.color,
            outline: self.outline.map(|value| value.map(|v| engine::TextOutline { color: v.color, width: v.width })),
            background: self.background.map(|value| value.map(|v| engine::TextBackground { color: v.color, padding: v.padding, radius: v.radius })),
            shadow: self.shadow.map(|value| value.map(|v| engine::TextShadow { color: v.color, offset: v.offset, blur: v.blur })),
            glow: self.glow.map(|value| value.map(|v| engine::TextGlow { color: v.color, outer_color: v.outer_color, radius: v.radius, intensity: v.intensity.unwrap_or(1.0), falloff: v.falloff.unwrap_or(2.0) })),
            align: self.align.map(Into::into),
            line_spacing: self.line_spacing,
            weight: self.weight,
            italic: self.italic,
            transform: self.transform.map(Into::into),
            letter_spacing: self.letter_spacing,
            animation: self.animation.map(Into::into),
            karaoke: self.karaoke.map(|value| value.map(Into::into)),
            style_id: self.style_id,
        }
    }
}

/// What a keyframe call animates: exactly a property, or an effect with one of its parameters.
fn keyframe_target(property: Option<ClipPropertyParam>, effect: Option<usize>, param: Option<String>) -> Result<engine::KeyframeTarget, AgentError> {
    match (property, effect, param) {
        (Some(property), None, None) => Ok(engine::KeyframeTarget::Property(property.into())),
        (None, Some(effect), Some(param)) => Ok(engine::KeyframeTarget::EffectParam { effect, param }),
        _ => Err(AgentError::new("invalid_params", "give either property, or both effect and param")),
    }
}

macro_rules! params {
    ($name:ident { $($(#[$meta:meta])* $field:ident : $kind:ty),* $(,)? }) => {
        #[derive(Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($(#[$meta])* pub $field: $kind,)* }
    };
}

params!(NoParams {});
params!(ProjectNewParams { #[serde(default)] discard_changes: bool });
params!(ProjectOpenParams { path: PathBuf, #[serde(default)] discard_changes: bool });
params!(ProjectSaveParams { path: Option<PathBuf>, #[serde(default)] overwrite: bool });
params!(PackagesListParams { filter: Option<String> });
params!(TrackAddParams { expected_revision: u64, kind: TrackKindParam });
params!(TrackLockedParams { expected_revision: u64, track: usize, locked: bool });
params!(TrackHiddenParams { expected_revision: u64, track: usize, hidden: bool });
params!(ClipAddParams { expected_revision: u64, track: usize, source: PathBuf, start: i64, source_in: Option<i64>, duration: Option<i64>, speed: Option<f64> });
params!(TextAddParams { expected_revision: u64, track: Option<usize>, start: i64, duration: i64, content: String, style: Option<TextStyleParam> });
params!(TextSetParams {
    expected_revision: u64,
    clip: ClipParam,
    content: Option<String>,
    font: Option<FontParam>,
    size: Option<f32>,
    color: Option<[u8; 4]>,
    #[serde(default, deserialize_with = "double_option")]
    outline: Option<Option<OutlineParam>>,
    #[serde(default, deserialize_with = "double_option")]
    background: Option<Option<BackgroundParam>>,
    #[serde(default, deserialize_with = "double_option")]
    shadow: Option<Option<ShadowParam>>,
    #[serde(default, deserialize_with = "double_option")]
    glow: Option<Option<GlowParam>>,
    align: Option<AlignParam>,
    line_spacing: Option<f32>,
    weight: Option<u16>,
    italic: Option<bool>,
    transform: Option<TransformParam>,
    letter_spacing: Option<f32>,
    animation: Option<AnimationParam>,
    /// Marks each word as it is spoken; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    karaoke: Option<Option<KaraokeParam>>,
    /// A style text_style_list names, applied before the other fields, which override it.
    style_id: Option<String>,
});
/// Which styles text_style_list names.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum StyleTypeParam {
    Text,
    Subtitle,
}
params!(TextStyleListParams {
    /// text or subtitle; both when absent.
    #[serde(rename = "type")]
    kind: Option<StyleTypeParam>,
});
params!(SubtitleImportParams {
    expected_revision: u64,
    /// The SRT file to read, UTF-8 with or without a byte order mark.
    path: Option<PathBuf>,
    /// The SRT text itself, instead of a path.
    content: Option<String>,
    /// A subtitle style text_style_list names.
    style_id: Option<String>,
});
params!(SubtitleSetStyleParams {
    expected_revision: u64,
    track: usize,
    /// A subtitle style text_style_list names.
    style_id: String,
    /// Also restyle the cues that were given a style of their own.
    #[serde(default)]
    all: bool,
});
params!(SubtitleExportParams { track: usize, path: Option<PathBuf>, #[serde(default)] overwrite: bool });
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
params!(SpeedParams { expected_revision: u64, clip: ClipParam, speed: f64 });
/// The clip whose animations clip_animation_set changes, and the groups to change, as in
/// [`AnimationParam`].
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipAnimationSetParams {
    pub expected_revision: u64,
    pub clip: ClipParam,
    /// Plays as the clip appears.
    #[serde(default, rename = "in", deserialize_with = "double_option")]
    pub enter: Option<Option<AnimationRefParam>>,
    /// Plays as the clip goes away.
    #[serde(default, rename = "out", deserialize_with = "double_option")]
    pub exit: Option<Option<AnimationRefParam>>,
    /// Repeats the whole time the clip shows.
    #[serde(default, rename = "loop", deserialize_with = "double_option")]
    pub repeat: Option<Option<AnimationRefParam>>,
}

fn clip_animation_set(p: ClipAnimationSetParams) -> Result<AgentRequest, AgentError> {
    if p.enter.is_none() && p.exit.is_none() && p.repeat.is_none() {
        return Err(AgentError::new("invalid_params", "give at least one of in, out and loop"));
    }
    let animation = AnimationParam { enter: p.enter, exit: p.exit, repeat: p.repeat }.into();
    edit(p.expected_revision, AgentEdit::ClipSetAnimation { clip: p.clip.into(), animation })
}

params!(MutedParams { expected_revision: u64, clip: ClipParam, muted: bool });
params!(FadesParams { expected_revision: u64, clip: ClipParam, fade_in: i64, fade_out: i64 });
params!(TransformParams { expected_revision: u64, clip: ClipParam, placement: Option<PlacementParam>, opacity: Option<f64>, rotation: Option<f64> });
params!(KeyframeSetParams { expected_revision: u64, clip: ClipParam, property: Option<ClipPropertyParam>, effect: Option<usize>, param: Option<String>, at: i64, value: f64, easing: Option<EasingParam> });
params!(KeyframeRemoveParams { expected_revision: u64, clip: ClipParam, property: Option<ClipPropertyParam>, effect: Option<usize>, param: Option<String>, at: i64 });
params!(RevisionParams { expected_revision: u64 });
params!(ExportStartParams { out: PathBuf, #[serde(default)] overwrite: bool, encoder: Option<String> });
params!(ExportJobParams { job: Option<u64> });
params!(FrameCaptureParams { time: Option<i64>, frame: Option<i64>, max_width: Option<u32> });

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

fn text_add(p: TextAddParams) -> Result<AgentRequest, AgentError> {
    let style = p.style.unwrap_or_default().patch(None);
    edit(p.expected_revision, AgentEdit::TextAdd { track: p.track, start: ticks(p.start), duration: ticks(p.duration), content: p.content, style })
}

fn text_set(p: TextSetParams) -> Result<AgentRequest, AgentError> {
    let style = TextStyleParam { font: p.font, size: p.size, color: p.color, outline: p.outline, background: p.background, shadow: p.shadow, glow: p.glow, align: p.align, line_spacing: p.line_spacing, weight: p.weight, italic: p.italic, transform: p.transform, letter_spacing: p.letter_spacing, animation: p.animation, karaoke: p.karaoke, style_id: p.style_id };
    edit(p.expected_revision, AgentEdit::TextSet { clip: p.clip.into(), patch: style.patch(p.content) })
}

/// Width a capture is limited to unless the call says otherwise.
const DEFAULT_MAX_WIDTH: u32 = 1280;

fn frame_capture(p: FrameCaptureParams) -> Result<AgentRequest, AgentError> {
    let target = match (p.time, p.frame) {
        (Some(time), None) => CaptureTarget::Time(ticks(time)),
        (None, Some(frame)) => CaptureTarget::Frame(frame),
        _ => return Err(AgentError::new("invalid_params", "give exactly one of time and frame")),
    };
    let max_width = p.max_width.unwrap_or(DEFAULT_MAX_WIDTH);
    if max_width == 0 {
        return Err(AgentError::new("invalid_params", "max_width must be positive"));
    }
    Ok(AgentRequest::FrameCapture { target, max_width })
}

/// Every tool. Times are integer ticks of 1/90000 s.
pub fn all_tools() -> Vec<ToolDef> {
    vec![
        tool!("session_status", "The mode, project file, revision, and whether an export or a drag is going on.", NoParams, |_| Ok(AgentRequest::SessionStatus)),
        tool!("project_new", "Replaces the project with an empty one. Refused with unsaved_changes unless discard_changes is true; in the open window (live mode) unsaved work is always kept, whatever discard_changes says.", ProjectNewParams, |p| Ok(AgentRequest::ProjectNew { discard_changes: p.discard_changes })),
        tool!("project_open", "Opens a project file. Refused with unsaved_changes unless discard_changes is true; in the open window (live mode) unsaved work is always kept, whatever discard_changes says.", ProjectOpenParams, |p| Ok(AgentRequest::ProjectOpen { path: p.path, discard_changes: p.discard_changes })),
        tool!("project_save", "Saves the project, to path or to its own file. Another existing file is refused with file_exists unless overwrite is true; a source, package or the running export's output is always refused.", ProjectSaveParams, |p| Ok(AgentRequest::ProjectSave { path: p.path, overwrite: p.overwrite })),
        tool!("project_get", "The project JSON, its duration and frame count. Clips are named by {track, clip} indexes, which change after an edit.", NoParams, |_| Ok(AgentRequest::ProjectGet)),
        tool!("packages_list", "Effect and transition packages, filtered by a text.", PackagesListParams, |p| Ok(AgentRequest::PackagesList { filter: p.filter })),
        tool!("track_add", "Adds an empty track on top; returns its index.", TrackAddParams, |p| edit(p.expected_revision, AgentEdit::TrackAdd { kind: p.kind.into() })),
        tool!("track_set_locked", "Locks or unlocks a track. Edits of a locked track fail with track_locked.", TrackLockedParams, |p| edit(p.expected_revision, AgentEdit::TrackSetLocked { track: p.track, locked: p.locked })),
        tool!("track_set_hidden", "Hides or shows a track in picture and sound.", TrackHiddenParams, |p| edit(p.expected_revision, AgentEdit::TrackSetHidden { track: p.track, hidden: p.hidden })),
        tool!("clip_add", "Adds a source file, a still image (png, jpg, webp, on a video track only), or a file of an installed pack as asset:<type>/<id>/<file> (type sfx, music or sticker), as a clip at start (ticks). duration defaults to what the stream the track plays still holds at that speed, and to 5 seconds for an image, whose end can be set freely. speed defaults to 1, between 0.1 and 10, and does not apply to an image. Returns the new clip.", ClipAddParams, |p| edit(p.expected_revision, AgentEdit::ClipAdd { track: p.track, source: p.source, start: ticks(p.start), source_in: p.source_in.map(ticks), duration: p.duration.map(ticks), speed: p.speed })),
        tool!("text_add", "Adds a text clip of content at start (ticks) for duration, on track or, when track is absent, on the first video track with room or a new one. style sets any of font, size (parts of the output height, 0.08 by default), color, outline, background, shadow, glow, align, line_spacing, weight, italic, transform, letter_spacing and animation; what is absent is as the Text tab's new text: white, centred, in the default font (the system UI font, such as Segoe UI). It is placed at its natural size in the middle of the frame, which clip_set_transform can change. Returns the new clip.", TextAddParams, text_add),
        tool!("text_set", "Changes a text clip: give content and any style field of text_add; what is absent stays, and null takes an outline, background, shadow, glow, karaoke or an animation group off. karaoke sets how the spoken word is marked (mode none, color, box or scale; looks for active, spoken, upcoming and emphasis words) and may give each word its time in source ticks; without times the words share the cue by their characters. Where the text is placed keeps the scale it was given around the same centre, so a bigger size or longer content draws bigger. Refused with not_text for a clip that is not text.", TextSetParams, text_set),
        tool!("font_list", "The fonts a text can name, each as the font value to give: the default font first (the system UI font, such as Segoe UI), then the installed families. state is loading while the installed fonts are still being read; ask again then.", NoParams, |_| Ok(AgentRequest::FontList)),
        tool!("clip_animation_list", "The preset animations a clip with a picture (video, image, sticker or text) can name: each with its id, its group (in, out or loop) and default_duration in ticks (for loop, one cycle); a pack preset also has its name, pack and applies_to, and effect: null or {package, available}, the gpufx effect it runs while it plays and whether that package is installed (a missing one is left out and the preset plays only its motion). per_unit: true marks a preset that moves the characters, words or lines of a text one by one, such as typewriter; it is for text clips only. Only an id of the group is accepted there.", NoParams, |_| Ok(AgentRequest::ClipAnimationList)),
        tool!("text_style_list", "The styles of the installed text style and subtitle style packs, filtered by type (text or subtitle; both when absent): each with its id to give as style_id to text_add (in style) or text_set, its name, type, category, preview_text, pack, the style values it copies into a text (absent ones are as on a new text; an absent size keeps the text's), its animations (durations in ticks) and, for a subtitle style, its layout (anchor_y and max_width in parts of the frame). font_hint names fonts used when installed. state is loading while the packs are still being read.", TextStyleListParams, |p| Ok(AgentRequest::TextStyleList { kind: p.kind.map(|kind| match kind { StyleTypeParam::Text => "text".to_owned(), StyleTypeParam::Subtitle => "subtitle".to_owned() }) })),
        tool!("clip_animation_set", "Sets the preset animations of a clip with a picture (video, image, sticker or text): in plays as it appears, out as it goes away, loop the whole time it shows. Each is {preset, duration in ticks}, with a preset clip_animation_list names for that group; a group that is absent stays, and null takes its animation off. Refused with invalid_params for a clip on an audio track.", ClipAnimationSetParams, clip_animation_set),
        tool!("subtitle_import_srt", "Adds an SRT file as a new subtitle track on top: one text clip per cue, from its start to its end, with the cue's text (tags such as <i> and {\\an8} dropped). Give exactly one of path and content. style_id, a subtitle style text_style_list names, is copied into every cue and remembered on the track; without one the cues are the default text near the bottom of the frame. A cue that cannot be read is left out, a cue running into the next is cut where the next starts, and one starting together with an earlier one is left out; warnings says which, by line. Returns track, cues and warnings. One undo step.", SubtitleImportParams, |p| edit(p.expected_revision, AgentEdit::SubtitleImport { path: p.path, content: p.content, style_id: p.style_id })),
        tool!("subtitle_set_style", "Gives a subtitle track the subtitle style style_id: its values are copied into every cue, which keeps its text and time, and each cue goes where the style's layout says unless its place has keyframes. A cue given a style of its own (text_set with style_id, or Apply style in the editor) keeps it unless all is true. Refused with invalid_params for a track that is not a subtitle track. Returns how many cues were restyled. One undo step.", SubtitleSetStyleParams, |p| edit(p.expected_revision, AgentEdit::SubtitleSetStyle { track: p.track, style_id: p.style_id, all: p.all })),
        tool!("subtitle_export_srt", "The text clips of a subtitle track as an SRT file (CRLF line ends), returned as srt, and also written to path when given. An existing file is refused with file_exists unless overwrite is true.", SubtitleExportParams, |p| Ok(AgentRequest::SubtitleExport { track: p.track, path: p.path, overwrite: p.overwrite })),
        tool!("clip_move", "Moves a clip to a track, as near start as the other clips allow. Returns where it landed.", ClipMoveParams, |p| edit(p.expected_revision, AgentEdit::ClipMove { clip: p.clip.into(), track: p.track, start: ticks(p.start) })),
        tool!("clip_trim_start", "Moves the start edge of a clip, keeping the frames under the end edge.", ClipTrimStartParams, |p| edit(p.expected_revision, AgentEdit::ClipTrimStart { clip: p.clip.into(), start: ticks(p.start) })),
        tool!("clip_trim_end", "Moves the end edge of a clip.", ClipTrimEndParams, |p| edit(p.expected_revision, AgentEdit::ClipTrimEnd { clip: p.clip.into(), end: ticks(p.end) })),
        tool!("clip_split", "Splits a clip at a time; returns the second half.", ClipSplitParams, |p| edit(p.expected_revision, AgentEdit::ClipSplit { clip: p.clip.into(), at: ticks(p.at) })),
        tool!("clip_delete", "Deletes a clip and the transitions touching it.", ClipOnlyParams, |p| edit(p.expected_revision, AgentEdit::ClipDelete { clip: p.clip.into() })),
        tool!("effect_add", "Appends an effect package to a clip.", EffectAddParams, |p| edit(p.expected_revision, AgentEdit::EffectAdd { clip: p.clip.into(), package: p.package, params: p.params })),
        tool!("effect_remove", "Removes an effect of a clip by index.", EffectRemoveParams, |p| edit(p.expected_revision, AgentEdit::EffectRemove { clip: p.clip.into(), effect: p.effect })),
        tool!("transition_add", "Adds a transition between clip after_clip and the next, which must touch.", TransitionAddParams, |p| edit(p.expected_revision, AgentEdit::TransitionAdd { track: p.track, after_clip: p.after_clip, package: p.package, duration: p.duration.map(ticks), params: p.params })),
        tool!("transition_remove", "Removes the transition after a clip.", TransitionRemoveParams, |p| edit(p.expected_revision, AgentEdit::TransitionRemove { track: p.track, after_clip: p.after_clip })),
        tool!("clip_set_speed", "Sets the playback speed of a clip, 0.1 to 10; 2 plays twice as fast. Keeps the source range, so the timeline length becomes source length / speed. Audio keeps its pitch. Refused with speed_overlap when the clip would run into the next one, and with transition_on_edge when its end holds a transition.", SpeedParams, |p| edit(p.expected_revision, AgentEdit::ClipSetSpeed { clip: p.clip.into(), speed: p.speed })),
        tool!("clip_set_gain", "Sets the gain of a clip in decibels.", GainParams, |p| edit(p.expected_revision, AgentEdit::ClipSetGain { clip: p.clip.into(), gain_db: p.gain_db })),
        tool!("clip_set_muted", "Mutes or unmutes a clip.", MutedParams, |p| edit(p.expected_revision, AgentEdit::ClipSetMuted { clip: p.clip.into(), muted: p.muted })),
        tool!("clip_set_fades", "Sets the audio fades of a clip, in ticks.", FadesParams, |p| edit(p.expected_revision, AgentEdit::ClipSetFades { clip: p.clip.into(), fade_in: ticks(p.fade_in), fade_out: ticks(p.fade_out) })),
        tool!("clip_set_transform", "Sets the static place (x, y, width, all in parts of the frame), opacity (0 to 1) and rotation (clockwise degrees) of a clip. What is absent stays. A number with keyframes ignores its static value.", TransformParams, |p| edit(p.expected_revision, AgentEdit::ClipSetTransform { clip: p.clip.into(), placement: p.placement.map(Into::into), opacity: p.opacity, rotation: p.rotation })),
        tool!("keyframe_set", "Sets a keyframe at `at`, ticks from the start of the clip (clip.start, not the clip_time of an effect), from 0 up to but not including the duration. Give property (x, y, width, opacity, rotation), or effect (its index) and param (a float parameter of that effect). Replaces one at the same tick; easing (linear, ease_in, ease_out, ease_in_out) is how the value moves to the next keyframe, and absent keeps the one replaced. Refused with keyframe_outside_clip.", KeyframeSetParams, |p| { let target = keyframe_target(p.property, p.effect, p.param)?; edit(p.expected_revision, AgentEdit::KeyframeSet { clip: p.clip.into(), target, at: ticks(p.at), value: p.value, easing: p.easing.map(Into::into) }) }),
        tool!("keyframe_remove", "Removes the keyframe at `at` of a property, or of a parameter of an effect, as in keyframe_set, also one that lies outside the clip. After the last one inside the clip the static value applies again. Refused with keyframe_not_found.", KeyframeRemoveParams, |p| { let target = keyframe_target(p.property, p.effect, p.param)?; edit(p.expected_revision, AgentEdit::KeyframeRemove { clip: p.clip.into(), target, at: ticks(p.at) }) }),
        tool!("undo", "Undoes the last edit.", RevisionParams, |p| Ok(AgentRequest::Undo { expected_revision: p.expected_revision })),
        tool!("redo", "Redoes an undone edit.", RevisionParams, |p| Ok(AgentRequest::Redo { expected_revision: p.expected_revision })),
        tool!("frame_capture", "The composited frame at a time (ticks) or frame index, exactly one of them, as a PNG no wider than max_width (default 1280). A position outside the timeline is an error.", FrameCaptureParams, frame_capture),
        tool!("export_start", "Exports the project to an MP4 in the background; returns a job. Refused with file_exists unless overwrite is true. One export at a time; edits wait for it.", ExportStartParams, |p| Ok(AgentRequest::ExportStart { out: p.out, overwrite: p.overwrite, encoder: p.encoder })),
        tool!("export_status", "The state of an export (the last one when job is absent): running with done and total, finished with its report, cancelled, or failed with a message.", ExportJobParams, |p| Ok(AgentRequest::ExportStatus { job: p.job })),
        tool!("export_cancel", "Asks the running export to stop; export_status tells when it has.", ExportJobParams, |p| Ok(AgentRequest::ExportCancel { job: p.job })),
    ]
}
