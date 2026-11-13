//! The Animation part of the inspector: the in, out and loop presets of a clip with a picture.
//! Every change goes through [`EditorState::live_animation`], so a drag of a duration is one undo
//! step and a click is one too.

use super::numeric::drag_field;
use super::theme;
use crate::editor::EditorState;
use crate::i18n::{Language, Message};
use crate::text_clip::{animation_of, choose_preset, presets_of, with_animation};
use crate::timecode::{from_seconds, to_seconds};
use engine::{AnimationGroup, AnimationRef, ClipAnimation, Time, TrackKind};
use std::ops::RangeInclusive;

const ANIMATION_SECONDS: RangeInclusive<f64> = 0.05..=10.0;
const GROUPS: [AnimationGroup; 3] = [AnimationGroup::Enter, AnimationGroup::Exit, AnimationGroup::Repeat];

/// Whether a clip on a track of `kind` has a picture to animate: video, image, sticker and text
/// clips do, sound does not.
pub(super) fn shows_animation(kind: TrackKind) -> bool {
    kind == TrackKind::Video
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut EditorState, id: egui::Id, animation: &ClipAnimation) {
    let language = state.language();
    ui.label(egui::RichText::new(language.t(Message::TabAnimation)).strong());
    let mut next = animation.clone();
    for group in GROUPS {
        animation_row(ui, id, &mut next, group, language);
    }
    if next != *animation {
        state.live_animation(next);
    }
}

fn group_label(group: AnimationGroup) -> Message {
    match group {
        AnimationGroup::Enter => Message::AnimationEnter,
        AnimationGroup::Exit => Message::AnimationExit,
        AnimationGroup::Repeat => Message::AnimationRepeat,
    }
}

/// The preset of one animation group, and how long it lasts when one is set.
fn animation_row(ui: &mut egui::Ui, id: egui::Id, animation: &mut ClipAnimation, group: AnimationGroup, language: Language) {
    let current = animation_of(animation, group).cloned();
    let none = language.t(Message::AnimationNone);
    let mut chosen: Option<Option<&'static str>> = None;
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(group_label(group)));
        let shown = current.as_ref().map_or_else(|| none.clone(), |value| language.t(preset_name(&value.preset)));
        egui::ComboBox::from_id_salt(id.with(("animation", group as u8))).selected_text(shown).width(ui.available_width()).show_ui(ui, |ui| {
            if ui.selectable_label(current.is_none(), none.clone()).clicked() {
                chosen = Some(None);
            }
            for preset in presets_of(group) {
                let selected = current.as_ref().is_some_and(|value| value.preset == preset);
                if ui.selectable_label(selected, language.t(Message::AnimationPreset(preset))).clicked() {
                    chosen = Some(Some(preset));
                }
            }
        });
    });
    if let Some(choice) = chosen {
        *animation = choose_preset(animation, group, choice);
        return;
    }
    let Some(current) = current else { return };
    ui.indent(("animation-duration", group as u8), |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::text_dim(), language.t(Message::FieldAnimationDuration));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut seconds = to_seconds(current.duration);
                if drag_field(ui, &mut seconds, ANIMATION_SECONDS, 0.01, 2, " s", None).1 {
                    let duration = from_seconds(seconds).max(Time::from_ticks(1));
                    *animation = with_animation(animation, group, Some(AnimationRef { duration, ..current.clone() }));
                }
            });
        });
    });
}

/// The message naming preset `id`, which the engine list holds for as long as the editor runs.
fn preset_name(id: &str) -> Message {
    let known = engine::ANIMATION_PRESETS.iter().find(|preset| preset.id == id).map(|preset| preset.id);
    match known {
        Some(id) => Message::AnimationPreset(id),
        None => Message::Technical(id.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_clips_with_a_picture_show_the_section() {
        assert!(shows_animation(TrackKind::Video));
        assert!(!shows_animation(TrackKind::Audio));
    }

    #[test]
    fn every_group_lists_only_its_own_presets() {
        for group in GROUPS {
            let presets = presets_of(group);
            assert!(!presets.is_empty(), "{group:?} has no preset");
            assert!(presets.iter().all(|id| engine::find_preset(group, id).is_some()));
        }
    }

    #[test]
    fn an_unknown_preset_is_named_by_its_id() {
        assert_eq!(preset_name("fade"), Message::AnimationPreset("fade"));
        assert_eq!(preset_name("no-such-preset"), Message::Technical("no-such-preset".to_owned()));
    }
}
