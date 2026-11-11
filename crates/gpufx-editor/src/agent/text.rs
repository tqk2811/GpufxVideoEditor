//! What the agent tools of text clips need beyond the engine: applying a patch to a text with the
//! checks an agent's mistake deserves a clear answer for, and the lists of fonts and animations.

use engine::{AnimationGroup, FontRef, TextAnimation, TextAnimationRef, TextContent, TEXT_ANIMATION_PRESETS};
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
    if let Some(animation) = &patch.animation {
        next.animation = patched_animation(&next.animation, animation)?;
    }
    Ok(next)
}

/// A family the font scan has finished without is refused; while it still runs the name is taken
/// as given. A font file must be an absolute path to a file that reads as a font.
fn check_font(font: &FontRef) -> Result<(), AgentError> {
    if let FontRef::File { path } = font {
        let file = std::path::Path::new(path);
        if !file.is_absolute() {
            return Err(invalid(format!("font file path {path} must be absolute")));
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

fn patched_animation(current: &TextAnimation, patch: &TextAnimationPatch) -> Result<TextAnimation, AgentError> {
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

fn check_animation(group: AnimationGroup, value: &TextAnimationRef) -> Result<(), AgentError> {
    let name = group.name();
    if engine::find_preset(group, &value.preset).is_none() {
        let known = presets(group).join(", ");
        return Err(invalid(format!("'{}' is not a preset of the '{name}' group; text_animation_list names them: {known}", value.preset)));
    }
    if value.duration <= engine::Time::ZERO {
        return Err(invalid(format!("the '{name}' animation must last more than 0 ticks")));
    }
    Ok(())
}

fn presets(group: AnimationGroup) -> Vec<&'static str> {
    TEXT_ANIMATION_PRESETS.iter().filter(|preset| preset.group == group).map(|preset| preset.id).collect()
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

/// The preset ids, by the name of the group in a project: `in`, `out` and `loop`.
pub fn animation_list() -> Value {
    let mut groups = serde_json::Map::new();
    for group in [AnimationGroup::Enter, AnimationGroup::Exit, AnimationGroup::Repeat] {
        groups.insert(group.name().to_owned(), json!(presets(group)));
    }
    Value::Object(groups)
}
