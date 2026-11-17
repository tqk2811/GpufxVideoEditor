//! Installing a text style pack from a `.zip`. No window here.
//!
//! A zip with `"type": "text-style"` is unpacked once to
//! `<user root>/packages/text-style/<id lower-cased>/`. It holds no font: only the manifest, whose
//! items are text styles, its licence files and, from format 2 on, the PNG and JPEG pictures its
//! layers are painted with. Applying a style copies its values into the clip (see
//! [`crate::text_style_library`]); its animations name presets of a clip animation pack, and its
//! texture pictures stay in the pack, named by [`TextImageRef::Pack`].
//!
//! Format 1 has one fill colour, one outline and one shadow. Format 2 adds the layers of a text
//! ([`TextContent::fill`], `outlines`, `shadows`, `inner_shadows`, in the project's own JSON), in
//! place of those three, the bloom ([`TextContent::bloom`]) and the pictures. A pack of format 1
//! that uses one of these fields is refused.
//!
//! A shared text-pack module for text and subtitle styles was considered; the user chose one module
//! per pack type. The subtitle style pack ([`crate::subtitle_style_install`]) reuses the style
//! schema of this one, since a subtitle style is a text style with a layout.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

use d3d12_backend::is_safe_id;
use engine::{AnimationGroup, Rgba, TextAlign, TextBackground, TextBloom, TextContent, TextGlow, TextImageRef, TextInnerShadow, TextOutline, TextPaint, TextShadow, TextTransform, TexturePackType};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::asset_install::{check_entry_extensions, check_item_ids, check_named_files, check_pack_header, clean_leftovers, lock_installs, rename_with_retries, unpack_into_place, AudioLicense, ExtractLimits, Unpacked, MANIFEST, MAX_COMPRESSION_RATIO, SUPPORTED_FORMAT};
use crate::package_import::{entries_of, read_manifest, validate_entries, ImportOutcome, ImportProblem, Limits, MIB};
use crate::text_clip::TEXT_SIZE_RANGE;

/// What the `type` of a text style pack says, and the folder it goes to.
pub const TEXT_STYLE_TYPE: &str = "text-style";
/// A pack is a manifest, a few licence files and, for a text style pack of format 2, the texture
/// pictures of its styles. A picture stays below what the rasterizer reads
/// ([`gpufx_text::MAX_TEXTURE_FILE_BYTES`]).
pub const STYLE_PACK_LIMITS: Limits = Limits { files: 256, total_bytes: 128 * MIB, file_bytes: 16 * MIB };
/// The format of a text style pack whose styles may have layers and pictures.
pub const LAYERED_STYLE_FORMAT: u32 = 2;
pub(crate) const STYLE_PACK_EXTRACT: ExtractLimits = ExtractLimits { limits: STYLE_PACK_LIMITS, max_ratio: MAX_COMPRESSION_RATIO };
/// The longest an animation of a style may last, in seconds.
pub const MAX_STYLE_ANIMATION_SECONDS: f64 = 60.0;
/// The most fonts a style may hint at.
pub const MAX_FONT_HINTS: usize = 16;
/// The longest a name, category, preview text or font hint may be, in characters.
pub const MAX_STYLE_TEXT_CHARS: usize = 200;
/// The largest line spacing a style may ask for.
pub const MAX_STYLE_LINE_SPACING: f32 = 10.0;

/// A text style pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTextStylePack {
    pub id: String,
    pub name: String,
    pub replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyleManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: AudioLicense,
    pub items: Vec<TextStyleManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyleManifestItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    /// What the style is shown on in the library; the name when absent.
    #[serde(default)]
    pub preview_text: Option<String>,
    pub style: PackTextStyle,
    #[serde(default)]
    pub animation: Option<PackStyleAnimation>,
}

