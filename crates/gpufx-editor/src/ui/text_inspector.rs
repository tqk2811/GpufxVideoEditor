//! The Text part of the inspector: what a text clip says and how it looks. Every change goes
//! through [`EditorState::live_text`], so a slider drag is one undo step and a click is one too. The
//! animation of the clip is its own section, in [`super::animation_inspector`].

use super::icons::{self, Icon};
use super::numeric::drag_field;
use super::theme;
use crate::editor::EditorState;
use crate::i18n::{Language, Message};
use crate::text_clip::{default_background, default_outline, default_shadow, TEXT_SIZE_RANGE};
use crate::text_shapes::available_families;
use egui::{Rect, Vec2};
use engine::{ClipKey, FontRef, Rgba, TextAlign, TextContent, TextTransform, TEXT_WEIGHT_RANGE};
use std::ops::RangeInclusive;

/// How long typing must pause before the text on screen follows, in seconds. Each pause is one
/// undo step, so undo goes back a word or so rather than a letter.
const TYPING_PAUSE: f64 = 0.6;
const CONTENT_ROWS: usize = 3;
const FONT_LIST_HEIGHT: f32 = 320.0;
const LINE_SPACING_RANGE: RangeInclusive<f64> = 0.5..=3.0;
/// Letter spacing, in percent of the font size.
const LETTER_SPACING_RANGE: RangeInclusive<f64> = -20.0..=100.0;
/// Font weights go in steps of a hundred, as fonts name them.
const WEIGHT_STEP: f64 = 100.0;
/// Outline, padding, radius, offset and blur, in parts of the font size, shown in percent.
const OUTLINE_RANGE: RangeInclusive<f64> = 0.0..=30.0;
const PADDING_RANGE: RangeInclusive<f64> = 0.0..=100.0;
const RADIUS_RANGE: RangeInclusive<f64> = 0.0..=100.0;
const OFFSET_RANGE: RangeInclusive<f64> = -50.0..=50.0;
const BLUR_RANGE: RangeInclusive<f64> = 0.0..=50.0;
const PERCENT: f64 = 100.0;
const WARNING_ICON_SIDE: f32 = 14.0;

/// Text typed in the content box that the clip does not show yet, and when it was last typed.
#[derive(Clone)]
struct ContentDraft {
    text: String,
    typed_at: f64,
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, id: egui::Id, text: &TextContent) {
    let language = state.language();
    ui.label(egui::RichText::new(language.t(Message::TabText)).strong());
    if let Some(family) = state.missing_font(key) {
        missing_font_warning(ui, &language.t(Message::FontMissing(family)));
    }
    let mut next = text.clone();
    if let Some(content) = content_box(ui, id, text, language) {
        next.content = content;
    }
    if font_combo(ui, id, &mut next, language) {
        state.ui.font_file_request = Some(key);
    }
    let output_height = f64::from(state.project().output.height.max(1));
    if let Some(size) = slider_row(ui, &language.t(Message::FieldTextSize), f64::from(text.size) * output_height, (*TEXT_SIZE_RANGE.start() * output_height)..=(*TEXT_SIZE_RANGE.end() * output_height), 0, " px") {
        next.size = (size / output_height) as f32;
    }
    color_row(ui, &language.t(Message::FieldTextColor), &mut next.color);
    align_row(ui, &mut next.align, language);
    if let Some(spacing) = slider_row(ui, &language.t(Message::FieldLineSpacing), f64::from(text.line_spacing), LINE_SPACING_RANGE, 2, "\u{d7}") {
        next.line_spacing = spacing as f32;
    }
    style_rows(ui, &mut next, language);
    ui.separator();
    outline_rows(ui, &mut next, language);
    background_rows(ui, &mut next, language);
    shadow_rows(ui, &mut next, language);
    if next != *text {
        state.live_text(next);
    }
}

