//! What the agent tools of text clips need beyond the engine: applying a patch to a text with the
//! checks an agent's mistake deserves a clear answer for, and the lists of fonts and animations.

use engine::{AnimationGroup, FontRef, ClipAnimation, AnimationRef, TextContent, ANIMATION_PRESETS};

use crate::clip_animation_library::{AnimatedClipKind, ClipAnimationLibrary};
use crate::font_library::FontLibrary;
use d3d12_backend::ProjectPaths;
use crate::subtitle_style_install::SubtitleLayout;
use crate::text_style_library::{LibraryTextStyle, StyleKind, TextStyleLibrary};
use serde_json::{json, Value};

use super::protocol::{AgentError, TextAnimationPatch, TextPatch};
use super::run::with_translation;
use crate::text_shapes::{available_families, default_family, warm_up_shared};
use crate::search::{rank, take_limit, SearchFields};

fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::new("invalid_params", message)
}

fn positive(name: &str, value: f32) -> Result<(), AgentError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(invalid(format!("{name} must be above 0, not {value}")))
    }
}

fn not_negative(name: &str, value: f32) -> Result<(), AgentError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(invalid(format!("{name} must be 0 or more, not {value}")))
    }
}

/// `look` with the fields `change` gives replaced.
fn merge_look(look: engine::KaraokeLook, change: engine::KaraokeLook) -> engine::KaraokeLook {
    engine::KaraokeLook { color: change.color.or(look.color), background: change.background.or(look.background), scale: change.scale.or(look.scale), opacity: change.opacity.or(look.opacity) }
}

/// `text` with the fields of `patch` replaced, or `invalid_params` for a value no text can have.
pub fn apply_patch(text: &TextContent, patch: &TextPatch) -> Result<TextContent, AgentError> {
    let mut next = text.clone();
    if let Some(content) = &patch.content {
        if content.is_empty() {
            return Err(invalid("content must not be empty"));
        }
        next.set_content(content.as_str());
    }
    if let Some(font) = &patch.font {
        check_font(font)?;
        next.font = font.clone();
    }
    if let Some(size) = patch.size {
        positive("size", size)?;
        next.size = size;
    }
    for (one, all, field) in [(patch.color.is_some(), patch.fill.is_some(), "color and fill"), (patch.outline.is_some(), patch.outlines.is_some(), "outline and outlines"), (patch.shadow.is_some(), patch.shadows.is_some(), "shadow and shadows")] {
        if one && all {
            return Err(invalid(format!("give only one of {field}")));
        }
    }
    if let Some(fill) = &patch.fill {
        next.fill = fill.clone();
    }
    if let Some(outlines) = &patch.outlines {
        next.outlines.clone_from(outlines);
    }
    if let Some(shadows) = &patch.shadows {
        next.shadows.clone_from(shadows);
    }
    if let Some(inner_shadows) = &patch.inner_shadows {
        next.inner_shadows.clone_from(inner_shadows);
    }
    if let Some(bloom) = patch.bloom {
        next.bloom = bloom;
    }
    if let Some(color) = patch.color {
        // A gradient or texture fill is replaced by the colour, as the tool says.
        next.fill = engine::TextPaint::solid(color);
    }
    if let Some(outline) = patch.outline.clone() {
        match outline {
            // The first outline, the one the tool shows; the others stay.
            Some(outline) => {
                not_negative("outline.width", outline.width)?;
                match next.outlines.first_mut() {
                    Some(first) => *first = outline,
                    None => next.outlines.push(outline),
                }
            }
            None => next.outlines.clear(),
        }
    }
    if let Some(background) = patch.background {
        if let Some(background) = &background {
            not_negative("background.padding", background.padding)?;
            not_negative("background.radius", background.radius)?;
        }
        next.background = background;
    }
    if let Some(shadow) = patch.shadow.clone() {
        if let Some(shadow) = &shadow {
            not_negative("shadow.blur", shadow.blur)?;
            if !(shadow.distance.is_finite() && shadow.angle.is_finite()) {
                return Err(invalid("shadow.distance and shadow.angle must be finite numbers"));
            }
        }
        match shadow {
            // The first shadow, the one the tool shows; the others stay.
            Some(shadow) => match next.shadows.first_mut() {
                Some(first) => *first = shadow,
                None => next.shadows.push(shadow),
            },
            None => next.shadows.clear(),
        }
    }
    if let Some(glow) = patch.glow {
        if let Some(glow) = &glow {
            not_negative("glow.radius", glow.radius)?;
            if !(glow.radius <= gpufx_text::MAX_GLOW_RATIO) {
                return Err(invalid(format!("glow.radius must be at most {}, not {}", gpufx_text::MAX_GLOW_RATIO, glow.radius)));
            }
            if !(glow.intensity.is_finite() && (0.0..=engine::MAX_GLOW_INTENSITY).contains(&glow.intensity)) {
                return Err(invalid(format!("glow.intensity must be from 0 to {}, not {}", engine::MAX_GLOW_INTENSITY, glow.intensity)));
            }
            if !engine::GLOW_FALLOFF_RANGE.contains(&glow.falloff) {
                return Err(invalid(format!("glow.falloff must be from {} to {}, not {}", engine::GLOW_FALLOFF_RANGE.start(), engine::GLOW_FALLOFF_RANGE.end(), glow.falloff)));
            }
        }
        next.glow = glow;
    }
    if let Some(align) = patch.align {
        next.align = align;
    }
    if let Some(line_spacing) = patch.line_spacing {
        positive("line_spacing", line_spacing)?;
        next.line_spacing = line_spacing;
    }
    if let Some(weight) = patch.weight {
        if !engine::TEXT_WEIGHT_RANGE.contains(&weight) {
            return Err(invalid(format!("weight must be from 100 to 900, not {weight}")));
        }
        next.weight = weight;
    }
    if let Some(italic) = patch.italic {
        next.italic = italic;
    }
    if let Some(transform) = patch.transform {
        next.set_transform(transform);
    }
    if let Some(letter_spacing) = patch.letter_spacing {
        if !(letter_spacing.is_finite() && letter_spacing.abs() <= gpufx_text::MAX_LETTER_SPACING) {
            return Err(invalid(format!("letter_spacing must be from -{0} to {0}, not {letter_spacing}", gpufx_text::MAX_LETTER_SPACING)));
        }
        next.letter_spacing = letter_spacing;
    }
    if let Some(karaoke) = &patch.karaoke {
        next.karaoke = karaoke.as_ref().map(|change| {
            let mut karaoke = next.karaoke.clone().unwrap_or_default();
            if let Some(mode) = change.mode {
                karaoke.mode = mode;
            }
            for (look, value) in [(&mut karaoke.active, change.active), (&mut karaoke.spoken, change.spoken), (&mut karaoke.upcoming, change.upcoming), (&mut karaoke.emphasis, change.emphasis)] {
                match value {
                    Some(Some(value)) => *look = Some(merge_look(look.unwrap_or_default(), value)),
                    Some(None) => *look = None,
                    None => {}
                }
            }
            if let Some(words) = &change.words {
                karaoke.words.clone_from(words);
            }
            karaoke
        });
    }
    check_texture_files(text, &next)?;
    if let Some(problem) = engine::karaoke_problem(&next) {
        return Err(invalid(problem));
    }
    if let Some(problem) = engine::text_style_problems(&next).into_iter().next() {
        return Err(invalid(format!("{} must be {}, not {}", problem.field, problem.allowed, problem.value)));
    }
    Ok(next)
}

