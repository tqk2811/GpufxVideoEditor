//! What the agent tools of text clips need beyond the engine: applying a patch to a text with the
//! checks an agent's mistake deserves a clear answer for, and the lists of fonts and animations.

use engine::{AnimationGroup, FontRef, ClipAnimation, AnimationRef, TextContent, ANIMATION_PRESETS};

use crate::clip_animation_library::{AnimatedClipKind, ClipAnimationLibrary};
use crate::subtitle_style_install::SubtitleLayout;
use crate::text_style_library::{StyleKind, TextStyleLibrary};
use serde_json::{json, Value};

use super::protocol::{AgentError, TextAnimationPatch, TextPatch};
use crate::text_shapes::{available_families, warm_up_shared};

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

/// `text` with the fields of `patch` replaced, or `invalid_params` for a value no text can have.
pub fn apply_patch(text: &TextContent, patch: &TextPatch) -> Result<TextContent, AgentError> {
    let mut next = text.clone();
    if let Some(content) = &patch.content {
        if content.is_empty() {
            return Err(invalid("content must not be empty"));
        }
        next.content.clone_from(content);
    }
    if let Some(font) = &patch.font {
        check_font(font)?;
        next.font = font.clone();
    }
    if let Some(size) = patch.size {
        positive("size", size)?;
        next.size = size;
    }
    if let Some(color) = patch.color {
        next.color = color;
    }
    if let Some(outline) = patch.outline {
        if let Some(outline) = &outline {
            not_negative("outline.width", outline.width)?;
        }
        next.outline = outline;
    }
    if let Some(background) = patch.background {
        if let Some(background) = &background {
            not_negative("background.padding", background.padding)?;
            not_negative("background.radius", background.radius)?;
        }
        next.background = background;
    }
    if let Some(shadow) = patch.shadow {
        if let Some(shadow) = &shadow {
            not_negative("shadow.blur", shadow.blur)?;
            if !shadow.offset.iter().all(|value| value.is_finite()) {
                return Err(invalid("shadow.offset must be finite numbers"));
            }
        }
        next.shadow = shadow;
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
        next.transform = transform;
    }
    if let Some(letter_spacing) = patch.letter_spacing {
        if !(letter_spacing.is_finite() && letter_spacing.abs() <= gpufx_text::MAX_LETTER_SPACING) {
            return Err(invalid(format!("letter_spacing must be from -{0} to {0}, not {letter_spacing}", gpufx_text::MAX_LETTER_SPACING)));
        }
        next.letter_spacing = letter_spacing;
    }
    Ok(next)
}

/// The animations of a clip with the `animation` field of `patch` applied, `None` when the patch
/// leaves them as they are. The animations live on the clip, not on its text. `kind` is the kind of
/// clip they are for, and `library` the installed packs a preset may come from.
pub fn apply_animation_patch(library: &ClipAnimationLibrary, kind: AnimatedClipKind, current: &ClipAnimation, patch: &TextPatch) -> Result<Option<ClipAnimation>, AgentError> {
    patch.animation.as_ref().map(|animation| patched_animation(library, kind, current, animation)).transpose()
}