/// The style of an item: the fields of [`TextContent`] but its content and font. A field that is
/// absent is as on a new text, except `size`, which then stays as the clip has it. A font is only
/// hinted at: the first family of `font_hint` installed on the machine draws it, else the clip keeps
/// its font. No font is shipped in a pack.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackTextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<PackOutline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<PackBackground>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<PackShadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<PackGlow>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_hint: Vec<String>,
    /// Format 2: what fills the glyphs, in place of `color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<TextPaint>,
    /// Format 2: the outlines, in place of `outline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outlines: Option<Vec<TextOutline>>,
    /// Format 2: the shadows, in place of `shadow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<Vec<TextShadow>>,
    /// Format 2: the inner shadows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_shadows: Option<Vec<TextInnerShadow>>,
    /// Format 2: the bloom, a light spread from the whole text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<TextBloom>,
}

/// [`TextOutline`], with no other field allowed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackOutline {
    pub color: Rgba,
    pub width: f32,
}

/// [`TextBackground`], with no other field allowed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackBackground {
    pub color: Rgba,
    pub padding: f32,
    pub radius: f32,
}

/// [`TextShadow`], with no other field allowed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackShadow {
    pub color: Rgba,
    pub offset: [f32; 2],
    pub blur: f32,
}

/// [`TextGlow`], with no other field allowed; `intensity` and `falloff` default as in a project.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackGlow {
    pub color: Rgba,
    #[serde(default)]
    pub outer_color: Option<Rgba>,
    pub radius: f32,
    #[serde(default)]
    pub intensity: Option<f32>,
    #[serde(default)]
    pub falloff: Option<f32>,
}

/// The animations a style sets, by group.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackStyleAnimation {
    #[serde(default, rename = "in")]
    pub enter: Option<PackAnimationRef>,
    #[serde(default, rename = "out")]
    pub exit: Option<PackAnimationRef>,
    #[serde(default, rename = "loop")]
    pub repeat: Option<PackAnimationRef>,
}

impl PackStyleAnimation {
    /// Each group with the animation the style gives it.
    pub fn groups(&self) -> [(AnimationGroup, Option<&PackAnimationRef>); 3] {
        [(AnimationGroup::Enter, self.enter.as_ref()), (AnimationGroup::Exit, self.exit.as_ref()), (AnimationGroup::Repeat, self.repeat.as_ref())]
    }
}

/// One animation: a built-in preset id, or `"<pack id>/<item id>"` of a clip animation pack.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackAnimationRef {
    pub preset: String,
    /// Seconds; for a loop, one cycle.
    pub duration: f64,
}

impl PackTextStyle {
    /// Whether the style uses a field of format 2.
    pub fn has_layers(&self) -> bool {
        self.fill.is_some() || self.outlines.is_some() || self.shadows.is_some() || self.inner_shadows.is_some() || self.bloom.is_some()
    }

    /// The style on a new text of `content` and `size`, before a font hint is looked up.
    pub fn text(&self, content: &str, size: f32) -> TextContent {
        let mut text = TextContent::new(content, self.size.unwrap_or(size));
        if let Some(color) = self.color {
            text.fill = TextPaint::solid(color);
        }
        if let Some(fill) = &self.fill {
            text.fill = fill.clone();
        }
        text.outlines = match &self.outlines {
            Some(outlines) => outlines.clone(),
            None => self.outline.map(|v| TextOutline::solid(v.color, v.width)).into_iter().collect(),
        };
        text.background = self.background.map(|v| TextBackground { color: v.color, padding: v.padding, radius: v.radius });
        text.shadows = match &self.shadows {
            Some(shadows) => shadows.clone(),
            None => self.shadow.map(|v| TextShadow::solid(v.color, v.offset, v.blur)).into_iter().collect(),
        };
        text.inner_shadows = self.inner_shadows.clone().unwrap_or_default();
        text.bloom = self.bloom;
        text.glow = self.glow.map(|v| {
            let mut glow = TextGlow { color: v.color, outer_color: v.outer_color, radius: v.radius, intensity: 1.0, falloff: 2.0 };
            glow.intensity = v.intensity.unwrap_or(glow.intensity);
            glow.falloff = v.falloff.unwrap_or(glow.falloff);
            glow
        });
        if let Some(align) = self.align {
            text.align = align;
        }
        if let Some(line_spacing) = self.line_spacing {
            text.line_spacing = line_spacing;
        }
        if let Some(weight) = self.weight {
            text.weight = weight;
        }
        text.italic = self.italic.unwrap_or(false);
        text.transform = self.transform.unwrap_or_default();
        text.letter_spacing = self.letter_spacing.unwrap_or(0.0);
        text
    }
}