/// The animations of a clip with the `animation` field of `patch` applied, `None` when the patch
/// leaves them as they are. The animations live on the clip, not on its text. `kind` is the kind of
/// clip they are for, and `library` the installed packs a preset may come from.
pub fn apply_animation_patch(library: &ClipAnimationLibrary, kind: AnimatedClipKind, current: &ClipAnimation, patch: &TextPatch) -> Result<Option<ClipAnimation>, AgentError> {
    patch.animation.as_ref().map(|animation| patched_animation(library, kind, current, animation)).transpose()
}

use gpufx_text::is_network_or_device_path;

/// A texture file `next` paints with that `text` did not is refused when it is on a network share
/// or a device, or relative, as a font file is (pictures the text already had stay, since the
/// project folder resolves a relative one). The renderer leaves such a layer out as well.
fn check_texture_files(text: &TextContent, next: &TextContent) -> Result<(), AgentError> {
    let known = gpufx_text::text_images(text);
    for image in gpufx_text::text_images(next) {
        let engine::TextImageRef::File { path } = image else { continue };
        if known.contains(&image) {
            continue;
        }
        if is_network_or_device_path(path) {
            return Err(invalid(format!("texture file path {path} is on a network share or a device; give a file on a local disk")));
        }
        if !std::path::Path::new(path).is_absolute() {
            return Err(invalid(format!("texture file path {path} must be absolute")));
        }
    }
    Ok(())
}

/// A family the font scan has finished without is refused; while it still runs the name is taken
/// as given. A font file must be an absolute path on a local disk to a file no larger than
/// [`gpufx_text::MAX_FONT_FILE_BYTES`] that reads as a font; the size is checked before reading.
fn check_font(font: &FontRef) -> Result<(), AgentError> {
    if let FontRef::File { path } = font {
        if is_network_or_device_path(path) {
            return Err(invalid(format!("font file path {path} is on a network share or a device; give a file on a local disk")));
        }
        let file = std::path::Path::new(path);
        if !file.is_absolute() {
            return Err(invalid(format!("font file path {path} must be absolute")));
        }
        if let Some((_, len)) = gpufx_text::font_file_stamp(file) {
            if len > gpufx_text::MAX_FONT_FILE_BYTES {
                return Err(invalid(format!("font file {path} is {len} bytes, more than the {} a font may have", gpufx_text::MAX_FONT_FILE_BYTES)));
            }
        }
        return match gpufx_text::font_file_family(file) {
            Some(_) => Ok(()),
            None => Err(invalid(format!("{path} is not a readable font file (.ttf, .otf or .ttc)"))),
        };
    }
    let FontRef::System { family } = font else { return Ok(()) };
    let families = available_families();
    if families.is_empty() || families.iter().any(|known| known == family) {
        Ok(())
    } else {
        Err(invalid(format!("no font family {family}: font_list names the families")))
    }
}

/// A font of a font pack must be a font file (.ttf, .otf or .ttc) of an installed pack, found
/// through `paths` as the compositor finds it, so nothing hangs on the scan of the font library and
/// a reference that names no place inside a pack is never written to the project. Every other font
/// is checked by [`apply_patch`].
pub fn check_font_pack(paths: &ProjectPaths, font: Option<&FontRef>) -> Result<(), AgentError> {
    let Some(FontRef::Pack { pack, file }) = font else { return Ok(()) };
    match paths.font_pack_path(pack, file) {
        Ok(_) => Ok(()),
        Err(problem) => Err(invalid(format!("no font {file} in an installed font pack {pack} ({problem}): font_list names the fonts of the packs"))),
    }
}