fn missing_font_warning(ui: &mut egui::Ui, message: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(WARNING_ICON_SIDE), egui::Sense::hover());
        icons::paint(ui.painter(), Rect::from_center_size(rect.center(), rect.size()), Icon::Warning, theme::refused());
        ui.add(egui::Label::new(egui::RichText::new(message).color(theme::refused())).wrap());
    });
}

/// The box of the words. What is typed shows on the clip once typing pauses or the box is left;
/// an empty box is not applied, and leaving it empty brings the words back.
fn content_box(ui: &mut egui::Ui, id: egui::Id, text: &TextContent, language: Language) -> Option<String> {
    let draft_id = id.with("content-draft");
    let now = ui.input(|input| input.time);
    let mut draft: Option<ContentDraft> = ui.data(|data| data.get_temp(draft_id));
    let mut shown = draft.as_ref().map_or_else(|| text.content.clone(), |draft| draft.text.clone());
    ui.colored_label(theme::text_dim(), language.t(Message::FieldTextContent));
    let response = ui.add(egui::TextEdit::multiline(&mut shown).id(id.with("content")).desired_rows(CONTENT_ROWS).desired_width(f32::INFINITY));
    if response.changed() {
        draft = Some(ContentDraft { text: shown.clone(), typed_at: now });
    }
    let pending = draft?;
    let mut applied = None;
    let paused = now - pending.typed_at >= TYPING_PAUSE;
    if response.lost_focus() || paused {
        if !pending.text.is_empty() && pending.text != text.content {
            applied = Some(pending.text.clone());
        }
        if response.lost_focus() || !pending.text.is_empty() {
            ui.data_mut(|data| data.remove::<ContentDraft>(draft_id));
            return applied;
        }
    } else {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(TYPING_PAUSE - (now - pending.typed_at)));
    }
    ui.data_mut(|data| data.insert_temp(draft_id, pending));
    applied
}

/// The font combo; true when "Choose a font file..." was clicked, which the window answers with
/// the file dialog.
fn font_combo(ui: &mut egui::Ui, id: egui::Id, text: &mut TextContent, language: Language) -> bool {
    let default_name = language.t(Message::DefaultFont);
    let mut choose_file = false;
    let shown = match &text.font {
        FontRef::Default => default_name.clone(),
        FontRef::System { family } => family.clone(),
        FontRef::File { path } => font_file_label(path),
    };
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::FieldFont));
        egui::ComboBox::from_id_salt(id.with("font")).selected_text(shown).width(ui.available_width()).height(FONT_LIST_HEIGHT).show_ui(ui, |ui| {
            ui.selectable_value(&mut text.font, FontRef::Default, default_name);
            if let FontRef::File { path } = &text.font {
                let _ = ui.selectable_label(true, font_file_label(path));
            }
            if ui.selectable_label(false, language.t(Message::ChooseFontFile)).clicked() {
                choose_file = true;
            }
            let families = available_families();
            if families.is_empty() {
                ui.colored_label(theme::text_dim(), language.t(Message::LoadingFonts));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
            for family in families.iter() {
                ui.selectable_value(&mut text.font, FontRef::System { family: family.clone() }, family);
            }
        });
    });
    choose_file
}

/// A font file as the combo shows it: its file name, without reading the file on every repaint.
fn font_file_label(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

/// A slider with a number beside it; the new value when either changed.
fn slider_row(ui: &mut egui::Ui, label: &str, value: f64, range: RangeInclusive<f64>, decimals: usize, suffix: &str) -> Option<f64> {
    ui.colored_label(theme::text_dim(), label);
    let mut value = value;
    let changed = ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(40.0);
        let mut changed = ui.add(egui::Slider::new(&mut value, range.clone()).show_value(false).clamping(egui::SliderClamping::Edits)).changed();
        let step = (range.end() - range.start()) / 300.0;
        changed |= drag_field(ui, &mut value, range, step, decimals, suffix, None).1;
        changed
    });
    changed.inner.then_some(value)
}

fn color_row(ui: &mut egui::Ui, label: &str, color: &mut Rgba) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.color_edit_button_srgba_unmultiplied(color));
    });
}

