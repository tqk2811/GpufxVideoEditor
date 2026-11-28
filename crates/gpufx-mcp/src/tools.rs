//! The tool table: name, description, parameter schema, and how the parameters become a request.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use engine::Time;
use gpufx_editor::agent::protocol::{double_option, ClipRef, KaraokePatch, TextAnimationPatch, TextPatch};
use gpufx_editor::agent::previews::{self, PreviewFormat};
use gpufx_editor::agent::{AgentEdit, AgentError, AgentRequest, CaptureTarget};
use rmcp::model::{JsonObject, Tool};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{Map, Value};

/// What a preview comes as.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum PreviewFormatParam {
    Frames,
    Video,
}

impl From<PreviewFormatParam> for PreviewFormat {
    fn from(format: PreviewFormatParam) -> Self {
        match format {
            PreviewFormatParam::Frames => PreviewFormat::Frames,
            PreviewFormatParam::Video => PreviewFormat::Video,
        }
    }
}

/// A kind of track, as the tools name it.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum TrackKindParam {
    /// Video files and still images, not stickers.
    Video,
    /// Sound files, and video files for their sound.
    Audio,
    /// Still images and stickers.
    Sticker,
    /// Text clips only; subtitle tracks are text tracks.
    Text,
}

impl From<TrackKindParam> for engine::TrackKind {
    fn from(kind: TrackKindParam) -> Self {
        match kind {
            TrackKindParam::Video => Self::Video,
            TrackKindParam::Audio => Self::Audio,
            TrackKindParam::Sticker => Self::Sticker,
            TrackKindParam::Text => Self::Text,
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
    /// A font of an installed font pack, by the pack id and the file in it that font_list names.
    Pack { pack: String, file: String },
}

impl From<FontParam> for engine::FontRef {
    fn from(font: FontParam) -> Self {
        match font {
            FontParam::Default => Self::Default,
            FontParam::System { family } => Self::System { family },
            FontParam::File { path } => Self::File { path },
            FontParam::Pack { pack, file } => Self::Pack { pack, file },
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

/// An outline around each glyph, of one colour; it replaces the text's first outline (adding one
/// when it has none) and leaves the others.
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

/// A drop shadow of one colour; it replaces the text's first shadow (adding one when it has none)
/// and leaves the others.
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

/// Which type of installed style pack holds a texture picture.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum PackTypeParam {
    TextStyle,
    SubtitleStyle,
}

/// The picture of a texture: PNG or JPEG.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ImageParam {
    /// A picture file by absolute path on a local disk (not a network share or device).
    File { path: String },
    /// The picture file (forward slashes, inside the pack) of the installed style pack pack;
    /// pack_type is text-style (the default) or subtitle-style.
    Pack { pack: String, file: String, pack_type: Option<PackTypeParam> },
}

/// How a texture covers the text.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum FitParam {
    /// Stretched to cover the box of the whole text, keeping its shape.
    Stretch,
    /// Repeated from the top left corner of the text box.
    Tile,
    /// Like tile, every other copy mirrored.
    Mirror,
    /// The picture cut into `cells` side by side; each glyph (counting only glyphs that draw ink,
    /// so spaces take none) is covered by the next cell in turn, centred on the glyph and grown with
    /// the outline being painted. scale multiplies the size that covers the glyph.
    Glyph,
}

/// A colour stop of a gradient.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct StopParam {
    /// 0 at the start of the gradient, 1 at its end; not below the stop before.
    pub position: f32,
    /// [r, g, b, a], each 0 to 255.
    pub color: [u8; 4],
}

/// The paint under a texture's picture, shown where the picture is see-through.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum BaseParam {
    /// One colour, [r, g, b, a], each 0 to 255.
    Solid { color: [u8; 4] },
    /// As the gradient paint: 2 to 16 stops, angle (0), per_glyph (false).
    Gradient { stops: Vec<StopParam>, angle: Option<f32>, per_glyph: Option<bool> },
}

fn gradient_paint(stops: Vec<StopParam>, angle: Option<f32>, per_glyph: Option<bool>) -> engine::TextPaint {
    engine::TextPaint::Gradient { stops: stops.into_iter().map(|stop| engine::GradientStop { position: stop.position, color: stop.color }).collect(), angle: angle.unwrap_or(0.0), per_glyph: per_glyph.unwrap_or(false) }
}

impl From<BaseParam> for engine::TextPaint {
    fn from(base: BaseParam) -> Self {
        match base {
            BaseParam::Solid { color } => Self::Solid { color },
            BaseParam::Gradient { stops, angle, per_glyph } => gradient_paint(stops, angle, per_glyph),
        }
    }
}

/// What a layer of a text (the fill, an outline, a shadow) is painted with.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum PaintParam {
    /// One colour, [r, g, b, a], each 0 to 255.
    Solid { color: [u8; 4] },
    /// Colours blending across the box of the whole text: 2 to 16 stops; angle in degrees
    /// clockwise on screen, 0 left to right and 90 top to bottom (0 by default). per_glyph true
    /// runs the gradient across each glyph on its own instead (false by default).
    Gradient { stops: Vec<StopParam>, angle: Option<f32>, per_glyph: Option<bool> },
    /// A picture laid over the text. scale (0.01 to 100, 1 by default): with tile, one copy is
    /// scale font sizes tall; with stretch or glyph it multiplies the size that covers the text or
    /// the glyph. angle turns the picture, degrees clockwise (0). opacity 0 to 1 (1) fades the
    /// picture with its base; mix 0 to 1 (1) is how much of the picture covers the base. fit is
    /// stretch by default. cells (1 to 64, 1 by default) is how many cells the picture is cut into
    /// with fit glyph. flip_x and flip_y mirror the picture left to right and top to bottom (false).
    /// base is a solid or gradient paint under the picture, shown where it is see-through (none).
    Texture {
        image: ImageParam,
        fit: Option<FitParam>,
        scale: Option<f32>,
        angle: Option<f32>,
        opacity: Option<f32>,
        mix: Option<f32>,
        cells: Option<u32>,
        flip_x: Option<bool>,
        flip_y: Option<bool>,
        base: Option<BaseParam>,
    },
}

impl From<PaintParam> for engine::TextPaint {
    fn from(paint: PaintParam) -> Self {
        match paint {
            PaintParam::Solid { color } => Self::Solid { color },
            PaintParam::Gradient { stops, angle, per_glyph } => gradient_paint(stops, angle, per_glyph),
            PaintParam::Texture { image, fit, scale, angle, opacity, mix, cells, flip_x, flip_y, base } => {
                let image = match image {
                    ImageParam::File { path } => engine::TextImageRef::File { path },
                    ImageParam::Pack { pack, file, pack_type } => engine::TextImageRef::Pack {
                        pack,
                        file,
                        pack_type: match pack_type {
                            Some(PackTypeParam::SubtitleStyle) => engine::TexturePackType::SubtitleStyle,
                            _ => engine::TexturePackType::TextStyle,
                        },
                    },
                };
                let fit = match fit {
                    Some(FitParam::Tile) => engine::TextureFit::Tile,
                    Some(FitParam::Mirror) => engine::TextureFit::Mirror,
                    Some(FitParam::Glyph) => engine::TextureFit::Glyph,
                    _ => engine::TextureFit::Stretch,
                };
                Self::Texture(engine::TextTexture {
                    image,
                    fit,
                    scale: scale.unwrap_or(1.0),
                    angle: angle.unwrap_or(0.0),
                    opacity: opacity.unwrap_or(1.0),
                    mix: mix.unwrap_or(1.0),
                    cells: cells.unwrap_or(1),
                    flip_x: flip_x.unwrap_or(false),
                    flip_y: flip_y.unwrap_or(false),
                    base: base.map(|base| Box::new(base.into())),
                })
            }
        }
    }
}

/// One outline of a list of outlines.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct LayerOutlineParam {
    /// Width past the glyph edge, in parts of the font size, 0 to 0.5. Each outline is a ring
    /// measured from the glyph edge (the inside of the glyphs shows the layer's paint), and the
    /// widest is drawn first.
    pub width: f32,
    pub paint: PaintParam,
}

impl From<LayerOutlineParam> for engine::TextOutline {
    fn from(outline: LayerOutlineParam) -> Self {
        engine::TextOutline { width: outline.width, paint: outline.paint.into() }
    }
}