pub fn patched_animation(library: &ClipAnimationLibrary, kind: AnimatedClipKind, current: &ClipAnimation, patch: &TextAnimationPatch) -> Result<ClipAnimation, AgentError> {
    let mut next = current.clone();
    for (group, change, slot) in [(AnimationGroup::Enter, &patch.enter, &mut next.enter), (AnimationGroup::Exit, &patch.exit, &mut next.exit), (AnimationGroup::Repeat, &patch.repeat, &mut next.repeat)] {
        if let Some(value) = change {
            if let Some(value) = value {
                check_animation(library, kind, group, value)?;
            }
            slot.clone_from(value);
        }
    }
    Ok(next)
}

fn check_animation(library: &ClipAnimationLibrary, kind: AnimatedClipKind, group: AnimationGroup, value: &AnimationRef) -> Result<(), AgentError> {
    let name = group.name();
    if let Some(preset) = engine::find_preset(group, &value.preset) {
        if preset.per_unit().is_some() && kind != AnimatedClipKind::Text {
            return Err(invalid(format!("'{}' moves the characters of a text, so it is only for text clips; clip_animation_list names what each preset applies to", value.preset)));
        }
    } else {
        if let Some((_, animation)) = library.find(group, &value.preset) {
            if !animation.applies_to(kind) {
                let kinds = animation.applies_to.iter().flatten().map(|kind| kind.name()).collect::<Vec<_>>().join(", ");
                return Err(invalid(format!("'{}' is not offered for {} clips, only for: {kinds}; clip_animation_list names what each preset applies to", value.preset, kind.name())));
            }
        } else if !library.loaded && engine::is_pack_preset(&value.preset) {
            return Err(invalid(format!("'{}' may be a preset of an installed pack, but the packs are still loading; try again in a moment", value.preset)));
        } else {
            let known = presets(library, group).join(", ");
            return Err(invalid(format!("'{}' is not a preset of the '{name}' group; clip_animation_list names them: {known}", value.preset)));
        }
    }
    // A negative in or out is a hold (see engine::AnimationRef::duration); a loop cannot be one.
    if value.duration == engine::Time::ZERO || (group == AnimationGroup::Repeat && value.duration < engine::Time::ZERO) {
        return Err(invalid(format!("the '{name}' animation must last more than 0 ticks (only in and out may be negative, as a hold)")));
    }
    Ok(())
}

fn presets(library: &ClipAnimationLibrary, group: AnimationGroup) -> Vec<String> {
    let built_in = ANIMATION_PRESETS.iter().filter(|preset| preset.group == group).map(|preset| preset.id.to_owned());
    built_in.chain(library.packs.iter().flat_map(|pack| &pack.animations).filter(|animation| animation.group == group).map(|animation| animation.reference.clone())).collect()
}

/// What a text and its animations start from before the fields of a patch: `text` and `animation`
/// as they are, or in the style `style_id` of `library`, with the layout of a subtitle style.
pub struct StyledBase {
    pub text: TextContent,
    pub animation: ClipAnimation,
    pub layout: Option<SubtitleLayout>,
}

/// The [`StyledBase`] of `text` and `animation` for a patch naming `style_id`. A style that is not
/// installed is refused, or said to be still loading while the packs are being read. A style that
/// names fonts in its `font_hint` while `families` (the installed fonts) is empty is said to be
/// loading too: the scan of the fonts is still running, and applying it now would silently skip the
/// font.
pub fn styled_base(library: &TextStyleLibrary, families: &[String], pack_fonts: &[crate::font_library::LibraryFont], style_id: Option<&str>, text: &TextContent, animation: &ClipAnimation) -> Result<StyledBase, AgentError> {
    let Some(style_id) = style_id else { return Ok(StyledBase { text: text.clone(), animation: animation.clone(), layout: None }) };
    match library.find(style_id) {
        Some(style) if families.is_empty() && !style.style.font_hint.is_empty() => Err(invalid(format!("'{style_id}' names fonts, but the list of fonts is still loading; try again in a moment"))),
        Some(style) => Ok(StyledBase { text: style.apply(text, families, pack_fonts), animation: style.animation_for(animation), layout: style.layout }),
        None if !library.loaded => Err(invalid(format!("'{style_id}' may be a style of an installed pack, but the packs are still loading; try again in a moment"))),
        None => Err(invalid(format!("no style '{style_id}': text_style_list names the installed styles"))),
    }
}

/// The subtitle style `style_id` of `library`, refused like [`styled_base`] refuses a style: not
/// installed, still loading, or naming fonts while the list of fonts is loading; and refused when it
/// is a text style.
pub fn subtitle_style(library: &TextStyleLibrary, families: &[String], style_id: &str) -> Result<LibraryTextStyle, AgentError> {
    match library.find(style_id) {
        Some(style) if style.kind != StyleKind::Subtitle => Err(invalid(format!("'{style_id}' is a text style; text_style_list with type subtitle names the subtitle styles"))),
        Some(style) if families.is_empty() && !style.style.font_hint.is_empty() => Err(invalid(format!("'{style_id}' names fonts, but the list of fonts is still loading; try again in a moment"))),
        Some(style) => Ok(style.clone()),
        None if !library.loaded => Err(invalid(format!("'{style_id}' may be a style of an installed pack, but the packs are still loading; try again in a moment"))),
        None => Err(invalid(format!("no style '{style_id}': text_style_list names the installed styles"))),
    }
}