fn align_row(ui: &mut egui::Ui, align: &mut TextAlign, language: Language) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::FieldAlign));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (value, label) in [(TextAlign::Right, Message::AlignRight), (TextAlign::Center, Message::AlignCenter), (TextAlign::Left, Message::AlignLeft)] {
                ui.selectable_value(align, value, language.t(label));
            }
        });
    });
}

/// Weight, italic, case and letter spacing.
fn style_rows(ui: &mut egui::Ui, text: &mut TextContent, language: Language) {
    let range = f64::from(*TEXT_WEIGHT_RANGE.start())..=f64::from(*TEXT_WEIGHT_RANGE.end());
    if let Some(weight) = slider_row(ui, &language.t(Message::FieldFontWeight), f64::from(text.weight), range.clone(), 0, "") {
        text.weight = ((weight / WEIGHT_STEP).round() * WEIGHT_STEP).clamp(*range.start(), *range.end()) as u16;
    }
    ui.checkbox(&mut text.italic, language.t(Message::TextItalic));
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::FieldTextTransform));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (value, label) in [(TextTransform::Lowercase, Message::TransformLowercase), (TextTransform::Uppercase, Message::TransformUppercase), (TextTransform::None, Message::TransformNone)] {
                ui.selectable_value(&mut text.transform, value, language.t(label));
            }
        });
    });
    percent_row(ui, &language.t(Message::FieldLetterSpacing), &mut text.letter_spacing, LETTER_SPACING_RANGE);
}

/// A checkbox that turns a style on, with its default, or off.
fn toggle<T: Clone>(ui: &mut egui::Ui, label: &str, value: &mut Option<T>, default: fn() -> T) {
    let mut on = value.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *value = on.then(default);
    }
}

fn percent_row(ui: &mut egui::Ui, label: &str, value: &mut f32, range: RangeInclusive<f64>) {
    if let Some(new) = slider_row(ui, label, f64::from(*value) * PERCENT, range, 0, " %") {
        *value = (new / PERCENT) as f32;
    }
}

fn outline_rows(ui: &mut egui::Ui, text: &mut TextContent, language: Language) {
    toggle(ui, &language.t(Message::TextOutline), &mut text.outline, default_outline);
    if let Some(outline) = text.outline.as_mut() {
        ui.indent("outline", |ui| {
            color_row(ui, &language.t(Message::FieldColor), &mut outline.color);
            percent_row(ui, &language.t(Message::FieldThickness), &mut outline.width, OUTLINE_RANGE);
        });
    }
}

fn background_rows(ui: &mut egui::Ui, text: &mut TextContent, language: Language) {
    toggle(ui, &language.t(Message::TextBackground), &mut text.background, default_background);
    if let Some(background) = text.background.as_mut() {
        ui.indent("background", |ui| {
            color_row(ui, &language.t(Message::FieldColor), &mut background.color);
            percent_row(ui, &language.t(Message::FieldPadding), &mut background.padding, PADDING_RANGE);
            percent_row(ui, &language.t(Message::FieldCornerRadius), &mut background.radius, RADIUS_RANGE);
        });
    }
}

fn shadow_rows(ui: &mut egui::Ui, text: &mut TextContent, language: Language) {
    toggle(ui, &language.t(Message::TextShadow), &mut text.shadow, default_shadow);
    if let Some(shadow) = text.shadow.as_mut() {
        ui.indent("shadow", |ui| {
            color_row(ui, &language.t(Message::FieldColor), &mut shadow.color);
            let offset = language.t(Message::FieldShadowOffset);
            percent_row(ui, &format!("{offset} X"), &mut shadow.offset[0], OFFSET_RANGE);
            percent_row(ui, &format!("{offset} Y"), &mut shadow.offset[1], OFFSET_RANGE);
            percent_row(ui, &language.t(Message::FieldBlur), &mut shadow.blur, BLUR_RANGE);
        });
    }
}