/// One drop shadow of a list. Give where it lies either as distance and angle or as offset.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct LayerShadowParam {
    /// How far it lies from the text, in parts of the font size, 0 to 4 (0 by default).
    pub distance: Option<f32>,
    /// Which way, in degrees counter-clockwise from the right: 0 right, 90 up, -45 down and to the
    /// right (0 by default).
    pub angle: Option<f32>,
    /// [x, y] in parts of the font size, y down, in place of distance and angle.
    pub offset: Option<[f32; 2]>,
    /// Blur radius, in parts of the font size, 0 to 2 (0 by default); it softens the shadow's
    /// glyphs, not its outlines.
    pub blur: Option<f32>,
    /// How far the shadow's glyphs are grown before the blur, in parts of the font size, 0 to 0.5
    /// (0 by default).
    pub spread: Option<f32>,
    pub paint: PaintParam,
    /// Outlines of the shadow itself, at most 4, as the text's outlines.
    pub outlines: Option<Vec<LayerOutlineParam>>,
}

/// One shadow inside the glyphs, over the fill, the first on top. Units as for shadows; give
/// distance and angle or offset.
#[derive(Deserialize, JsonSchema, Clone)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct InnerShadowParam {
    /// In parts of the font size, 0 to 4 (0 by default).
    pub distance: Option<f32>,
    /// Degrees counter-clockwise from the right (0 by default).
    pub angle: Option<f32>,
    /// [x, y] in parts of the font size, y down, in place of distance and angle.
    pub offset: Option<[f32; 2]>,
    /// In parts of the font size, 0 to 2 (0 by default).
    pub blur: Option<f32>,
    pub paint: PaintParam,
}

/// The distance and angle of a shadow given either way; refused when both ways are given.
fn shadow_polar(field: &str, distance: Option<f32>, angle: Option<f32>, offset: Option<[f32; 2]>) -> Result<(f32, f32), AgentError> {
    match offset {
        Some(_) if distance.is_some() || angle.is_some() => Err(AgentError::new("invalid_params", format!("{field}: give offset, or distance and angle, not both"))),
        Some(offset) => Ok(engine::offset_to_polar(offset)),
        None => Ok((distance.unwrap_or(0.0), angle.unwrap_or(0.0))),
    }
}

/// A soft light of one colour spread from the whole text, under it.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct BloomParam {
    /// [r, g, b, a], each 0 to 255.
    pub color: [u8; 4],
    /// Brightness, 0 to 1 (0.5 by default).
    pub strength: Option<f32>,
    /// Spread, 0 to 1 (0.4 by default); 1 blurs it by one font size.
    pub range: Option<f32>,
    /// Moves the light right (negative left), -1 to 1 in parts of its spread (0 by default).
    pub dir_x: Option<f32>,
    /// Moves the light down (negative up), -1 to 1 in parts of its spread (0 by default).
    pub dir_y: Option<f32>,
}

type ShadowLists = (Option<Vec<engine::TextShadow>>, Option<Vec<engine::TextInnerShadow>>);