fn short_text(what: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if value.chars().count() > MAX_STYLE_TEXT_CHARS {
        return Err(format!("{what} is longer than {MAX_STYLE_TEXT_CHARS} characters"));
    }
    Ok(())
}

fn not_negative(what: &str, value: f32) -> Result<(), String> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(format!("{what} must be 0 or more, not {value}"))
    }
}

/// Checks a style: every value one a project accepts, the size within what the inspector offers.
pub fn check_pack_style(style: &PackTextStyle) -> Result<(), String> {
    if let Some(size) = style.size {
        if !(size.is_finite() && TEXT_SIZE_RANGE.contains(&f64::from(size))) {
            return Err(format!("size must be from {} to {}, not {size}", TEXT_SIZE_RANGE.start(), TEXT_SIZE_RANGE.end()));
        }
    }
    if let Some(line_spacing) = style.line_spacing {
        if !(line_spacing.is_finite() && line_spacing > 0.0 && line_spacing <= MAX_STYLE_LINE_SPACING) {
            return Err(format!("line_spacing must be above 0 and at most {MAX_STYLE_LINE_SPACING}, not {line_spacing}"));
        }
    }
    if let Some(outline) = &style.outline {
        not_negative("outline.width", outline.width)?;
    }
    if let Some(background) = &style.background {
        not_negative("background.padding", background.padding)?;
        not_negative("background.radius", background.radius)?;
    }
    if let Some(shadow) = &style.shadow {
        not_negative("shadow.blur", shadow.blur)?;
        if !shadow.offset.iter().all(|value| value.is_finite()) {
            return Err("shadow.offset must be finite numbers".to_owned());
        }
    }
    for (old, new, both) in [("color", "fill", style.color.is_some() && style.fill.is_some()), ("outline", "outlines", style.outline.is_some() && style.outlines.is_some()), ("shadow", "shadows", style.shadow.is_some() && style.shadows.is_some())] {
        if both {
            return Err(format!("{old} and {new} are both given; {new} takes the place of {old}"));
        }
    }
    let mut text = style.text("x", 0.1);
    if let Some(problem) = engine::text_style_problems(&text).into_iter().next() {
        return Err(format!("{} {} must be {}", problem.field, problem.value, problem.allowed));
    }
    for paint in text.paints_mut() {
        let Some(image) = paint.texture_image() else { continue };
        match image {
            TextImageRef::File { path } => return Err(format!("the texture \"{path}\" must name a picture of a text style pack, not a file")),
            TextImageRef::Pack { pack, file, .. } => {
                if !(d3d12_backend::is_safe_pack_file(pack, file) && is_texture(file)) {
                    return Err(format!("the texture \"{pack}/{file}\" is not a PNG or JPEG picture of a pack"));
                }
            }
        }
    }
    if style.font_hint.len() > MAX_FONT_HINTS {
        return Err(format!("font_hint names more than {MAX_FONT_HINTS} fonts"));
    }
    style.font_hint.iter().try_for_each(|family| short_text("a font_hint", family))
}

/// Checks the animations of a style: a preset of its group, built in or named `"<pack>/<item>"`
/// with ids that can be folder names, and a duration above 0 and at most
/// [`MAX_STYLE_ANIMATION_SECONDS`]. A pack preset need not be installed: a clip naming a missing
/// one plays without it.
pub fn check_pack_animation(animation: &PackStyleAnimation) -> Result<(), String> {
    for (group, value) in animation.groups() {
        let Some(value) = value else { continue };
        let name = group.name();
        let preset = &value.preset;
        let known = if engine::is_pack_preset(preset) {
            preset.split_once(engine::PACK_PRESET_SEPARATOR).is_some_and(|(pack, item)| is_safe_id(pack) && is_safe_id(item))
        } else {
            engine::find_preset(group, preset).is_some()
        };
        if !known {
            return Err(format!("the '{name}' animation '{preset}' is neither a built-in preset of that group nor '<pack>/<item>'"));
        }
        if !(value.duration.is_finite() && value.duration > 0.0 && value.duration <= MAX_STYLE_ANIMATION_SECONDS) {
            return Err(format!("the '{name}' animation must last more than 0 and at most {MAX_STYLE_ANIMATION_SECONDS} seconds"));
        }
    }
    Ok(())
}