/// Whether `path` names a network share or a device (`\\server\share`, `//server/share`,
/// `\\?\UNC\...`, `\\.\...`) rather than a local disk. Opening one can reach out to another
/// machine, so an agent may not name one.
fn is_network_or_device_path(path: &str) -> bool {
    use std::path::{Component, Prefix};
    if let Some(Component::Prefix(prefix)) = std::path::Path::new(path).components().next() {
        return !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_));
    }
    // Off Windows there is no prefix, but both spellings still start a share there.
    path.starts_with(r"\\") || path.starts_with("//")
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
    if engine::find_preset(group, &value.preset).is_none() {
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
    if value.duration <= engine::Time::ZERO {
        return Err(invalid(format!("the '{name}' animation must last more than 0 ticks")));
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
/// installed is refused, or said to be still loading while the packs are being read.
pub fn styled_base(library: &TextStyleLibrary, style_id: Option<&str>, text: &TextContent, animation: &ClipAnimation) -> Result<StyledBase, AgentError> {
    let Some(style_id) = style_id else { return Ok(StyledBase { text: text.clone(), animation: animation.clone(), layout: None }) };
    match library.find(style_id) {
        Some(style) => Ok(StyledBase { text: style.apply(text, &available_families()), animation: style.animation_for(animation), layout: style.layout }),
        None if !library.loaded => Err(invalid(format!("'{style_id}' may be a style of an installed pack, but the packs are still loading; try again in a moment"))),
        None => Err(invalid(format!("no style '{style_id}': text_style_list names the installed styles"))),
    }
}

/// The styles of the installed packs of `kind` (`text` or `subtitle`, both when absent): each with
/// its id to give as `style_id`, its name, type, category, preview text, pack, the style values it
/// copies into a text, its animations and, for a subtitle style, its layout. `state` is `loading`
/// while the packs are still being read.
pub fn text_style_list(library: &TextStyleLibrary, kind: Option<&str>) -> Result<Value, AgentError> {
    let kind = match kind {
        None => None,
        Some(name) => Some(StyleKind::from_name(name).ok_or_else(|| invalid(format!("type must be text or subtitle, not '{name}'")))?),
    };
    let styles: Vec<Value> = library
        .styles(kind)
        .map(|(pack, style)| {
            let animation = style.animation.as_ref().map(|animation| serde_json::to_value(animation).unwrap_or(Value::Null));
            json!({"id": style.reference, "name": style.name, "type": style.kind.name(), "category": style.category, "preview_text": style.preview_text, "pack": {"id": pack.id, "name": pack.name}, "style": style.style, "animation": animation, "layout": style.layout})
        })
        .collect();
    let problems: Vec<Value> = library.problems.iter().map(|problem| json!({"pack": problem.pack, "message": problem.message})).collect();
    Ok(json!({"state": if library.loaded { "ready" } else { "loading" }, "styles": styles, "problems": problems}))
}

/// The fonts a text can name, each as the `font` value to use: the system default first. While the
/// scan of the installed fonts has not finished only that one is listed, and the state says so.
pub fn font_list() -> Value {
    let families = available_families();
    if families.is_empty() {
        warm_up_shared();
    }
    let system = families.iter().map(|family| json!({"kind": "system", "family": family}));
    let fonts: Vec<Value> = std::iter::once(json!({"kind": "default"})).chain(system).collect();
    json!({"state": if families.is_empty() { "loading" } else { "ready" }, "fonts": fonts})
}

/// Every preset animation: its id, the name of its group in a project (`in`, `out` or `loop`) and
/// the duration a new one gets, in ticks. A preset of an installed pack also names its pack, its
/// own name and the kinds of clip it is made for.
pub fn animation_list(library: &ClipAnimationLibrary) -> Value {
    let built_in = ANIMATION_PRESETS.iter().map(|preset| json!({"id": preset.id, "group": preset.group.name(), "default_duration": preset.default_duration.ticks()}));
    let packs = library.packs.iter().flat_map(|pack| {
        pack.animations.iter().map(move |animation| {
            let applies_to = animation.applies_to.as_ref().map(|kinds| kinds.iter().map(|kind| kind.name()).collect::<Vec<_>>());
            json!({"id": animation.reference, "group": animation.group.name(), "default_duration": animation.default_duration.ticks(), "name": animation.name, "pack": {"id": pack.id, "name": pack.name}, "applies_to": applies_to})
        })
    });
    let presets: Vec<Value> = built_in.chain(packs).collect();
    json!({"presets": presets})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_preset_is_listed_with_its_pack_and_accepted_for_the_kinds_it_applies_to() {
        use crate::clip_animation_library::{AnimatedClipKind, ClipAnimationLibrary, ClipAnimationPack, LibraryAnimation};
        let curve = engine::AnimationCurve::default().with(engine::CurveProperty::Opacity, vec![engine::CurveKey { t: 0.0, value: 0.0, ease: engine::Ease::Linear }, engine::CurveKey { t: 1.0, value: 1.0, ease: engine::Ease::Linear }]);
        let rise = LibraryAnimation { reference: "agent-pack/rise".to_owned(), name: "Rise".to_owned(), group: AnimationGroup::Enter, default_duration: engine::Time::from_ticks(45_000), applies_to: Some(vec![AnimatedClipKind::Text]), curve };
        let library = ClipAnimationLibrary { packs: vec![ClipAnimationPack { id: "agent-pack".to_owned(), name: "Agent Pack".to_owned(), animations: vec![rise] }], problems: Vec::new(), loaded: true };

        let list = animation_list(&library);
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

        let loading = ClipAnimationLibrary::default();
        let error = patched_animation(&loading, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("agent-pack/rise")).unwrap_err();
        assert!(error.message.contains("still loading"), "{}", error.message);
        let error = patched_animation(&loading, AnimatedClipKind::Text, &ClipAnimation::default(), &patch("nope")).unwrap_err();
        assert!(error.message.contains("is not a preset"), "a name without a pack is not waiting for one: {}", error.message);
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