/// The shadows and inner shadows of a text style, as the engine has them.
fn layers_patch(shadows: Option<Vec<LayerShadowParam>>, inner_shadows: Option<Vec<InnerShadowParam>>) -> Result<ShadowLists, AgentError> {
    let shadows = shadows
        .map(|shadows| {
            shadows
                .into_iter()
                .enumerate()
                .map(|(index, shadow)| {
                    let (distance, angle) = shadow_polar(&format!("shadows[{index}]"), shadow.distance, shadow.angle, shadow.offset)?;
                    let outlines = shadow.outlines.unwrap_or_default().into_iter().map(Into::into).collect();
                    Ok(engine::TextShadow { distance, angle, blur: shadow.blur.unwrap_or(0.0), spread: shadow.spread.unwrap_or(0.0), paint: shadow.paint.into(), outlines })
                })
                .collect::<Result<Vec<_>, AgentError>>()
        })
        .transpose()?;
    let inner_shadows = inner_shadows
        .map(|shadows| {
            shadows
                .into_iter()
                .enumerate()
                .map(|(index, shadow)| {
                    let (distance, angle) = shadow_polar(&format!("inner_shadows[{index}]"), shadow.distance, shadow.angle, shadow.offset)?;
                    Ok(engine::TextInnerShadow { distance, angle, blur: shadow.blur.unwrap_or(0.0), paint: shadow.paint.into() })
                })
                .collect::<Result<Vec<_>, AgentError>>()
        })
        .transpose()?;
    Ok((shadows, inner_shadows))
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
    /// How long one run lasts, in ticks, not 0. Negative on in or out: a hold of that length, a
    /// still of the first (in) or last (out) source frame added before or after the clip, with the
    /// animation playing over it; the clip gets that much longer and later clips of its track move.
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
    /// [r, g, b, a], each 0 to 255; white by default. Fills the text with this one colour, in place
    /// of a gradient or texture fill it had.
    pub color: Option<[u8; 4]>,
    /// The first outline; null takes every outline off.
    #[serde(default, deserialize_with = "double_option")]
    pub outline: Option<Option<OutlineParam>>,
    /// null takes the box off.
    #[serde(default, deserialize_with = "double_option")]
    pub background: Option<Option<BackgroundParam>>,
    /// The first shadow; null takes every shadow off.
    #[serde(default, deserialize_with = "double_option")]
    pub shadow: Option<Option<ShadowParam>>,
    /// null takes the glow off.
    #[serde(default, deserialize_with = "double_option")]
    pub glow: Option<Option<GlowParam>>,
    /// The whole fill: solid, gradient or texture. Not together with color.
    pub fill: Option<PaintParam>,
    /// Every outline (at most 4), replacing the text's; [] takes them off. Not together with outline.
    pub outlines: Option<Vec<LayerOutlineParam>>,
    /// Every drop shadow (at most 32), replacing the text's; the first is drawn on top, so a stack
    /// moving away step by step reads as 3D depth; [] takes them off. Not together with shadow.
    pub shadows: Option<Vec<LayerShadowParam>>,
    /// Every inner shadow (at most 32), replacing the text's; [] takes them off.
    pub inner_shadows: Option<Vec<InnerShadowParam>>,
    /// A light spread from the whole text; null takes it off.
    #[serde(default, deserialize_with = "double_option")]
    pub bloom: Option<Option<BloomParam>>,
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
    fn patch(self, content: Option<String>) -> Result<TextPatch, AgentError> {
        let (shadows, inner_shadows) = layers_patch(self.shadows, self.inner_shadows)?;
        Ok(TextPatch {
            content,
            font: self.font.map(Into::into),
            size: self.size,
            color: self.color,
            outline: self.outline.map(|value| value.map(|v| engine::TextOutline::solid(v.color, v.width))),
            background: self.background.map(|value| value.map(|v| engine::TextBackground { color: v.color, padding: v.padding, radius: v.radius })),
            shadow: self.shadow.map(|value| value.map(|v| engine::TextShadow::solid(v.color, v.offset, v.blur))),
            glow: self.glow.map(|value| value.map(|v| engine::TextGlow { color: v.color, outer_color: v.outer_color, radius: v.radius, intensity: v.intensity.unwrap_or(1.0), falloff: v.falloff.unwrap_or(2.0) })),
            fill: self.fill.map(Into::into),
            outlines: self.outlines.map(|outlines| outlines.into_iter().map(Into::into).collect()),
            shadows,
            inner_shadows,
            bloom: self.bloom.map(|value| value.map(|v| engine::TextBloom { color: v.color, strength: v.strength.unwrap_or(0.5), range: v.range.unwrap_or(0.4), dir_x: v.dir_x.unwrap_or(0.0), dir_y: v.dir_y.unwrap_or(0.0) })),
            align: self.align.map(Into::into),
            line_spacing: self.line_spacing,
            weight: self.weight,
            italic: self.italic,
            transform: self.transform.map(Into::into),
            letter_spacing: self.letter_spacing,
            animation: self.animation.map(Into::into),
            karaoke: self.karaoke.map(|value| value.map(Into::into)),
            style_id: self.style_id,
        })
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
params!(PackagesListParams { #[doc = "Every word must be in some field, ignoring case and accents; best matches first."] filter: Option<String>, #[doc = "At most this many items (of each list), after ranking; all when absent."] limit: Option<NonZeroUsize> });
params!(ListPackagesParams { kind: Option<String> });
params!(AssetListParams { r#type: Option<String>, #[doc = "Every word must be in some field, ignoring case and accents; best matches first."] filter: Option<String>, #[doc = "At most this many items (of each list), after ranking; all when absent."] limit: Option<NonZeroUsize> });
params!(FontListParams { #[doc = "Every word must be in some field, ignoring case and accents; best matches first."] filter: Option<String>, #[doc = "At most this many items (of each list), after ranking; all when absent."] limit: Option<NonZeroUsize> });
params!(ClipAnimationListParams { #[doc = "Every word must be in some field, ignoring case and accents; best matches first."] filter: Option<String>, #[doc = "At most this many items (of each list), after ranking; all when absent."] limit: Option<NonZeroUsize> });
params!(TrackAddParams { expected_revision: u64, kind: TrackKindParam });
params!(TrackLockedParams { expected_revision: u64, track: usize, locked: bool });
params!(TrackHiddenParams { expected_revision: u64, track: usize, hidden: bool });
/// Frames per second as the exact fraction `num / den`.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct FrameRateParam {
    pub num: u32,
    pub den: u32,
}

params!(ProjectOutputParams { expected_revision: u64, width: Option<u32>, height: Option<u32>, frame_rate: Option<FrameRateParam> });
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
    fill: Option<PaintParam>,
    outlines: Option<Vec<LayerOutlineParam>>,
    shadows: Option<Vec<LayerShadowParam>>,
    inner_shadows: Option<Vec<InnerShadowParam>>,
    #[serde(default, deserialize_with = "double_option")]
    bloom: Option<Option<BloomParam>>,
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
    /// Every word must be in some field, ignoring case and accents; best matches first.
    filter: Option<String>,
    /// At most this many styles, after ranking; all when absent.
    limit: Option<NonZeroUsize>,
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
params!(BeatsDetectParams { expected_revision: u64, clip: ClipParam, #[serde(default)] retry: bool });
params!(EffectAddParams { expected_revision: u64, clip: ClipParam, package: String, #[serde(default)] params: Map<String, Value> });
params!(EffectRemoveParams { expected_revision: u64, clip: ClipParam, effect: usize });
params!(EffectRegionParams { expected_revision: u64, clip: ClipParam, effect: usize, shape: Option<String>, cx: f64, cy: f64, w: f64, h: f64, angle: Option<f64>, feather: Option<f64>, track_id: Option<u32>, at: Option<i64>, #[serde(default)] follow_scale: bool, #[serde(default)] follow_rotation: bool, #[serde(default)] perspective: bool });
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

/// The method of clip_set_denoise.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DenoiseModeParam {
    /// No noise reduction.
    Off,
    /// RNNoise, a network trained on speech: keeps voices, removes the rest.
    Voice,
    /// Spectral gating of steady broadband noise (hiss, fans), for any material.
    General,
}

/// The model of clip_set_matte.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum MatteModelParam {
    /// No background removal.
    Off,
    /// MODNet webcam: finer edges, slower.
    Quality,
    /// PP-HumanSeg V2 Lite: coarser edges, fast.
    Fast,
}

params!(MatteParams { expected_revision: u64, clip: ClipParam, model: Option<MatteModelParam>, threshold: Option<f64>, feather: Option<f64>, edge: Option<f64>, invert: Option<bool>, temporal: Option<u32> });

fn clip_set_matte(p: MatteParams) -> Result<AgentRequest, AgentError> {
    // What is left out keeps the clip's current value.
    let model = p.model.map(|model| match model {
        MatteModelParam::Off => engine::MatteModel::Off,
        MatteModelParam::Quality => engine::MatteModel::Quality,
        MatteModelParam::Fast => engine::MatteModel::Fast,
    });
    edit(p.expected_revision, AgentEdit::ClipSetMatte { clip: p.clip.into(), model, threshold: p.threshold, feather: p.feather, edge: p.edge, invert: p.invert, temporal: p.temporal })
}

params!(PathParams {
    expected_revision: u64,
    clip: ClipParam,
    #[doc = "x = f(t): the offset across, in parts of the frame width. Empty or absent is 0."]
    x: Option<String>,
    #[doc = "y = g(t): the offset down, in parts of the frame height. Empty or absent is 0."]
    y: Option<String>,
    #[doc = "Degrees clockwise added to the rotation; may use dir. Absent: no turning."]
    rotation: Option<String>,
    #[doc = "Removes the path of the clip; x, y and rotation must then be absent."]
    #[serde(default)]
    remove: bool
});

fn clip_set_path(p: PathParams) -> Result<AgentRequest, AgentError> {
    let path = if p.remove {
        if p.x.is_some() || p.y.is_some() || p.rotation.is_some() {
            return Err(AgentError::new("invalid_params", "remove takes no x, y or rotation"));
        }
        None
    } else {
        Some(engine::MotionPath::new(p.x.unwrap_or_default(), p.y.unwrap_or_default(), p.rotation.filter(|text| !text.trim().is_empty())))
    };
    edit(p.expected_revision, AgentEdit::ClipSetPath { clip: p.clip.into(), path })
}

/// The effect of clip_set_voice.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum VoiceEffectParam {
    None,
    /// Ring modulation at 50 Hz.
    Robot,
}

/// A ready-made voice of clip_set_voice.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum VoicePresetParam {
    /// 0 semitones, no effect.
    Normal,
    /// +9 semitones.
    Chipmunk,
    /// -6 semitones.
    Deep,
    /// 0 semitones, robot effect.
    Robot,
}

params!(DenoiseParams { expected_revision: u64, clip: ClipParam, mode: Option<DenoiseModeParam>, strength: Option<f64> });
params!(VoiceParams { expected_revision: u64, clip: ClipParam, preset: Option<VoicePresetParam>, semitones: Option<f64>, effect: Option<VoiceEffectParam> });

fn clip_set_denoise(p: DenoiseParams) -> Result<AgentRequest, AgentError> {
    // What is left out keeps the clip's current value.
    let mode = p.mode.map(|mode| match mode {
        DenoiseModeParam::Off => engine::DenoiseMode::Off,
        DenoiseModeParam::Voice => engine::DenoiseMode::Voice,
        DenoiseModeParam::General => engine::DenoiseMode::General,
    });
    edit(p.expected_revision, AgentEdit::ClipSetDenoise { clip: p.clip.into(), mode, strength: p.strength })
}

fn clip_set_voice(p: VoiceParams) -> Result<AgentRequest, AgentError> {
    // A preset gives both values first; semitones or effect, when given, override it, and what
    // neither gives keeps the clip's current value.
    let preset = p.preset.map(|preset| {
        match preset {
            VoicePresetParam::Normal => engine::VoicePreset::Normal,
            VoicePresetParam::Chipmunk => engine::VoicePreset::Chipmunk,
            VoicePresetParam::Deep => engine::VoicePreset::Deep,
            VoicePresetParam::Robot => engine::VoicePreset::Robot,
        }
        .voice()
    });
    let effect = p.effect.map(|effect| match effect {
        VoiceEffectParam::None => engine::VoiceEffect::None,
        VoiceEffectParam::Robot => engine::VoiceEffect::Robot,
    });
    let semitones = p.semitones.or(preset.map(|voice| voice.semitones));
    let effect = effect.or(preset.map(|voice| voice.effect));
    edit(p.expected_revision, AgentEdit::ClipSetVoice { clip: p.clip.into(), semitones, effect })
}

params!(MutedParams { expected_revision: u64, clip: ClipParam, muted: bool });
params!(ReverseParams { expected_revision: u64, clip: ClipParam, reverse: bool });
params!(MarkerAddParams { expected_revision: u64, time: i64, label: Option<String>, color: Option<[u8; 4]> });
params!(MarkerParams { expected_revision: u64, marker: usize });
params!(MarkerMoveParams { expected_revision: u64, marker: usize, time: i64 });
params!(FadesParams { expected_revision: u64, clip: ClipParam, fade_in: i64, fade_out: i64 });
params!(TransformParams { expected_revision: u64, clip: ClipParam, placement: Option<PlacementParam>, opacity: Option<f64>, rotation: Option<f64> });
params!(KeyframeSetParams { expected_revision: u64, clip: ClipParam, property: Option<ClipPropertyParam>, effect: Option<usize>, param: Option<String>, at: i64, value: f64, easing: Option<EasingParam> });
params!(KeyframeRemoveParams { expected_revision: u64, clip: ClipParam, property: Option<ClipPropertyParam>, effect: Option<usize>, param: Option<String>, at: i64 });
params!(RevisionParams { expected_revision: u64 });
params!(CensorAddParams { expected_revision: u64, track: Option<usize>, start: i64, duration: i64, placement: Option<PlacementParam>, aspect: Option<f64>, shape: Option<String>, feather: Option<f64>, mode: Option<String>, strength: Option<f64>, color: Option<[u8; 4]> });
params!(LockSetParams { expected_revision: u64, clip: ClipParam, track_id: u32, at: Option<i64>, #[serde(default)] lock_scale: bool, #[serde(default)] lock_rotation: bool, zoom: Option<f64>, smooth: Option<f64>, #[serde(default)] perspective: bool });
params!(FollowSetParams { expected_revision: u64, clip: ClipParam, track_id: u32, reference: ClipParam, at: Option<i64>, #[serde(default)] follow_scale: bool, #[serde(default)] follow_rotation: bool, smooth: Option<f64>, #[serde(default)] perspective: bool });
params!(TrackSampleParams { expected_revision: u64, track_id: Option<u32>, clip: Option<ClipParam>, source_time: i64, cx: Option<f64>, cy: Option<f64>, w: Option<f64>, h: Option<f64>, angle: Option<f64>, confidence: Option<f64>, quad: Option<[[f64; 2]; 4]> });
params!(ExportStartParams { out: PathBuf, #[serde(default)] overwrite: bool, codec: Option<String>, encoder: Option<String> });
params!(ExportJobParams { job: Option<u64> });
params!(TrackRunParams { expected_revision: u64, track_id: u32, from: Option<i64>, direction: Option<String>, rotation: Option<bool>, mode: Option<String> });
params!(TrackJobParams { job: Option<u64> });
params!(CensorFacesParams { expected_revision: u64, clip: ClipParam, shape: Option<String>, feather: Option<f64>, mode: Option<String>, strength: Option<f64>, color: Option<[u8; 4]> });
params!(TrackCreateParams { expected_revision: u64, clip: ClipParam, at: Option<i64>, cx: Option<f64>, cy: Option<f64>, w: Option<f64>, h: Option<f64>, angle: Option<f64>, quad: Option<[[f64; 2]; 4]> });
params!(TrackSmoothParams { expected_revision: u64, track_id: u32, smooth: f64 });
params!(FrameCaptureParams { time: Option<i64>, frame: Option<i64>, max_width: Option<u32> });
params!(TransitionPreviewParams {
    #[doc = "The transition package, as packages_list names it."] id: String,
    #[doc = "frames (default): PNG pictures; video: an MP4 file in the cache."] format: Option<PreviewFormatParam>,
    #[doc = "How many pictures, 2 to 24 (default 6)."] frames: Option<u32>,
    #[doc = "Long edge: 144 (default, square), 360 or 720 (16:9)."] size: Option<u32>,
});
params!(AssetPreviewParams {
    #[doc = "A sticker source from asset_list, asset:sticker/<id>/<file>."] source: String,
    #[doc = "frames (default): PNG pictures; video: an MP4 file (animated stickers only)."] format: Option<PreviewFormatParam>,
    #[doc = "How many pictures of an animated sticker, 2 to 24 (default 6)."] frames: Option<u32>,
    #[doc = "Most pixels on the long edge, 1 to 720 (default 360); never more than the sticker has."] size: Option<u32>,
});

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

/// The shape of a censor region by name.
fn censor_shape(shape: Option<&str>) -> Result<Option<engine::RegionShape>, AgentError> {
    match shape {
        None => Ok(None),
        Some("rect") => Ok(Some(engine::RegionShape::Rect)),
        Some("ellipse") => Ok(Some(engine::RegionShape::Ellipse)),
        Some(other) => Err(AgentError::new("invalid_params", format!("shape {other:?}: rect or ellipse"))),
    }
}

/// The mode of a censor region by name, with its strength and colour; `None` when nothing is given.
fn censor_mode(mode: Option<&str>, strength: Option<f64>, color: Option<[u8; 4]>) -> Result<Option<engine::CensorMode>, AgentError> {
    match mode {
        None if strength.is_none() && color.is_none() => Ok(None),
        None | Some("blur") => Ok(Some(engine::CensorMode::Blur { radius: strength.unwrap_or(30.0) })),
        Some("pixelate") => Ok(Some(engine::CensorMode::Pixelate { block: strength.unwrap_or(24.0) })),
        Some("solid") => Ok(Some(engine::CensorMode::Solid { color: color.unwrap_or([0, 0, 0, 255]) })),
        Some(other) => Err(AgentError::new("invalid_params", format!("mode {other:?}: blur, pixelate or solid"))),
    }
}

fn censor_add(p: CensorAddParams) -> Result<AgentRequest, AgentError> {
    let shape = censor_shape(p.shape.as_deref())?.unwrap_or(engine::RegionShape::Rect);
    let mode = censor_mode(p.mode.as_deref(), p.strength, p.color)?.unwrap_or_default();
    let defaults = engine::CensorContent::default();
    let censor = engine::CensorContent { shape, aspect: p.aspect.unwrap_or(defaults.aspect), feather: p.feather.unwrap_or(defaults.feather), mode };
    edit(p.expected_revision, AgentEdit::CensorAdd { track: p.track, start: ticks(p.start), duration: ticks(p.duration), placement: p.placement.map(Into::into), censor })
}

/// A sample placed by hand: its four corners `quad` (the box then comes from them), or the box
/// `cx`, `cy`, `w`, `h`, which must then all be given.
#[allow(clippy::too_many_arguments)]
fn hand_sample(source_time: engine::Time, cx: Option<f64>, cy: Option<f64>, w: Option<f64>, h: Option<f64>, angle: Option<f64>, confidence: f64, quad: Option<[[f64; 2]; 4]>) -> Result<engine::TrackSample, AgentError> {
    if let Some(quad) = quad {
        // The box is worked out from the corners when the track's picture is known.
        return Ok(engine::TrackSample { source_time, cx: 0.5, cy: 0.5, w: 0.0, h: 0.0, angle: 0.0, confidence, manual: true, quad: Some(quad) });
    }
    match (cx, cy, w, h) {
        (Some(cx), Some(cy), Some(w), Some(h)) => Ok(engine::TrackSample { source_time, cx, cy, w, h, angle: angle.unwrap_or(0.0), confidence, manual: true, quad: None }),
        _ => Err(AgentError::new("invalid_params", "give cx, cy, w and h, or quad")),
    }
}

fn track_set_sample(p: TrackSampleParams) -> Result<AgentRequest, AgentError> {
    let sample = hand_sample(ticks(p.source_time), p.cx, p.cy, p.w, p.h, p.angle, p.confidence.unwrap_or(1.0), p.quad)?;
    edit(p.expected_revision, AgentEdit::TrackSetSample { track_id: p.track_id, clip: p.clip.map(Into::into), sample })
}

fn track_create(p: TrackCreateParams) -> Result<AgentRequest, AgentError> {
    let sample = hand_sample(engine::Time::ZERO, p.cx, p.cy, p.w, p.h, p.angle, 1.0, p.quad)?;
    edit(p.expected_revision, AgentEdit::TrackCreate { clip: p.clip.into(), at: p.at.map(ticks), sample })
}

fn text_add(p: TextAddParams) -> Result<AgentRequest, AgentError> {
    let style = p.style.unwrap_or_default().patch(None)?;
    edit(p.expected_revision, AgentEdit::TextAdd { track: p.track, start: ticks(p.start), duration: ticks(p.duration), content: p.content, style })
}

fn text_set(p: TextSetParams) -> Result<AgentRequest, AgentError> {
    let style = TextStyleParam { font: p.font, size: p.size, color: p.color, outline: p.outline, background: p.background, shadow: p.shadow, glow: p.glow, fill: p.fill, outlines: p.outlines, shadows: p.shadows, inner_shadows: p.inner_shadows, bloom: p.bloom, align: p.align, line_spacing: p.line_spacing, weight: p.weight, italic: p.italic, transform: p.transform, letter_spacing: p.letter_spacing, animation: p.animation, karaoke: p.karaoke, style_id: p.style_id };
    edit(p.expected_revision, AgentEdit::TextSet { clip: p.clip.into(), patch: style.patch(p.content)? })
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

fn transition_preview(p: TransitionPreviewParams) -> Result<AgentRequest, AgentError> {
    let frames = previews::check_frames(p.frames.unwrap_or(previews::DEFAULT_FRAMES))?;
    let size = p.size.unwrap_or(previews::TRANSITION_SIZES[0]);
    previews::transition_spec(size)?;
    Ok(AgentRequest::TransitionPreview { id: p.id, format: p.format.map_or(PreviewFormat::Frames, Into::into), frames, size })
}

fn asset_preview(p: AssetPreviewParams) -> Result<AgentRequest, AgentError> {
    let frames = previews::check_frames(p.frames.unwrap_or(previews::DEFAULT_FRAMES))?;
    let size = p.size.unwrap_or(previews::ASSET_DEFAULT_SIZE);
    if size == 0 || size > previews::ASSET_MAX_SIZE {
        return Err(AgentError::new("invalid_params", format!("size {size}: from 1 to {}", previews::ASSET_MAX_SIZE)));
    }
    Ok(AgentRequest::AssetPreview { source: p.source, format: p.format.map_or(PreviewFormat::Frames, Into::into), frames, size })
}

/// Every tool. Times are integer ticks of 1/90000 s.
pub fn all_tools() -> Vec<ToolDef> {
    vec![
        tool!("session_status", "The mode, project file, revision, and whether an export or a drag is going on.", NoParams, |_| Ok(AgentRequest::SessionStatus)),
        tool!("project_new", "Replaces the project with an empty one. Refused with unsaved_changes unless discard_changes is true; in the open window (live mode) unsaved work is always kept, whatever discard_changes says.", ProjectNewParams, |p| Ok(AgentRequest::ProjectNew { discard_changes: p.discard_changes })),
        tool!("project_open", "Opens a project file. Refused with unsaved_changes unless discard_changes is true; in the open window (live mode) unsaved work is always kept, whatever discard_changes says.", ProjectOpenParams, |p| Ok(AgentRequest::ProjectOpen { path: p.path, discard_changes: p.discard_changes })),
        tool!("project_save", "Saves the project, to path or to its own file. Another existing file is refused with file_exists unless overwrite is true; a source, package or the running export's output is always refused.", ProjectSaveParams, |p| Ok(AgentRequest::ProjectSave { path: p.path, overwrite: p.overwrite })),
        tool!("project_get", "The project JSON, its duration and frame count. Clips are named by {track, clip} indexes, which change after an edit.", NoParams, |_| Ok(AgentRequest::ProjectGet)),
        tool!("packages_list", "Effect and transition packages, filtered by a text (name, group, package, description or tag; best first). When the editor shows a language other than English and the pack translates an item into it, the item also has translation: {language, name, description, category, tags}, with only the translated fields; the English fields and the ids stay as they are. A package translation may also have params: {<param name>: {label, description}}. filter also matches the translated names, tags and descriptions.", PackagesListParams, |p| Ok(AgentRequest::PackagesList { filter: p.filter, limit: p.limit.map(NonZeroUsize::get) })),
        tool!("list_packages", "The installed packs, read only, as the package manager lists them: kind (effect, transition, text-style, subtitle-style, clip-animation, sticker, sfx, music, font), key, name, version, source (shipped or user), items, bytes, enabled, hidden (an effect kept out of the effect lists), shadowed (another root's pack of the same key is used) and problem (why a broken pack cannot be used). kind limits the list to one kind. A pack turned off is not loaded by the editor, the agent or an export. When the editor shows a language other than English and the pack translates an item into it, the item also has translation: {language, name, description, category, tags}, with only the translated fields; the English fields and the ids stay as they are.", ListPackagesParams, |p| Ok(AgentRequest::InstalledPacks { kind: p.kind })),
        tool!("track_add", "Adds an empty track on top; returns its index. kind is video (video files and images), audio (sounds, and video files for their sound), sticker (images and stickers) or text (text clips only).", TrackAddParams, |p| edit(p.expected_revision, AgentEdit::TrackAdd { kind: p.kind.into() })),
        tool!("track_set_locked", "Locks or unlocks a track. Edits of a locked track fail with track_locked.", TrackLockedParams, |p| edit(p.expected_revision, AgentEdit::TrackSetLocked { track: p.track, locked: p.locked })),
        tool!("project_set_output", "Sets the output frame size in pixels (2 to 7680 a side, an odd side is rounded down to even) and the frame rate as an exact fraction {num, den} of frames per second, 1 to 60 fps: {num: 30, den: 1} is 30 fps, {num: 24000, den: 1001} is 23.976, {num: 30000, den: 1001} is 29.97 and {num: 60000, den: 1001} is 59.94. Give at least one; the others stay. Returns the output now in use. Text size and clip placement are parts of the frame, so they follow the new size: a vertical frame needs smaller text.", ProjectOutputParams, |p| edit(p.expected_revision, AgentEdit::ProjectSetOutput { width: p.width, height: p.height, frame_rate: p.frame_rate.map(|rate| engine::FrameRate { num: rate.num, den: rate.den }) })),
        tool!("track_set_hidden", "Hides or shows a track in picture and sound.", TrackHiddenParams, |p| edit(p.expected_revision, AgentEdit::TrackSetHidden { track: p.track, hidden: p.hidden })),
        tool!("clip_add", "Adds a source file, a still image (png, jpg, webp, on a video or sticker track), or a file of an installed pack as asset:<type>/<id>/<file> (type sfx, music or sticker; a sticker goes on a sticker track only), as a clip at start (ticks). duration defaults to what the stream the track plays still holds at that speed, and to 5 seconds for an image, whose end can be set freely. speed defaults to 1, between 0.1 and 10, and does not apply to an image. Returns the new clip.", ClipAddParams, |p| edit(p.expected_revision, AgentEdit::ClipAdd { track: p.track, source: p.source, start: ticks(p.start), source_in: p.source_in.map(ticks), duration: p.duration.map(ticks), speed: p.speed })),
        tool!("text_add", "Adds a text clip of content at start (ticks) for duration, on track, which must be a text track, or, when track is absent, on the first text track with room or a new one. style sets any of font, size (parts of the output height, 0.08 by default), color, outline, background, shadow, glow, fill, outlines, shadows, inner_shadows, bloom, align, line_spacing, weight, italic, transform, letter_spacing and animation; what is absent is as the Text tab's new text: white, centred, in the default font (the system UI font, such as Segoe UI). It is placed at its natural size in the middle of the frame, which clip_set_transform can change. Returns the new clip.", TextAddParams, text_add),
        tool!("text_set", "Changes a text clip: give content and any style field of text_add; what is absent stays, and null takes an outline, background, shadow, glow, karaoke or an animation group off. outline and shadow change only the first outline or shadow of a text with several (null takes them all off); color fills the text with one colour, replacing a gradient or texture fill. The layered look goes through fill (solid, gradient or texture paint), outlines, shadows and inner_shadows, each list replacing all the layers of its kind ([] takes them off), and bloom (null takes it off); paints of outlines and shadows are the same kinds. A texture picture is a pack picture or an absolute local file. color with fill, outline with outlines, or shadow with shadows is refused. project_get shows every layer of a text. karaoke sets how the spoken word is marked (mode none, color, box or scale; looks for active, spoken, upcoming and emphasis words) and may give each word its time in source ticks; without times the words share the cue by their characters. Where the text is placed keeps the scale it was given around the same centre, so a bigger size or longer content draws bigger. Refused with not_text for a clip that is not text.", TextSetParams, text_set),
        tool!("font_list", "The fonts a text can name, each as the font value to give: the default font first (the system UI font, such as Segoe UI), then the installed families, then the fonts of the installed font packs (kind pack, with their name and pack_name). filter keeps the fonts whose family, name or pack match it, best first. state is loading while the installed fonts are still being read; ask again then.", FontListParams, |p| Ok(AgentRequest::FontList { filter: p.filter, limit: p.limit.map(NonZeroUsize::get) })),
        tool!("clip_animation_list", "The preset animations a clip with a picture (video, image, sticker or text) can name: each with its id, its group (in, out or loop) and default_duration in ticks (for loop, one cycle); a pack preset also has its name, pack and applies_to, and effect: null or {package, available}, the gpufx effect it runs while it plays and whether that package is installed (a missing one is left out and the preset plays only its motion). per_unit: true marks a preset that moves the characters, words or lines of a text one by one, such as typewriter; it is for text clips only. Only an id of the group is accepted there. filter keeps the presets whose id, name, group or pack match it, best first. When the editor shows a language other than English and the pack translates an item into it, the item also has translation: {language, name, description, category, tags}, with only the translated fields; the English fields and the ids stay as they are. Its pack has its own translation. filter also matches the translated names and tags.", ClipAnimationListParams, |p| Ok(AgentRequest::ClipAnimationList { filter: p.filter, limit: p.limit.map(NonZeroUsize::get) })),
        tool!("asset_list", "The items of the installed music, sfx and sticker packs, filtered by type (music, sfx or sticker; all when absent) and by filter (every word must be in the name, category or a tag, or the pack name; best first). Each has its type, the source to give clip_add (music and sfx go on an audio track, a sticker on a sticker track), name, category and tags; a sound also has duration in ticks (from its pack, may be absent) and loop, a sticker its size in pixels. state is loading while the packs are still being read. When the editor shows a language other than English and the pack translates an item into it, the item also has translation: {language, name, description, category, tags}, with only the translated fields; the English fields and the ids stay as they are. filter also matches the translated names, categories and tags, and the translated pack names.", AssetListParams, |p| Ok(AgentRequest::AssetList { kind: p.r#type, filter: p.filter, limit: p.limit.map(NonZeroUsize::get) })),
        tool!("text_style_list", "The styles of the installed text style and subtitle style packs, filtered by type (text or subtitle; both when absent) and by filter (name, category, id or pack; best first): each with its id to give as style_id to text_add (in style) or text_set, its name, type, category, preview_text, pack, the style values it copies into a text (absent ones are as on a new text; an absent size keeps the text's), its animations (durations in ticks) and, for a subtitle style, its layout (anchor_y and max_width in parts of the frame). font_hint names fonts used when installed. state is loading while the packs are still being read. When the editor shows a language other than English and the pack translates an item into it, the item also has translation: {language, name, description, category, tags}, with only the translated fields; the English fields and the ids stay as they are. Its pack has its own translation. filter also matches the translated names, categories and tags.", TextStyleListParams, |p| Ok(AgentRequest::TextStyleList { kind: p.kind.map(|kind| match kind { StyleTypeParam::Text => "text".to_owned(), StyleTypeParam::Subtitle => "subtitle".to_owned() }), filter: p.filter, limit: p.limit.map(NonZeroUsize::get) })),
        tool!("clip_animation_set", "Sets the preset animations of a clip with a picture (video, image, sticker or text): in plays as it appears, out as it goes away, loop the whole time it shows. Each is {preset, duration in ticks}, with a preset clip_animation_list names for that group; a group that is absent stays, and null takes its animation off. A negative duration on in or out is a hold: a still of the first (in) or last (out) source frame shown that long before or after the moving part, silent, with the animation playing over it; the clip gets that much longer and every later clip of its track moves by as much (changing or removing it moves them back). A hold removes a transition on the same cut; removed_transitions in the result lists them as {track, after_clip, package}. Refused with invalid_params for a clip on an audio track.", ClipAnimationSetParams, clip_animation_set),
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
        tool!("transition_add", "Adds a transition between clip after_clip and the next, which must touch. A hold (negative in or out animation) on that cut becomes a plain animation of the same length, and later clips move back by it; released_holds in the result lists them as {clip, group: in or out}.", TransitionAddParams, |p| edit(p.expected_revision, AgentEdit::TransitionAdd { track: p.track, after_clip: p.after_clip, package: p.package, duration: p.duration.map(ticks), params: p.params })),
        tool!("transition_remove", "Removes the transition after a clip.", TransitionRemoveParams, |p| edit(p.expected_revision, AgentEdit::TransitionRemove { track: p.track, after_clip: p.after_clip })),
        tool!("clip_set_speed", "Sets the playback speed of a clip, 0.1 to 10; 2 plays twice as fast. Keeps the source range, so the timeline length becomes source length / speed. Audio keeps its pitch. Refused with speed_overlap when the clip would run into the next one, and with transition_on_edge when its end holds a transition.", SpeedParams, |p| edit(p.expected_revision, AgentEdit::ClipSetSpeed { clip: p.clip.into(), speed: p.speed })),
        tool!("clip_set_gain", "Sets the gain of a clip in decibels.", GainParams, |p| edit(p.expected_revision, AgentEdit::ClipSetGain { clip: p.clip.into(), gain_db: p.gain_db })),
        tool!("clip_set_muted", "Mutes or unmutes a clip.", MutedParams, |p| edit(p.expected_revision, AgentEdit::ClipSetMuted { clip: p.clip.into(), muted: p.muted })),
        tool!("clip_set_reverse", "Plays a clip backwards (reverse true) or forwards again. The clip keeps its place, length, source range and keyframes; its frames and its sound play from the end of that range to its start. Refused on an image or text clip (reverse_of_still).", ReverseParams, |p| edit(p.expected_revision, AgentEdit::ClipSetReverse { clip: p.clip.into(), reverse: p.reverse })),
        tool!("clip_set_fades", "Sets the audio fades of a clip, in ticks.", FadesParams, |p| edit(p.expected_revision, AgentEdit::ClipSetFades { clip: p.clip.into(), fade_in: ticks(p.fade_in), fade_out: ticks(p.fade_out) })),
        tool!("clip_set_denoise", "Sets the noise reduction of a clip's sound: mode off, voice (RNNoise, keeps speech and removes everything else) or general (removes steady broadband noise such as hiss or fans from any material), and strength from 0 to 1. Either may be left out, which keeps the clip's current value (a clip never set has mode off and strength 1). It works on the source audio before speed, pitch, gain and fades. Refused with out_of_range for a strength outside 0 to 1.", DenoiseParams, clip_set_denoise),
        tool!("clip_set_matte", "Sets the AI background removal of a video or image clip (the person is kept, the background becomes transparent so lower tracks show through): model off, quality (MODNet, finer edges) or fast (PP-HumanSeg lite); threshold 0 to 1 (0 keeps the soft matte, above 0 makes it hard at that level); feather 0 to 20 and edge -20 (shrink) to 20 (grow), in pixels of a 1080-line output; invert (keep the background instead); temporal 0 to 4 (average the matte over that many earlier source frames, off when 0). Each may be left out, which keeps the clip's current value. Ignored on text clips. A model that is not installed leaves the clip drawn without its matte. Refused with out_of_range for a value outside its range.", MatteParams, clip_set_matte),
        tool!("clip_set_path", "Moves a clip along a path given as equations, or removes its path (remove true). x = f(t) and y = g(t) are offsets in parts of the frame width and height, laid on top of the clip's place and X/Y keyframes (which stay where the path starts from); rotation, optional, is clockwise degrees added to its rotation. Variables: t (seconds since the start of the path, which is the clip start for a new path; after a split or a start trim it goes on from where the cut was), d (the span of the path in seconds: the clip length for a new path, kept as it was by splits and trims), p (t / d, 0 to 1); rotation also has dir, the heading of the path on screen in the same degrees (0 is to the right, 90 down), so rotation \"dir\" turns the clip along its path. A path set here starts again from the clip start. Numbers, + - * / ^, parentheses, pi, tau, e and the functions sin cos tan asin acos atan atan2 abs sign min max clamp mix step smoothstep pow sqrt exp log floor ceil round fract. A value that is not finite counts as 0. Example circle: x \"0.2*cos(tau*p)\", y \"0.2*sin(tau*p)\". Refused with invalid_expression when an expression does not parse.", PathParams, clip_set_path),
        tool!("clip_set_voice", "Sets the voice change of a clip: a pitch shift in semitones (-12 to 12, the speed stays) and an effect (none or robot). preset (normal: 0, chipmunk: +9, deep: -6, robot: 0 with the robot effect) gives both first; semitones or effect, when given, override it, and what neither gives keeps the clip's current value. Refused with out_of_range for semitones beyond 12 either way.", VoiceParams, clip_set_voice),
        tool!("clip_set_transform", "Sets the static place (x, y, width, all in parts of the frame), opacity (0 to 1) and rotation (clockwise degrees) of a clip. What is absent stays. A number with keyframes ignores its static value.", TransformParams, |p| edit(p.expected_revision, AgentEdit::ClipSetTransform { clip: p.clip.into(), placement: p.placement.map(Into::into), opacity: p.opacity, rotation: p.rotation })),
        tool!("keyframe_set", "Sets a keyframe at `at`, ticks from the start of the clip (clip.start, not the clip_time of an effect), from 0 up to but not including the duration. Give property (x, y, width, opacity, rotation), or effect (its index) and param (a float parameter of that effect). Replaces one at the same tick; easing (linear, ease_in, ease_out, ease_in_out) is how the value moves to the next keyframe, and absent keeps the one replaced. Refused with keyframe_outside_clip.", KeyframeSetParams, |p| { let target = keyframe_target(p.property, p.effect, p.param)?; edit(p.expected_revision, AgentEdit::KeyframeSet { clip: p.clip.into(), target, at: ticks(p.at), value: p.value, easing: p.easing.map(Into::into) }) }),
        tool!("keyframe_remove", "Removes the keyframe at `at` of a property, or of a parameter of an effect, as in keyframe_set, also one that lies outside the clip. After the last one inside the clip the static value applies again. Refused with keyframe_not_found.", KeyframeRemoveParams, |p| { let target = keyframe_target(p.property, p.effect, p.param)?; edit(p.expected_revision, AgentEdit::KeyframeRemove { clip: p.clip.into(), target, at: ticks(p.at) }) }),
        tool!("beats_detect", "Detects the beats of the sound of a clip (tempo by autocorrelation of a spectral-flux onset envelope, beats by dynamic programming, downbeats every 4 beats) and stores them for the clip's source file in the time of that source, shared by every clip of the file, so they follow the clips through moves, trims, splits and speed changes; one undo step (none when the stored beats are already the same), and they become snap targets in the editor. Returns state done with tempo (BPM of the source), count and beats: the beats the clip plays, each {at: timeline ticks, downbeat}. Without a window it waits; in the open window (live mode) detection runs in the background and the call answers state running with progress (0 to 1) and no edit: call again with the then current revision until it says done. A file is analysed once per version; detecting again is instant. Calling again after done returns the stored result and changes nothing while the stored beats are the same (no undo step, the redo stack stays). Refused with invalid_params for a still or text clip and detect_failed when the sound cannot be read; a failure is remembered for that version of the file and every later call reports it without decoding again, unless retry is true, which starts a new detection.", BeatsDetectParams, |p| Ok(AgentRequest::BeatsDetect { expected_revision: p.expected_revision, clip: p.clip.into(), retry: p.retry })),
        tool!("clip_beats_clear", "Takes the beats off the source of a clip, so off every clip of that file. One undo step.", ClipOnlyParams, |p| edit(p.expected_revision, AgentEdit::ClipBeatsClear { clip: p.clip.into() })),
        tool!("marker_add", "Adds a marker on the ruler at time (timeline ticks, not before 0), with an optional label (at most 200 characters) and color [r, g, b, a]. Markers stay where they are when clips move, and they are snap targets in the editor. Returns marker, its index: markers are sorted by time, so indexes change as markers are added, moved or removed. One undo step.", MarkerAddParams, |p| edit(p.expected_revision, AgentEdit::MarkerAdd { at: ticks(p.time), label: p.label, color: p.color })),
        tool!("marker_remove", "Removes a marker by index (markers_list). Refused with marker_not_found.", MarkerParams, |p| edit(p.expected_revision, AgentEdit::MarkerRemove { marker: p.marker })),
        tool!("marker_move", "Moves a marker to time (timeline ticks); returns marker, the index it now has. Refused with marker_not_found.", MarkerMoveParams, |p| edit(p.expected_revision, AgentEdit::MarkerMove { marker: p.marker, at: ticks(p.time) })),
        tool!("markers_list", "The markers of the ruler, each {marker: index, at: timeline ticks, label, color}, and the beats of every clip whose source has some: {clip, source, tempo, beats: [{at: timeline ticks, downbeat}]}, only those the clip plays.", NoParams, |_| Ok(AgentRequest::MarkersList)),
        tool!("censor_add", "Adds a censor region: a box on a sticker track that hides what the tracks below show inside it, from start for duration (ticks), on track (a sticker track) or, when absent, on the first sticker track with room or a new one. placement (x, y, width in parts of the frame; the height follows from aspect, width over height in output pixels, 16/9 by default) defaults to a third of the frame wide in the middle; clip_set_transform and keyframe_set move it later. shape: rect (default) or ellipse. feather: soft edge, 0 to 1 of half the smaller side (0.2 by default). mode: blur (default; strength is the radius, 0 to 120, in pixels of a 1080-line frame, 30 by default), pixelate (strength is the block, 1 to 200, 24 by default) or solid (color [r, g, b, a], black by default). Returns the new clip.", CensorAddParams, censor_add),
        tool!("track_set_sample", "Adds or replaces (same source_time) a sample of a motion track: where an object is in the source file at source_time (ticks of the source, not of the timeline): middle cx, cy and size w, h in parts of the source picture, angle in clockwise degrees (0), or quad, four corners as in track_create, confidence 0 to 1 (1). Give track_id, or clip (a clip showing the file) to start a new track of that file with this sample. Tracks belong to the file, so every clip of it shares them. Returns track_id.", TrackSampleParams, track_set_sample),
        tool!("track_run", "Runs the object tracker on a motion track: from its sample at source time from (ticks of the source; by default the last sample placed by hand going forward, the first going backward) frame by frame through the file (its preview proxy when it has one), direction forward (default), backward, or both (forward, then backward from the same sample once the forward run is written; the answer then has backward, the state of the second job), until the next sample placed by hand, the end of the file, or a frame where the object is lost. Corners inside the box are followed by pyramidal Lucas-Kanade flow and a RANSAC similarity fit (move, scale and, with rotation true, the default, turn); a template search finds the object again after a short loss. mode fast (default) is that classical tracker on the CPU; mode robust runs NanoTrack, a Siamese network on the GPU that holds on through blur, deformation and short occlusions, keeps the box upright at the start sample's angle (rotation is ignored) and needs its model installed (else invalid_params). Every frame gets a sample with a confidence (0 to 1); a lost frame ends the run with a sample under 0.5, marked for a fix by hand: place a sample there with track_set_sample and run again. The samples are written as one undo step when the run ends, replacing earlier tracker samples in its span but never samples placed by hand. Without a window it waits and returns the outcome as track_status does; in the open window (live mode) it returns {job, state: running} at once. Refused with busy while the track is being tracked and invalid_params when there is no sample at from.", TrackRunParams, |p| Ok(AgentRequest::TrackRun { expected_revision: p.expected_revision, track_id: p.track_id, from: p.from.map(ticks), direction: p.direction, rotation: p.rotation, mode: p.mode })),
        tool!("track_status", "The state of a tracker job (the last one when job is absent): running with done and total frames, or done, lost, cancelled or failed with track_id, count (samples written) and last (the last sample, for lost the one to fix), and message for failed. A finished run's samples are written before it answers.", TrackJobParams, |p| Ok(AgentRequest::TrackStatus { job: p.job })),
        tool!("censor_faces", "Censors every face of a video clip automatically: the YuNet face detector runs on the part of the clip's file the clip shows (its preview proxy when it has one), at most 10 frames a second; the faces are joined frame to frame by overlap (IoU) into face tracks, dropping short or unsure ones; each face track becomes a motion track of the file and a censor region on a sticker track that follows it (scale included, smoothed), 1.3 times the face box, over the time the face is seen; all of it one undo step when the job ends. shape: ellipse (default) or rect. feather: soft edge, 0 to 1 (0.3 by default). mode: blur (default; strength is the radius, 0 to 120, 30 by default), pixelate (strength is the block, 1 to 200) or solid (color). Needs the face detector model installed (else invalid_params); refused with invalid_params for a clip that is not a video and busy while the clip's faces are being found. Without a window it waits and returns the outcome as censor_faces_status does; in the open window (live mode) it returns {job, state: running} at once.", CensorFacesParams, |p| Ok(AgentRequest::CensorFaces { expected_revision: p.expected_revision, clip: p.clip.into(), shape: censor_shape(p.shape.as_deref())?, feather: p.feather, mode: censor_mode(p.mode.as_deref(), p.strength, p.color)? })),
        tool!("censor_faces_status", "The state of a face censor job (the last one when job is absent): running with done and total frames, finishing (all frames done, the regions wait to be written while edits wait for a drag or an export), or done (faces: how many regions were added, track_ids: their motion tracks; 0 when no face was found), cancelled (nothing added), stale (the clip changed meanwhile: another file, moved, cut, trimmed, sped up or reversed; nothing added) or failed with message; done may carry a message when some faces could not be placed. A finished job's regions are written before it answers.", TrackJobParams, |p| Ok(AgentRequest::CensorFacesStatus { job: p.job })),
        tool!("censor_faces_cancel", "Stops a face censor job (the last one when job is absent); it adds nothing. Returns its state as censor_faces_status does.", TrackJobParams, |p| Ok(AgentRequest::CensorFacesCancel { job: p.job })),
        tool!("track_cancel", "Stops a tracker job (the last one when job is absent); the samples made so far are written. Returns its state as track_status does.", TrackJobParams, |p| Ok(AgentRequest::TrackCancel { job: p.job })),
        tool!("clip_follow_set", "Makes clip follow motion track track_id as it is seen through reference, a clip showing the tracked file: at timeline time at (reference's start when absent) the clip stands where it is placed, and from there it moves by how far the object moved, through the trim, speed, direction, place and transform of reference. follow_scale and follow_rotation (false) also scale and turn it with the object; smooth (0 to 2 seconds of source, 0) evens out the track. perspective (false) pins the clip's four corners to the object's four corners (corner pin, for a planar track; a track of boxes gives the corners of its boxes), scale and turn included. The clip's own keyframes add to the motion. A split, trim or delete of reference keeps the follow on the clip that shows the anchor, or drops it. Refused with invalid_project when the track, the reference or the anchor does not fit.", FollowSetParams, |p| edit(p.expected_revision, AgentEdit::ClipFollowSet { clip: p.clip.into(), track_id: p.track_id, reference: p.reference.into(), at: p.at.map(ticks), follow_scale: p.follow_scale, follow_rotation: p.follow_rotation, smooth: p.smooth.unwrap_or(0.0), perspective: p.perspective })),
        tool!("track_create", "Starts a new motion track of the file clip shows, with one box placed by hand: middle cx, cy and size w, h in parts of the source picture, angle in clockwise degrees (0); or, for a planar (four corner, perspective) track, quad: the four corners [[x, y] x4] in parts of the source picture, top-left, top-right, bottom-right, bottom-left of the flat object, convex (the box then comes from them; track_run in fast mode tracks the corners by homography), at the source time clip shows at timeline time at (ticks; clip's start when absent). Tracks belong to the file, so every clip of it shares them. Returns track_id and source_time; then run track_run on it.", TrackCreateParams, track_create),
        tool!("track_smooth", "Sets the smoothing of everything that follows motion track track_id (clips following it and effect regions linked to it): smooth seconds of source time, 0 (off) to 2, a Gaussian over source time applied when the track is read; the samples stay as they are.", TrackSmoothParams, |p| edit(p.expected_revision, AgentEdit::TrackSmooth { track_id: p.track_id, smooth: p.smooth })),
        tool!("track_bake_keyframes", "Turns the follow of clip, or its lock when the clip is locked to a track, into keyframes: X and Y at every output frame of the clip (Width too when it follows or keeps the scale or zooms, Rotation when it follows or keeps the turn), where it showed, replacing keyframes of those properties; then the follow or lock goes. One undo step. Refused with not_following when the clip neither follows nor is locked, and with bake_perspective when it follows in perspective (a corner pin has no keyframes)."
, ClipOnlyParams, |p| edit(p.expected_revision, AgentEdit::TrackBakeKeyframes { clip: p.clip.into() })),
        tool!("clip_follow_clear", "Stops a clip following a motion track; it stays where it is placed.", ClipOnlyParams, |p| edit(p.expected_revision, AgentEdit::ClipFollowClear { clip: p.clip.into() })),
        tool!("clip_lock_set", "Locks the frame of a video clip to motion track track_id of its own source (stabilize on the object): the whole picture moves against the track so the object stays where it is at timeline time at (the clip's start when absent), and the rest of the frame moves the other way. lock_scale and lock_rotation (false) also keep the object's size and angle, so the frame scales and turns against it; zoom (0.1 to 8, 1) scales the locked picture around the object to hide the revealed edges, which show what is under the clip; smooth (0 to 2 seconds of source, 0) evens out the track. perspective (false) holds a planar (four corner) track's corners still on screen by warping the picture against them (inverse corner pin), size and angle included whatever lock_scale and lock_rotation say; refused with invalid_project on a track without four corners, and such a lock cannot be baked (bake_perspective). A split or trim keeps the lock on every piece. Overlays that follow the track through this clip stay on the object. Refused with invalid_project when the track is of another file, the clip follows a track, or a number is out of range.", LockSetParams, |p| edit(p.expected_revision, AgentEdit::ClipLockSet { clip: p.clip.into(), track_id: p.track_id, at: p.at.map(ticks), lock_scale: p.lock_scale, lock_rotation: p.lock_rotation, zoom: p.zoom, smooth: p.smooth.unwrap_or(0.0), perspective: p.perspective })),
        tool!("effect_region_set", "Limits effect effect (index in the clip's effects) of a video clip to a region of its picture: only blur and pixelate effects (gaussian-blur, box-blur, pixelate) take one, else invalid_project. shape rect or ellipse (rect); cx, cy, w, h the middle and size in parts of the picture's width and height (before the clip is placed, so the region stays on the clip); angle clockwise degrees (0); feather 0 to 1 (0.2) the soft edge in parts of half the smaller side. track_id makes the region move with that motion track of the clip's own source, anchored at the source time shown at timeline time at (the clip's start when absent); follow_scale and follow_rotation (false) also grow and turn it with the object; perspective (false, with a planar four corner track) lays it on the object's corners in perspective. The track's smoothing applies. Replaces any region the effect had. effect_not_found for a bad index.", EffectRegionParams, |p| {
            let region = engine::EffectRegion { shape: censor_shape(p.shape.as_deref())?.unwrap_or_default(), cx: p.cx, cy: p.cy, w: p.w, h: p.h, angle: p.angle.unwrap_or(0.0), feather: p.feather.unwrap_or(0.2), link: None, perspective: false, corners: None };
            edit(p.expected_revision, AgentEdit::EffectRegionSet { clip: p.clip.into(), effect: p.effect, region, track_id: p.track_id, at: p.at.map(ticks), follow_scale: p.follow_scale, follow_rotation: p.follow_rotation, perspective: p.perspective })
        }),
        tool!("effect_region_clear", "Gives effect effect of a video clip the whole picture again (ends its region).", EffectRemoveParams, |p| edit(p.expected_revision, AgentEdit::EffectRegionClear { clip: p.clip.into(), effect: p.effect })),
        tool!("clip_lock_clear", "Ends the lock of a video clip; it goes back to where it is placed.", ClipOnlyParams, |p| edit(p.expected_revision, AgentEdit::ClipLockClear { clip: p.clip.into() })),
        tool!("undo", "Undoes the last edit.", RevisionParams, |p| Ok(AgentRequest::Undo { expected_revision: p.expected_revision })),
        tool!("redo", "Redoes an undone edit.", RevisionParams, |p| Ok(AgentRequest::Redo { expected_revision: p.expected_revision })),
        tool!("transition_preview", "What a transition looks like: format frames (default) gives frames (2 to 24, default 6) PNG pictures spread evenly over progress 0 to 1, with JSON giving the progress of each (one picture when the transition is static); format video gives an MP4 (20 fps, 24 frames) kept in a cache of the server, as its absolute path and a resource link. size is the long edge: 144 (default, the square of the effect picker, sharing its cache), 360 or 720 (16:9). A preview not cached yet is rendered first, which can take a while; a package that failed before answers preview_failed with the reason, an unknown id invalid_params. A render holds the server's next call until it ends (up to 1 to 3 minutes by size).", TransitionPreviewParams, transition_preview),
        tool!("asset_preview", "What a sticker of asset_list looks like (source asset:sticker/...): a still gives one PNG; an animated GIF or WebP gives frames (2 to 24, default 6) PNGs spread evenly over its loop, each with its time_ms, or with format video an MP4 (30 fps, one loop of at most 60 s, else animation_too_long; even sides of at least 32 pixels, so a sticker with a side under 32 answers invalid_params) kept in a cache, as its absolute path and a resource link. A sticker whose frames at that size take more than 256 MiB answers animation_too_large. size is the most pixels on the long edge, 1 to 720 (default 360), never more than the sticker has. A source that is not a sticker answers invalid_params.", AssetPreviewParams, asset_preview),
        tool!("frame_capture", "The composited frame at a time (ticks) or frame index, exactly one of them, as a PNG no wider than max_width (default 1280). A position outside the timeline is an error.", FrameCaptureParams, frame_capture),
        tool!("export_start", "Exports the project to an MP4 in the background; returns a job. Refused with file_exists unless overwrite is true. One export at a time; edits wait for it. codec: h264 (default) or hevc. encoder: auto (default; the first that opens, hardware first: *_nvenc, *_qsv, *_amf, *_mf, then libx264/libx265 in the GPL build, then libopenh264/libkvazaar) or an encoder name such as hevc_nvenc; a named encoder of another codec is refused with invalid_encoder.", ExportStartParams, |p| Ok(AgentRequest::ExportStart { out: p.out, overwrite: p.overwrite, codec: p.codec, encoder: p.encoder })),
        tool!("export_status", "The state of an export (the last one when job is absent): running with done and total, finished with its report, cancelled, or failed with a message.", ExportJobParams, |p| Ok(AgentRequest::ExportStatus { job: p.job })),
        tool!("export_cancel", "Asks the running export to stop; export_status tells when it has.", ExportJobParams, |p| Ok(AgentRequest::ExportCancel { job: p.job })),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_set_path_names_every_function_and_constant_of_the_expressions() {
        let tools = all_tools();
        let description = tools.iter().find(|tool| tool.name == "clip_set_path").expect("listed").description;
        let words: Vec<&str> = description.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        for name in engine::motion_path::function_names().chain(engine::motion_path::constant_names()) {
            assert!(words.contains(&name), "{name} is missing from the description");
        }
    }
}