/// Checks the parts every style item has, for an item `id`.
pub(crate) fn check_style_item_parts(id: &str, name: &str, category: Option<&str>, preview_text: Option<&str>, style: &PackTextStyle, animation: Option<&PackStyleAnimation>) -> Result<(), ImportProblem> {
    let in_item = |message: String| ImportProblem::BadManifest(format!("the item \"{id}\": {message}"));
    if !is_safe_id(id) {
        return Err(ImportProblem::BadManifest(format!("the item id \"{id}\" may only hold letters, digits, '.', '-' and '_'")));
    }
    short_text("name", name).map_err(in_item)?;
    if let Some(category) = category {
        short_text("category", category).map_err(in_item)?;
    }
    if let Some(preview_text) = preview_text {
        short_text("preview_text", preview_text).map_err(in_item)?;
    }
    check_pack_style(style).map_err(in_item)?;
    animation.map_or(Ok(()), check_pack_animation).map_err(in_item)
}

/// Parses a manifest. The fields are exactly those of the format.
pub fn parse_text_style_manifest(bytes: &[u8]) -> Result<TextStyleManifest, ImportProblem> {
    serde_json::from_slice(bytes).map_err(|e| ImportProblem::BadManifest(e.to_string()))
}

/// Checks what a manifest says, apart from the files. The reader of installed packs makes the same
/// check, so a pack that installs reads.
pub fn check_text_style_fields(manifest: &TextStyleManifest) -> Result<(), ImportProblem> {
    if manifest.kind != TEXT_STYLE_TYPE {
        return Err(ImportProblem::UnknownType(manifest.kind.clone()));
    }
    check_pack_header(header_format(manifest.format), &manifest.id, &manifest.name, &manifest.version, &manifest.license, manifest.items.len(), "styles")?;
    check_layer_format(manifest.format, manifest.items.iter().map(|item| (item.id.as_str(), &item.style)))?;
    check_item_ids(manifest.items.iter().map(|item| (item.id.as_str(), item.name.as_str())))?;
    manifest.items.iter().try_for_each(|item| check_style_item_parts(&item.id, &item.name, item.category.as_deref(), item.preview_text.as_deref(), &item.style, item.animation.as_ref()))
}

/// The format whose header a style pack of `format` has: format 2 has the header of format 1.
pub(crate) fn header_format(format: u32) -> u32 {
    if format == LAYERED_STYLE_FORMAT {
        SUPPORTED_FORMAT
    } else {
        format
    }
}

/// Refuses a style (`(item id, style)`) of a pack of `format` below 2 that uses a field of format 2.
pub(crate) fn check_layer_format<'a>(format: u32, mut styles: impl Iterator<Item = (&'a str, &'a PackTextStyle)>) -> Result<(), ImportProblem> {
    if format == LAYERED_STYLE_FORMAT {
        return Ok(());
    }
    match styles.find(|(_, style)| style.has_layers()) {
        Some((id, _)) => Err(ImportProblem::BadManifest(format!("the item \"{id}\": fill, outlines, shadows, inner_shadows and bloom need format {LAYERED_STYLE_FORMAT}"))),
        None => Ok(()),
    }
}

/// The pictures a pack of format `format` may hold: those of format 2 hold texture pictures, others
/// no media.
pub(crate) fn style_media(format: u32) -> fn(&str) -> bool {
    if format == LAYERED_STYLE_FORMAT {
        is_texture
    } else {
        no_media
    }
}