/// The styles of the installed packs of `kind` (`text` or `subtitle`, both when absent): each with
/// its id to give as `style_id`, its name, type, category, preview text, pack, the style values it
/// copies into a text, its animations and, for a subtitle style, its layout. `state` is `loading`
/// while the packs are still being read. Only the styles matching `filter` (see [`crate::search`])
/// are listed, best first, matched in English and in every language a pack brings. At most `limit`
/// when given. A style whose pack brings the editor's language `code` (not English) has its texts in
/// it under `translation`, its pack too.
pub fn text_style_list(library: &TextStyleLibrary, kind: Option<&str>, filter: &str, limit: Option<usize>, code: &str) -> Result<Value, AgentError> {
    let kind = match kind {
        None => None,
        Some(name) => Some(StyleKind::from_name(name).ok_or_else(|| invalid(format!("type must be text or subtitle, not '{name}'")))?),
    };
    let found = rank(library.styles(kind), filter, |(pack, style)| pack.i18n.search(style.i18n.search(SearchFields::new(&style.name).tags(style.category.as_deref().into_iter().chain([style.kind.name()])).other([style.reference.as_str(), pack.id.as_str(), pack.name.as_str(), style.preview_text.as_str()]))));
    let styles: Vec<Value> = take_limit(found, limit)
        .into_iter()
        .map(|(pack, style)| {
            let animation = style.animation.as_ref().map(|animation| serde_json::to_value(animation).unwrap_or(Value::Null));
            let pack_json = with_translation(json!({"id": pack.id, "name": pack.name}), &pack.i18n, code);
            with_translation(json!({"id": style.reference, "name": style.name, "type": style.kind.name(), "category": style.category, "preview_text": style.preview_text, "pack": pack_json, "style": style.style, "animation": animation, "layout": style.layout}), &style.i18n, code)
        })
        .collect();
    let problems: Vec<Value> = library.problems.iter().map(|problem| json!({"pack": problem.pack, "message": problem.message})).collect();
    Ok(json!({"state": if library.loaded { "ready" } else { "loading" }, "styles": styles, "problems": problems}))
}

/// The fonts a text can name, each as the `font` value to use: the system default first, then the
/// installed families, then the fonts of the installed font packs with their names. While a scan
/// of the installed fonts or of the packs has not finished, what it finds is missing and the state
/// says so. Only the fonts matching `filter` (see [`crate::search`]) are listed, best first. At most `limit` when given.
pub fn font_list(library: &FontLibrary, filter: &str, limit: Option<usize>) -> Value {
    let families = available_families();
    if families.is_empty() {
        warm_up_shared();
    }
    // The default also matches the family it draws with, so a search for "segoe" finds it too.
    let default_family = default_family();
    let default = std::iter::once((json!({"kind": "default"}), SearchFields::new("default").other(default_family.as_deref().into_iter().chain(["system"]))));
    let system = families.iter().map(|family| (json!({"kind": "system", "family": family}), SearchFields::new(family).tags(["system"])));
    let packs = library.fonts.iter().map(|font| (json!({"kind": "pack", "pack": font.pack, "file": font.file, "name": font.name, "pack_name": font.pack_name, "family": font.family}), SearchFields::new(&font.name).tags(["pack"]).other(font.family.as_deref().into_iter().chain([font.pack.as_str(), font.pack_name.as_str(), font.file.as_str()]))));
    let fonts: Vec<Value> = take_limit(rank(default.chain(system).chain(packs), filter, |(_, fields)| fields.clone()), limit).into_iter().map(|(font, _)| font).collect();
    let ready = !families.is_empty() && library.loaded;
    json!({"state": if ready { "ready" } else { "loading" }, "fonts": fonts})
}

