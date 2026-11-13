//! What the agent tools of text clips need beyond the engine: applying a patch to a text with the
//! checks an agent's mistake deserves a clear answer for, and the lists of fonts and animations.

use engine::{AnimationGroup, FontRef, ClipAnimation, AnimationRef, TextContent, ANIMATION_PRESETS};

use crate::clip_animation_library::ClipAnimationLibrary;
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
    if let Some(align) = patch.align {
        next.align = align;
    }
    if let Some(line_spacing) = patch.line_spacing {
        positive("line_spacing", line_spacing)?;
        next.line_spacing = line_spacing;
    }
    Ok(next)
}

/// The animations of a clip with the `animation` field of `patch` applied, `None` when the patch
/// leaves them as they are. The animations live on the clip, not on its text.
pub fn apply_animation_patch(current: &ClipAnimation, patch: &TextPatch) -> Result<Option<ClipAnimation>, AgentError> {
    patch.animation.as_ref().map(|animation| patched_animation(current, animation)).transpose()
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

pub fn patched_animation(current: &ClipAnimation, patch: &TextAnimationPatch) -> Result<ClipAnimation, AgentError> {
    let mut next = current.clone();
    for (group, change, slot) in [(AnimationGroup::Enter, &patch.enter, &mut next.enter), (AnimationGroup::Exit, &patch.exit, &mut next.exit), (AnimationGroup::Repeat, &patch.repeat, &mut next.repeat)] {
        if let Some(value) = change {
            if let Some(value) = value {
                check_animation(group, value)?;
            }
            slot.clone_from(value);
        }
    }
    Ok(next)
}

fn check_animation(group: AnimationGroup, value: &AnimationRef) -> Result<(), AgentError> {
    let name = group.name();
    // A preset of a pack is checked against the packs the engine was handed.
    let known = engine::find_preset(group, &value.preset).is_some() || engine::pack_presets().find(group, &value.preset).is_some();
    if !known {
        let known = presets(group).join(", ");
        return Err(invalid(format!("'{}' is not a preset of the '{name}' group; clip_animation_list names them: {known}", value.preset)));
    }
    if value.duration <= engine::Time::ZERO {
        return Err(invalid(format!("the '{name}' animation must last more than 0 ticks")));
    }
    Ok(())
}

fn presets(group: AnimationGroup) -> Vec<String> {
    let packs = engine::pack_presets();
    let built_in = ANIMATION_PRESETS.iter().filter(|preset| preset.group == group).map(|preset| preset.id.to_owned());
    built_in.chain(packs.presets().iter().filter(|preset| preset.group == group).map(|preset| preset.reference.clone())).collect()
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

    /// The only test of the crate that sets the pack table of the engine.
    #[test]
    fn a_pack_preset_is_listed_with_its_pack_and_accepted_once_the_engine_has_it() {
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
        engine::set_pack_presets(library.preset_table());
        assert!(patched_animation(&ClipAnimation::default(), &patch("agent-pack/rise")).is_ok());
        let error = patched_animation(&ClipAnimation::default(), &patch("agent-pack/gone")).unwrap_err();
        assert!(error.message.contains("agent-pack/rise"), "names the pack presets too: {}", error.message);
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