/// A pack holds no media file.
pub(crate) fn no_media(_: &str) -> bool {
    false
}

/// Whether `file` is a picture a text layer may be painted with.
pub(crate) fn is_texture(file: &str) -> bool {
    let extension = Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    d3d12_backend::ASSET_TEXTURE_EXTENSIONS.contains(&extension.as_str())
}

/// The pictures of its own pack (the pack `id` of type `pack_type`) that `styles` paint with, each
/// once.
pub(crate) fn own_textures<'a>(styles: impl Iterator<Item = &'a PackTextStyle>, id: &str, pack_type: TexturePackType) -> Vec<&'a str> {
    let mut files: Vec<&str> = Vec::new();
    for style in styles {
        let mut paints: Vec<&TextPaint> = style.fill.iter().collect();
        for outline in style.outlines.iter().flatten() {
            paints.push(&outline.paint);
        }
        for shadow in style.shadows.iter().flatten() {
            paints.push(&shadow.paint);
            paints.extend(shadow.outlines.iter().map(|outline| &outline.paint));
        }
        paints.extend(style.inner_shadows.iter().flatten().map(|shadow| &shadow.paint));
        for image in paints.into_iter().filter_map(TextPaint::texture_image) {
            if let TextImageRef::Pack { pack, file, pack_type: kind } = image {
                if *kind == pack_type && pack.eq_ignore_ascii_case(id) && !files.contains(&file.as_str()) {
                    files.push(file);
                }
            }
        }
    }
    files
}