/// Every preset animation: its id, the name of its group in a project (`in`, `out` or `loop`) and
/// the duration a new one gets, in ticks. A preset of an installed pack also names its pack, its
/// own name and the kinds of clip it is made for. Only the presets matching `filter` (see
/// [`crate::search`]) are listed, best first, a pack preset matched in English and in every language
/// its pack brings. At most `limit` when given. A pack preset whose pack brings the editor's language
/// `code` (not English) has its texts in it under `translation`, its pack too.
pub fn animation_list(library: &ClipAnimationLibrary, filter: &str, limit: Option<usize>, code: &str) -> Value {
    let built_in = ANIMATION_PRESETS.iter().map(|preset| {
        let per_unit = preset.per_unit().is_some();
        let mut entry = json!({"id": preset.id, "group": preset.group.name(), "default_duration": preset.default_duration.ticks(), "per_unit": per_unit});
        if per_unit {
            entry["applies_to"] = json!(["text"]);
        }
        (entry, SearchFields::new(preset.id).tags([preset.group.name()]))
    });
    let packs = library.packs.iter().flat_map(|pack| {
        pack.animations.iter().map(move |animation| {
            let applies_to = animation.applies_to.as_ref().map(|kinds| kinds.iter().map(|kind| kind.name()).collect::<Vec<_>>());
            let effect = animation.effect_package.as_ref().map(|package| json!({"package": package, "available": animation.effect.is_some()}));
            let fields = pack.i18n.search(animation.i18n.search(SearchFields::new(&animation.name).tags([animation.group.name()]).other([animation.reference.as_str(), pack.id.as_str(), pack.name.as_str()])));
            let pack_json = with_translation(json!({"id": pack.id, "name": pack.name}), &pack.i18n, code);
            (with_translation(json!({"id": animation.reference, "group": animation.group.name(), "default_duration": animation.default_duration.ticks(), "name": animation.name, "pack": pack_json, "applies_to": applies_to, "effect": effect, "per_unit": animation.per_unit.is_some()}), &animation.i18n, code), fields)
        })
    });
    let presets: Vec<Value> = take_limit(rank(built_in.chain(packs), filter, |(_, fields)| fields.clone()), limit).into_iter().map(|(preset, _)| preset).collect();
    json!({"presets": presets})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without a scan of the font library to lean on, a font of a pack is still checked: a reference
    /// that is no font file inside an installed pack is refused instead of written to the project.
    #[test]
    fn a_pack_font_is_checked_on_disk_whether_or_not_the_library_was_scanned() {
        let root = std::env::temp_dir().join(format!("gpufx-agent-font-pack-{}", std::process::id()));
        let pack = root.join("packages/font/open-fonts");
        std::fs::create_dir_all(pack.join("fonts")).unwrap();
        std::fs::write(pack.join("manifest.json"), "{}").unwrap();
        std::fs::write(pack.join("fonts/a.ttf"), b"font").unwrap();
        std::fs::write(pack.join("OFL.txt"), b"licence").unwrap();
        let paths = ProjectPaths::roots_only(std::path::Path::new("agent.json"), vec![root.clone()]);
        let font = |pack: &str, file: &str| FontRef::Pack { pack: pack.to_owned(), file: file.to_owned() };
        assert!(check_font_pack(&paths, Some(&font("Open-Fonts", "fonts/a.ttf"))).is_ok());
        assert!(check_font_pack(&paths, None).is_ok());
        assert!(check_font_pack(&paths, Some(&FontRef::Default)).is_ok());
        for (pack, file) in [("open-fonts", "../../x.ttf"), ("open-fonts", "fonts\\a.ttf"), ("open-fonts", "/fonts/a.ttf"), ("open-fonts", "OFL.txt"), ("open fonts", "fonts/a.ttf"), ("..", "a.ttf"), ("", "a.ttf"), ("open-fonts", "fonts/b.ttf"), ("other", "fonts/a.ttf")] {
            assert!(check_font_pack(&paths, Some(&font(pack, file))).is_err(), "{pack}/{file}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn karaoke_is_patched_checked_and_taken_off_with_null() {
        use super::super::protocol::KaraokePatch;
        let word = |start: i64, end: i64| engine::KaraokeWord { start: engine::Time::from_ticks(start), end: engine::Time::from_ticks(end), emphasis: false };
        let text = TextContent::new("one two", 0.1);
        let on = TextPatch { karaoke: Some(Some(KaraokePatch { mode: Some(engine::KaraokeMode::Box), words: Some(vec![word(0, 10), word(10, 20)]), ..KaraokePatch::default() })), ..TextPatch::default() };
        let patched = apply_patch(&text, &on).unwrap();
        let karaoke = patched.karaoke.clone().unwrap();
        assert_eq!((karaoke.mode, karaoke.words.len()), (engine::KaraokeMode::Box, 2));

        let look = TextPatch { karaoke: Some(Some(KaraokePatch { active: Some(Some(engine::KaraokeLook { color: Some([255, 0, 0, 255]), ..Default::default() })), ..KaraokePatch::default() })), ..TextPatch::default() };
        let looked = apply_patch(&patched, &look).unwrap();
        assert_eq!(looked.karaoke.as_ref().unwrap().mode, engine::KaraokeMode::Box, "what is absent stays");
        assert_eq!(looked.karaoke.as_ref().unwrap().words.len(), 2);

        let wrong = TextPatch { karaoke: Some(Some(KaraokePatch { words: Some(vec![word(0, 10)]), ..KaraokePatch::default() })), ..TextPatch::default() };
        assert!(apply_patch(&patched, &wrong).unwrap_err().message.contains("2 words"));

        let retyped = apply_patch(&patched, &TextPatch { content: Some("a b c".to_owned()), ..TextPatch::default() }).unwrap();
        assert!(retyped.karaoke.as_ref().unwrap().words.is_empty(), "new words share the cue again");

        let off = apply_patch(&patched, &TextPatch { karaoke: Some(None), ..TextPatch::default() }).unwrap();
        assert_eq!(off.karaoke, None);
    }

    #[test]
    fn a_karaoke_look_is_merged_field_by_field() {
        use super::super::protocol::KaraokePatch;
        let text = TextContent::new("one two", 0.1);
        let red = TextPatch { karaoke: Some(Some(KaraokePatch { mode: Some(engine::KaraokeMode::Color), active: Some(Some(engine::KaraokeLook { color: Some([255, 0, 0, 255]), ..Default::default() })), ..KaraokePatch::default() })), ..TextPatch::default() };
        let colored = apply_patch(&text, &red).unwrap();
        let faded = TextPatch { karaoke: Some(Some(KaraokePatch { active: Some(Some(engine::KaraokeLook { opacity: Some(0.5), ..Default::default() })), ..KaraokePatch::default() })), ..TextPatch::default() };
        let faded = apply_patch(&colored, &faded).unwrap();
        let active = faded.karaoke.clone().unwrap().active.unwrap();
        assert_eq!((active.color, active.opacity), (Some([255, 0, 0, 255]), Some(0.5)), "the colour set before stays");
        let cleared = TextPatch { karaoke: Some(Some(KaraokePatch { active: Some(None), ..KaraokePatch::default() })), ..TextPatch::default() };
        assert_eq!(apply_patch(&faded, &cleared).unwrap().karaoke.unwrap().active, None, "null takes the look off");
    }

    #[test]
    fn a_pack_preset_is_found_by_its_translation_and_lists_the_one_of_the_editor_language() {
        use crate::clip_animation_library::{ClipAnimationLibrary, ClipAnimationPack, LibraryAnimation};
        let translations = crate::i18n::pack::PackTranslations::from_files([("vi".to_owned(), r#"{"name": "Gói chuyển động", "items": {"rise": {"name": "Nâng lên"}}}"#.to_owned())]);
        let rise = LibraryAnimation { i18n: translations.item("rise"), per_unit: None, reference: "p/rise".to_owned(), name: "Rise".to_owned(), group: AnimationGroup::Enter, default_duration: engine::Time::from_ticks(1), applies_to: None, curve: engine::AnimationCurve::default(), effect_package: None, effect: None };
        let library = ClipAnimationLibrary { packs: vec![ClipAnimationPack { i18n: translations.pack(), id: "p".to_owned(), name: "Moves".to_owned(), animations: vec![rise] }], problems: Vec::new(), loaded: true };

        let found = animation_list(&library, "nang len", None, "vi");
        let presets = found["presets"].as_array().unwrap();
        assert_eq!(presets.len(), 1, "{presets:?}");
        assert_eq!((presets[0]["id"].as_str(), presets[0]["name"].as_str()), (Some("p/rise"), Some("Rise")), "the id and the English name stay");
        assert_eq!(presets[0]["translation"], json!({"language": "vi", "name": "Nâng lên"}));
        assert_eq!(presets[0]["pack"]["translation"]["name"], "Gói chuyển động");
        let english = animation_list(&library, "goi chuyen dong", None, "en");
        let presets = english["presets"].as_array().unwrap();
        assert_eq!(presets.len(), 1, "the pack's translated name is matched whatever the language");
        assert!(presets[0].get("translation").is_none() && presets[0]["pack"].get("translation").is_none(), "no translation for English");
    }

    #[test]
    fn a_pack_preset_is_listed_with_its_pack_and_accepted_for_the_kinds_it_applies_to() {
        use crate::clip_animation_library::{AnimatedClipKind, ClipAnimationLibrary, ClipAnimationPack, LibraryAnimation};
        let curve = engine::AnimationCurve::default().with(engine::CurveProperty::Opacity, vec![engine::CurveKey { t: 0.0, value: 0.0, ease: engine::Ease::Linear }, engine::CurveKey { t: 1.0, value: 1.0, ease: engine::Ease::Linear }]);
        let rise = LibraryAnimation { i18n: Default::default(), per_unit: None, reference: "agent-pack/rise".to_owned(), name: "Rise".to_owned(), group: AnimationGroup::Enter, default_duration: engine::Time::from_ticks(45_000), applies_to: Some(vec![AnimatedClipKind::Text]), curve, effect_package: None, effect: None };
        let library = ClipAnimationLibrary { packs: vec![ClipAnimationPack { i18n: Default::default(), id: "agent-pack".to_owned(), name: "Agent Pack".to_owned(), animations: vec![rise] }], problems: Vec::new(), loaded: true };

        let list = animation_list(&library, "", None, "en");
        let presets = list["presets"].as_array().unwrap();
        assert_eq!(presets.len(), ANIMATION_PRESETS.len() + 1);
        let pack = presets.iter().find(|preset| preset["id"] == "agent-pack/rise").unwrap();
        assert_eq!((pack["group"].as_str(), pack["pack"]["name"].as_str(), pack["applies_to"][0].as_str(), pack["default_duration"].as_i64()), (Some("in"), Some("Agent Pack"), Some("text"), Some(45_000)));

        let patch = |preset: &str| TextAnimationPatch { enter: Some(Some(AnimationRef { preset: preset.to_owned(), duration: engine::Time::from_ticks(9000) })), exit: None, repeat: None };
        assert!(patched_animation(&library, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("agent-pack/rise")).is_ok());
        let error = patched_animation(&library, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("agent-pack/gone")).unwrap_err();
        assert!(error.message.contains("agent-pack/rise"), "names the pack presets too: {}", error.message);
        let error = patched_animation(&library, AnimatedClipKind::Video, &ClipAnimation::default(), &patch("agent-pack/rise")).unwrap_err();
        assert!(error.message.contains("not offered for video") && error.message.contains("text"), "{}", error.message);
        assert_eq!(pack["per_unit"], false);
        let typewriter = presets.iter().find(|preset| preset["id"] == "typewriter" && preset["group"] == "in").unwrap();
        assert_eq!((typewriter["per_unit"].as_bool(), typewriter["applies_to"][0].as_str()), (Some(true), Some("text")));
        assert!(patched_animation(&library, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("typewriter")).is_ok());
        let error = patched_animation(&library, AnimatedClipKind::Image, &ClipAnimation::default(), &patch("typewriter")).unwrap_err();
        assert!(error.message.contains("only for text clips"), "{}", error.message);

        let loading = ClipAnimationLibrary::default();
        let error = patched_animation(&loading, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("agent-pack/rise")).unwrap_err();
        assert!(error.message.contains("still loading"), "{}", error.message);
        let error = patched_animation(&loading, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("nope")).unwrap_err();
        assert!(error.message.contains("is not a preset"), "a name without a pack is not waiting for one: {}", error.message);
    }

    #[test]
    fn the_list_says_whether_the_effect_of_a_pack_preset_is_installed() {
        use crate::clip_animation_library::{ClipAnimationLibrary, ClipAnimationPack, LibraryAnimation};
        let animation = |id: &str, effect_package: Option<&str>, effect: Option<engine::PresetEffect>| LibraryAnimation { i18n: Default::default(), per_unit: None, reference: format!("p/{id}"), name: id.to_owned(), group: AnimationGroup::Enter, default_duration: engine::Time::from_ticks(1), applies_to: None, curve: engine::AnimationCurve::default(), effect_package: effect_package.map(str::to_owned), effect };
        let ready = engine::PresetEffect { package: "packages/gpufx/x/wipe".to_owned(), progress: "progress".to_owned(), params: serde_json::Map::new() };
        let animations = vec![animation("plain", None, None), animation("ready", Some("io.x.wipe"), Some(ready)), animation("missing", Some("io.x.gone"), None)];
        let library = ClipAnimationLibrary { packs: vec![ClipAnimationPack { i18n: Default::default(), id: "p".to_owned(), name: "P".to_owned(), animations }], problems: Vec::new(), loaded: true };
        let list = animation_list(&library, "", None, "en");
        let effect = |id: &str| list["presets"].as_array().unwrap().iter().find(|preset| preset["id"] == format!("p/{id}")).unwrap()["effect"].clone();
        assert_eq!(effect("plain"), Value::Null);
        assert_eq!(effect("ready"), json!({"package": "io.x.wipe", "available": true}));
        assert_eq!(effect("missing"), json!({"package": "io.x.gone", "available": false}));
    }

    #[test]
    fn weight_italic_transform_and_letter_spacing_are_patched_and_checked() {
        let text = TextContent::new("Hi", 0.1);
        let patch = TextPatch { weight: Some(700), italic: Some(true), transform: Some(engine::TextTransform::Uppercase), letter_spacing: Some(0.1), ..TextPatch::default() };
        let next = apply_patch(&text, &patch).unwrap();
        assert_eq!((next.weight, next.italic, next.transform, next.letter_spacing), (700, true, engine::TextTransform::Uppercase, 0.1));
        for bad in [TextPatch { weight: Some(50), ..TextPatch::default() }, TextPatch { weight: Some(1000), ..TextPatch::default() }, TextPatch { letter_spacing: Some(f32::NAN), ..TextPatch::default() }, TextPatch { letter_spacing: Some(3.0), ..TextPatch::default() }] {
            assert_eq!(apply_patch(&text, &bad).unwrap_err().code, "invalid_params", "{bad:?}");
        }
    }

    #[test]
    fn a_glow_is_patched_checked_and_taken_off_with_null() {
        let text = TextContent::new("Hi", 0.1);
        let glow = engine::TextGlow { color: [0, 255, 255, 255], outer_color: Some([0, 0, 255, 0]), radius: 0.5, intensity: 1.5, falloff: 1.0 };
        let next = apply_patch(&text, &TextPatch { glow: Some(Some(glow)), ..TextPatch::default() }).unwrap();
        assert_eq!(next.glow, Some(glow));
        assert_eq!(apply_patch(&next, &TextPatch { glow: Some(None), ..TextPatch::default() }).unwrap().glow, None);
        for bad in [engine::TextGlow { radius: -1.0, ..glow }, engine::TextGlow { radius: 9.0, ..glow }, engine::TextGlow { intensity: 5.0, ..glow }, engine::TextGlow { intensity: f32::NAN, ..glow }, engine::TextGlow { falloff: 0.0, ..glow }] {
            assert_eq!(apply_patch(&text, &TextPatch { glow: Some(Some(bad)), ..TextPatch::default() }).unwrap_err().code, "invalid_params", "{bad:?}");
        }
        let parsed: TextPatch = serde_json::from_value(json!({"glow": null})).unwrap();
        assert_eq!(parsed.glow, Some(None));
    }

    fn file(path: &str) -> FontRef {
        FontRef::File { path: path.to_owned() }
    }

    #[test]
    fn a_style_with_a_font_hint_is_said_to_be_loading_until_the_fonts_are_listed() {
        let text = crate::text_clip::default_text("Hi");
        let animation = ClipAnimation::default();
        let hinted = crate::text_style_library::subtitle_library_for_test(&["Segoe UI"]);
        let error = styled_base(&hinted, &[], &[], Some("s/low"), &text, &animation).map(|_| ()).unwrap_err();
        assert!(error.message.contains("still loading") && error.message.contains("fonts"), "{}", error.message);
        let families = vec!["Segoe UI".to_owned()];
        let styled = styled_base(&hinted, &families, &[], Some("s/low"), &text, &animation).unwrap();
        assert_eq!(styled.text.font, FontRef::System { family: "Segoe UI".to_owned() });
        let plain = crate::text_style_library::subtitle_library_for_test(&[]);
        assert!(styled_base(&plain, &[], &[], Some("s/low"), &text, &animation).is_ok(), "a style without a font_hint needs no fonts");
        assert!(styled_base(&hinted, &[], &[], None, &text, &animation).is_ok(), "no style, no wait");
    }

    #[test]
    fn a_font_file_on_a_network_share_or_device_is_refused_without_opening_it() {
        for path in [r"\\server\share\font.ttf", "//server/share/font.ttf", r"\\?\UNC\server\share\font.ttf", r"\\.\pipe\font", r"\\?\GLOBALROOT\Device\font.ttf"] {
            let error = check_font(&file(path)).expect_err(path);
            assert_eq!(error.code, "invalid_params", "{path}");
            assert!(error.message.contains("network share"), "{path}: {}", error.message);
        }
        assert!(!is_network_or_device_path(r"C:\Windows\Fonts\arial.ttf"));
        assert!(!is_network_or_device_path(r"\\?\C:\Windows\Fonts\arial.ttf"), "a verbatim local path is a local disk");
    }

    #[test]
    fn outline_and_shadow_change_only_the_first_layer() {
        let mut text = crate::text_clip::default_text("Hi");
        let gold = engine::TextPaint::Gradient { stops: vec![engine::GradientStop { position: 0.0, color: [255, 200, 0, 255] }, engine::GradientStop { position: 1.0, color: [120, 60, 0, 255] }], angle: 90.0, per_glyph: false };
        text.outlines = vec![engine::TextOutline::solid([0, 0, 0, 255], 0.05), engine::TextOutline { width: 0.1, paint: gold.clone() }];
        text.shadows = vec![engine::TextShadow::solid([0, 0, 0, 128], [0.05, 0.05], 0.0), engine::TextShadow::solid([0, 0, 0, 64], [0.1, 0.1], 0.0)];
        let outline = engine::TextOutline::solid([255, 0, 0, 255], 0.08);
        let next = apply_patch(&text, &TextPatch { outline: Some(Some(outline.clone())), ..TextPatch::default() }).unwrap();
        assert_eq!(next.outlines, vec![outline.clone(), text.outlines[1].clone()]);
        let shadow = engine::TextShadow::solid([0, 0, 255, 255], [0.02, 0.02], 0.01);
        let next = apply_patch(&text, &TextPatch { shadow: Some(Some(shadow.clone())), ..TextPatch::default() }).unwrap();
        assert_eq!(next.shadows, vec![shadow.clone(), text.shadows[1].clone()]);
        // An empty list gains the one given; null still takes them all off.
        let bare = crate::text_clip::default_text("Hi");
        let next = apply_patch(&bare, &TextPatch { outline: Some(Some(outline.clone())), shadow: Some(Some(shadow.clone())), ..TextPatch::default() }).unwrap();
        assert_eq!((next.outlines, next.shadows), (vec![outline], vec![shadow]));
        let next = apply_patch(&text, &TextPatch { outline: Some(None), shadow: Some(None), ..TextPatch::default() }).unwrap();
        assert!(next.outlines.is_empty() && next.shadows.is_empty());
        // color turns a gradient fill into that colour.
        text.fill = gold;
        assert_eq!(apply_patch(&text, &TextPatch { color: Some([1, 2, 3, 255]), ..TextPatch::default() }).unwrap().fill, engine::TextPaint::solid([1, 2, 3, 255]));
    }

    #[test]
    fn a_new_texture_file_on_a_network_share_or_relative_is_refused() {
        let text = crate::text_clip::default_text("Hi");
        let with = |path: &str| {
            let mut next = text.clone();
            next.fill = engine::TextPaint::Texture(engine::TextTexture { image: engine::TextImageRef::File { path: path.to_owned() }, fit: engine::TextureFit::Stretch, scale: 1.0, angle: 0.0, opacity: 1.0, mix: 1.0, cells: 1, flip_x: false, flip_y: false, base: None });
            next
        };
        for path in [r"\\server\share\a.png", "//server/share/a.png", r"\\?\UNC\server\share\a.png"] {
            let error = check_texture_files(&text, &with(path)).expect_err(path);
            assert!(error.code == "invalid_params" && error.message.contains("network share"), "{path}: {}", error.message);
        }
        let error = check_texture_files(&text, &with("textures/a.png")).expect_err("relative");
        assert!(error.message.contains("absolute"), "{}", error.message);
        assert!(check_texture_files(&text, &with(r"C:\pictures\a.png")).is_ok());
        let kept = with("textures/a.png");
        assert!(check_texture_files(&kept, &kept).is_ok(), "a picture the text had stays");
    }

    #[test]
    fn a_font_file_larger_than_the_limit_is_refused_before_it_is_read() {
        let Some(arial) = std::env::var_os("WINDIR").map(|dir| std::path::Path::new(&dir).join("Fonts").join("arial.ttf")).filter(|path| path.is_file()) else {
            eprintln!("skipped: no arial.ttf");
            return;
        };
        let dir = std::env::temp_dir().join(format!("gpufx-agent-font-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A real font with zeros after it reads as a font, so only the size refuses it. The
        // zeros are added by setting the length, which writes nothing.
        let big = dir.join("big.ttf");
        std::fs::copy(&arial, &big).unwrap();
        let path = big.to_string_lossy().into_owned();
        assert!(check_font(&file(&path)).is_ok(), "the copy itself is a font");
        std::fs::OpenOptions::new().write(true).open(&big).unwrap().set_len(gpufx_text::MAX_FONT_FILE_BYTES + 1).unwrap();
        let error = check_font(&file(&path)).expect_err("too large");
        assert_eq!(error.code, "invalid_params");
        assert!(error.message.contains("bytes"), "{}", error.message);
        assert_eq!(gpufx_text::font_file_family(&big), None, "the rasterizer side does not read it either");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