/// Checks the zip at `source` and installs it under `user_root`. An existing pack of the same id is
/// replaced only when `overwrite` allows it.
pub fn install_text_style_pack(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    // Held for the whole install, as for the other packs: the working folders are shared.
    let _lock = lock_installs(&packages).map_err(|e| ImportProblem::WriteFailed(format!("another install is running ({e})")))?;
    clean_leftovers(&packages);

    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, STYLE_PACK_LIMITS)?;
    validate_entries(&entries, STYLE_PACK_LIMITS)?;
    let manifest = parse_text_style_manifest(&read_manifest(&mut zip)?)?;
    check_text_style_fields(&manifest)?;
    // Only a pack of format 2 may hold pictures, and every picture of its own a style names must be
    // in it.
    let media = style_media(manifest.format);
    check_entry_extensions(&entries, media)?;
    let textures = own_textures(manifest.items.iter().map(|item| &item.style), &manifest.id, TexturePackType::TextStyle);
    check_named_files(&manifest.license.files, textures.into_iter(), &entries, media, "a PNG or JPEG picture")?;

    let id = manifest.id.to_ascii_lowercase();
    let check_unpacked = |temporary: &Path| {
        let unpacked = parse_text_style_manifest(&fs::read(temporary.join(MANIFEST)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?)?;
        if unpacked == manifest {
            Ok(())
        } else {
            Err(ImportProblem::BadManifest("it changed while the zip was being read".to_owned()))
        }
    };
    let unpacked = unpack_into_place(&mut zip, &entries, &packages, TEXT_STYLE_TYPE, &id, &manifest.name, overwrite, STYLE_PACK_EXTRACT, &mut rename_with_retries, check_unpacked)?;
    Ok(match unpacked {
        Unpacked::AlreadyThere { name } => ImportOutcome::AlreadyInstalled { name },
        Unpacked::Done { replaced } => ImportOutcome::TextStyleInstalled(InstalledTextStylePack { id, name: manifest.name, replaced }),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::package_import::{install_zip, EntryProblem};
    use std::io::{Cursor, Write};
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-text-style-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) const ITEM_JSON: &str = r#"{"id":"neon","name":"Neon","category":"glow","preview_text":"Hello","style":{"size":0.1,"color":[255,255,255,255],"outline":{"color":[0,0,0,255],"width":0.05},"glow":{"color":[0,200,255,255],"radius":0.4},"align":"left","line_spacing":1.2,"weight":700,"italic":true,"transform":"uppercase","letter_spacing":0.1,"font_hint":["Montserrat","Segoe UI"]},"animation":{"in":{"preset":"fade","duration":0.5},"loop":{"preset":"avs-moves/wave","duration":2}}}"#;

    pub(crate) fn manifest_json() -> String {
        format!(r#"{{"format":1,"type":"text-style","id":"Avs-Text","name":"Text Styles","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{ITEM_JSON}]}}"#)
    }

    pub(crate) fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    pub(crate) fn pack(manifest: &str) -> Vec<u8> {
        zip_of(&[("manifest.json", manifest.as_bytes()), ("LICENSE.txt", b"license")])
    }

    pub(crate) fn written(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_pack_goes_through_install_zip_to_its_folder_under_a_lower_cased_id() {
        let dir = scratch("ok");
        let root = dir.join("root");
        let outcome = install_zip(&written(&dir, &pack(&manifest_json())), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::TextStyleInstalled(InstalledTextStylePack { id: "avs-text".to_owned(), name: "Text Styles".to_owned(), replaced: false }));
        let manifest = parse_text_style_manifest(&fs::read(root.join("packages/text-style/avs-text/manifest.json")).unwrap()).unwrap();
        let style = &manifest.items[0].style;
        assert_eq!((style.weight, style.italic, style.transform), (Some(700), Some(true), Some(TextTransform::Uppercase)));
        assert!(!root.join("packages/.install-tmp").exists(), "no working folder stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_style_gives_a_text_with_its_values_and_new_text_defaults_elsewhere() {
        let manifest = parse_text_style_manifest(manifest_json().as_bytes()).unwrap();
        let text = manifest.items[0].style.text("Hi", 0.2);
        assert_eq!(text.content, "Hi");
        assert_eq!(text.size, 0.1, "the style's size");
        assert_eq!(text.align, TextAlign::Left);
        assert_eq!(text.glow.map(|glow| (glow.intensity, glow.falloff)), Some((1.0, 2.0)), "glow defaults as in a project");
        assert!(text.background.is_none() && text.shadows.first().is_none());
        let plain = PackTextStyle::default().text("Hi", 0.2);
        assert_eq!(plain, TextContent::new("Hi", 0.2), "an empty style is a new text at the given size");
    }

    #[test]
    fn a_manifest_that_is_not_exactly_the_format_is_refused_and_installs_nothing() {
        let full = manifest_json();
        for (name, text, wanted) in [
            ("an unknown field", full.replacen(r#""id":"Avs-Text""#, r#""extra":1,"id":"Avs-Text""#, 1), "BadManifest"),
            ("an unknown item field", full.replace(r#""category":"glow""#, r#""category":"glow","speed":2"#), "BadManifest"),
            ("an unknown style field", full.replace(r#""italic":true"#, r#""italic":true,"bloom":[]"#), "BadManifest"),
            ("a layer field in format 1", full.replace(r#""italic":true"#, r#""italic":true,"shadows":[]"#), "BadManifest"),
            ("a bloom in format 1", full.replace(r#""italic":true"#, r#""italic":true,"bloom":{"color":[255,255,255,255]}"#), "BadManifest"),
            ("an unknown outline field", full.replace(r#""width":0.05"#, r#""width":0.05,"join":"round""#), "BadManifest"),
            ("an unknown animation group", full.replace(r#""loop":{"#, r#""middle":{"#), "BadManifest"),
            ("an unknown align", full.replace(r#""align":"left""#, r#""align":"justify""#), "BadManifest"),
            ("a colour above 255", full.replace("[0,200,255,255]", "[0,200,256,255]"), "BadManifest"),
            ("a size of 0", full.replace(r#""size":0.1"#, r#""size":0"#), "BadManifest"),
            ("a size too large", full.replace(r#""size":0.1"#, r#""size":3"#), "BadManifest"),
            ("a weight too light", full.replace(r#""weight":700"#, r#""weight":50"#), "BadManifest"),
            ("letter spacing too wide", full.replace(r#""letter_spacing":0.1"#, r#""letter_spacing":5"#), "BadManifest"),
            ("a glow too far", full.replace(r#""radius":0.4"#, r#""radius":9"#), "BadManifest"),
            ("a negative outline", full.replace(r#""width":0.05"#, r#""width":-1"#), "BadManifest"),
            ("a line spacing of 0", full.replace(r#""line_spacing":1.2"#, r#""line_spacing":0"#), "BadManifest"),
            ("an empty font hint", full.replace(r#""Montserrat""#, r#""""#), "BadManifest"),
            ("an unknown built-in preset", full.replace(r#""preset":"fade""#, r#""preset":"spin-forever""#), "BadManifest"),
            ("an unsafe pack preset", full.replace("avs-moves/wave", "../x/wave"), "BadManifest"),
            ("a zero duration", full.replace(r#""duration":0.5"#, r#""duration":0"#), "BadManifest"),
            ("a long duration", full.replace(r#""duration":2"#, r#""duration":600"#), "BadManifest"),
            ("an unsafe item id", full.replace(r#""id":"neon""#, r#""id":"a/b""#), "BadManifest"),
            ("a duplicate item id", full.replace(ITEM_JSON, &format!("{ITEM_JSON},{}", ITEM_JSON.replace(r#""id":"neon""#, r#""id":"NEON""#))), "BadManifest"),
            ("another format", full.replacen(r#""format":1"#, r#""format":3"#, 1), "BadManifest"),
            ("no items", full.split(r#""items""#).next().unwrap().to_owned() + r#""items":[]}"#, "BadManifest"),
            ("a licence file not in the zip", full.replace(r#""LICENSE.txt""#, r#""GONE.txt""#), "BadManifest"),
            ("an unsafe id", full.replace("Avs-Text", "../x"), "BadId"),
        ] {
            let dir = scratch("manifest");
            let error = install_zip(&written(&dir, &pack(&text)), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(!dir.join("root/packages/text-style").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    /// A format 2 manifest whose style fills with a picture of its own pack, outlines with a
    /// gradient and stacks two shadows.
    fn layered_manifest(style: &str) -> String {
        manifest_json().replacen(r#""format":1"#, r#""format":2"#, 1).replace(r#""color":[255,255,255,255],"outline":{"color":[0,0,0,255],"width":0.05},"#, style)
    }

    const LAYERED_STYLE: &str = r#""fill":{"kind":"texture","image":{"kind":"pack","pack":"Avs-Text","file":"textures/gold.png"},"fit":"tile","scale":0.5},"outlines":[{"width":0.05,"paint":{"kind":"gradient","stops":[{"position":0,"color":[255,0,0,255]},{"position":1,"color":[0,0,255,255]}],"angle":90}}],"shadows":[{"distance":0.02,"angle":-45,"paint":{"kind":"solid","color":[0,0,0,255]}},{"distance":0.04,"angle":-45,"paint":{"kind":"solid","color":[0,0,0,255]},"outlines":[{"width":0.02,"paint":{"kind":"solid","color":[255,255,255,255]}}]}],"inner_shadows":[{"distance":0.01,"angle":90,"blur":0.01,"paint":{"kind":"solid","color":[0,0,0,128]}}],"bloom":{"color":[255,255,0,255],"strength":0.8,"dir_y":0.5},"#;

    fn layered_pack(manifest: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut all: Vec<(&str, &[u8])> = vec![("manifest.json", manifest.as_bytes()), ("LICENSE.txt", b"license")];
        all.extend_from_slice(files);
        zip_of(&all)
    }

    #[test]
    fn a_format_2_pack_holds_its_pictures_and_its_styles_give_layers() {
        let dir = scratch("layered");
        let root = dir.join("root");
        let manifest = layered_manifest(LAYERED_STYLE);
        let outcome = install_zip(&written(&dir, &layered_pack(&manifest, &[("textures/gold.png", b"png"), ("textures/spare.JPG", b"jpg")])), &root, false).unwrap();
        assert!(matches!(outcome, ImportOutcome::TextStyleInstalled(_)), "{outcome:?}");
        assert!(root.join("packages/text-style/avs-text/textures/gold.png").is_file());
        let manifest = parse_text_style_manifest(manifest.as_bytes()).unwrap();
        let text = manifest.items[0].style.text("Hi", 0.2);
        assert_eq!(text.fill.texture_image(), Some(&TextImageRef::text_style_pack("Avs-Text", "textures/gold.png")));
        assert_eq!((text.outlines.len(), text.shadows.len(), text.shadows[1].outlines.len(), text.inner_shadows.len()), (1, 2, 1, 1));
        assert!(matches!(text.outlines[0].paint, TextPaint::Gradient { .. }));
        assert_eq!(text.bloom, Some(TextBloom { color: [255, 255, 0, 255], strength: 0.8, range: 0.4, dir_x: 0.0, dir_y: 0.5 }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_format_2_pack_is_refused_for_what_it_must_not_hold() {
        let png: &[(&str, &[u8])] = &[("textures/gold.png", b"png")];
        let many_shadows = format!(r#""shadows":[{}],"#, vec![r#"{"distance":0.01,"angle":0,"paint":{"kind":"solid","color":[0,0,0,255]}}"#; engine::MAX_TEXT_SHADOWS + 1].join(","));
        let file_texture = LAYERED_STYLE.replace(r#"{"kind":"pack","pack":"Avs-Text","file":"textures/gold.png"}"#, r#"{"kind":"file","path":"C:/gold.png"}"#);
        let both = format!(r#""color":[255,255,255,255],{LAYERED_STYLE}"#);
        for (name, manifest, files, wanted) in [
            ("its own picture missing", layered_manifest(LAYERED_STYLE), &[][..], "BadManifest"),
            ("a picture that is not PNG or JPEG", layered_manifest(&LAYERED_STYLE.replace("gold.png", "gold.gif")), &[("textures/gold.gif", &b"gif"[..])][..], "BadManifest"),
            ("a texture that is a file", layered_manifest(&file_texture), png, "BadManifest"),
            ("too many shadows", layered_manifest(&many_shadows), &[][..], "BadManifest"),
            ("a fill and a colour", layered_manifest(&both), png, "BadManifest"),
            ("a bloom too strong", layered_manifest(&LAYERED_STYLE.replace(r#""strength":0.8"#, r#""strength":2"#)), png, "BadManifest"),
            ("a picture in a format 1 pack", manifest_json(), png, "Entry"),
        ] {
            let dir = scratch("layered-bad");
            let error = install_zip(&written(&dir, &layered_pack(&manifest, files)), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(!dir.join("root/packages/text-style").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
        // A picture of another pack need not be in the zip, as an animation of another pack need not.
        let dir = scratch("layered-other");
        let other = layered_manifest(&LAYERED_STYLE.replace(r#""pack":"Avs-Text""#, r#""pack":"metal""#));
        assert!(install_zip(&written(&dir, &layered_pack(&other, &[])), &dir.join("root"), false).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_font_file_in_the_zip_is_refused() {
        let dir = scratch("entries");
        let root = dir.join("root");
        let with_font = zip_of(&[("manifest.json", manifest_json().as_bytes()), ("LICENSE.txt", b"l"), ("Montserrat.ttf", b"f")]);
        assert_eq!(install_zip(&written(&dir, &with_font), &root, false), Err(ImportProblem::Entry { entry: "Montserrat.ttf".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        assert!(!root.join("packages/text-style").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_pack_is_replaced_only_when_allowed() {
        let dir = scratch("overwrite");
        let root = dir.join("root");
        install_zip(&written(&dir, &pack(&manifest_json())), &root, false).unwrap();
        let newer = written(&dir, &pack(&manifest_json().replace(r#""name":"Text Styles""#, r#""name":"New Name""#)));
        assert_eq!(install_zip(&newer, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "Text Styles".to_owned() }));
        assert!(matches!(install_zip(&newer, &root, true), Ok(ImportOutcome::TextStyleInstalled(InstalledTextStylePack { replaced: true, .. }))));
        let manifest = parse_text_style_manifest(&fs::read(root.join("packages/text-style/avs-text/manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.name, "New Name");
        let _ = fs::remove_dir_all(&dir);
    }
}
