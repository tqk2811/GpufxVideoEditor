//! Widgets for package parameters. Each returns the new parameter map when the user changed one;
//! the caller decides what that edit means.

use super::numeric::drag_field;
use super::theme;
use crate::params::{bool_value, color_json, color_value, display_name, editable, enum_value, expression_value, number_json, number_value, vector_json, vector_value, widget_for, with_value, ParamWidget};
use d3d12_backend::{expression_error, ParamInfo};
use crate::i18n::{Language, Message};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const NUMBER_WIDTH: f32 = 64.0;
const VECTOR_FIELD_WIDTH: f32 = 54.0;

/// Text of an expression field that the project does not have yet.
#[derive(Clone)]
struct Draft {
    text: String,
    saved: String,
    /// The frame that last stored it, which every frame drawing its field does.
    seen: u64,
}

/// Every draft of the open inspector, kept in one place so that they can all be dropped together.
type Drafts = HashMap<egui::Id, Draft>;

fn drafts_id() -> egui::Id {
    egui::Id::unique("expression-drafts")
}

fn draft_text(ctx: &egui::Context, id: egui::Id) -> Option<String> {
    ctx.data(|data| data.get_temp::<Drafts>(drafts_id()).and_then(|drafts| drafts.get(&id).map(|draft| draft.text.clone())))
}

fn store_draft(ctx: &egui::Context, id: egui::Id, text: String, saved: String) {
    let seen = ctx.cumulative_frame_nr();
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Drafts>(drafts_id()).insert(id, Draft { text, saved, seen }));
}

fn remove_draft(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Drafts>(drafts_id()).remove(&id));
}

/// Forgets every draft and takes the focus from its field. The widget ids of the inspector name a
/// clip by its position, so a draft cannot follow the clip to another one. True when text the
/// project did not have was lost from a field still on screen: the field of a deleted effect has
/// stopped drawing, and its draft was gone for the user long before.
pub(super) fn drop_expression_drafts(ctx: &egui::Context) -> bool {
    let drafts = ctx.data_mut(|data| data.remove_temp::<Drafts>(drafts_id())).unwrap_or_default();
    ctx.memory_mut(|memory| drafts.keys().for_each(|id| memory.surrender_focus(id.with("edit"))));
    let frame = ctx.cumulative_frame_nr();
    drafts.values().any(|draft| draft.text != draft.saved && draft.seen + 1 >= frame)
}

/// The rows of every editable parameter in `infos`, with `params` as their values.
pub fn rows(ui: &mut egui::Ui, id: egui::Id, language: Language, infos: &[ParamInfo], params: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut changed = None;
    for info in editable(infos) {
        ui.push_id((id, info.name.as_str()), |ui| {
            if let Some(next) = row(ui, language, info, params) {
                changed = Some(next);
            }
        });
    }
    changed
}

fn row(ui: &mut egui::Ui, language: Language, info: &ParamInfo, params: &Map<String, Value>) -> Option<Map<String, Value>> {
    let label = ui.label(display_name(info));
    if !info.description.is_empty() {
        label.on_hover_text(&info.description);
    }
    let value = match widget_for(info) {
        ParamWidget::Slider { min, max, integer } => slider(ui, info, params, min, max, integer),
        ParamWidget::Number { min, max, integer } => number(ui, info, params, min, max, integer),
        ParamWidget::Checkbox => {
            let mut flag = bool_value(params, info);
            ui.checkbox(&mut flag, "").changed().then(|| json!(flag))
        }
        ParamWidget::Color => {
            let mut color = color_value(params, info);
            ui.color_edit_button_rgba_unmultiplied(&mut color).changed().then(|| color_json(color))
        }
        ParamWidget::Dropdown(choices) => dropdown(ui, info, params, &choices),
        ParamWidget::Vector { components, min, max } => vector(ui, info, params, components, min, max),
        ParamWidget::Expression => expression(ui, language, info, params),
    };
    value.map(|value| with_value(params, &info.name, value))
}

fn slider(ui: &mut egui::Ui, info: &ParamInfo, params: &Map<String, Value>, min: f64, max: f64, integer: bool) -> Option<Value> {
    let mut value = number_value(params, info);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - NUMBER_WIDTH - ui.spacing().item_spacing.x).max(40.0);
        let mut slider = egui::Slider::new(&mut value, min..=max).show_value(false).clamping(egui::SliderClamping::Edits);
        if integer {
            slider = slider.integer();
        }
        changed |= ui.add(slider).changed();
        changed |= number_field(ui, &mut value, Some(min), Some(max), integer);
    });
    changed.then(|| number_json(info, value))
}

fn number(ui: &mut egui::Ui, info: &ParamInfo, params: &Map<String, Value>, min: Option<f64>, max: Option<f64>, integer: bool) -> Option<Value> {
    let mut value = number_value(params, info);
    number_field(ui, &mut value, min, max, integer).then(|| number_json(info, value))
}

fn number_field(ui: &mut egui::Ui, value: &mut f64, min: Option<f64>, max: Option<f64>, integer: bool) -> bool {
    let low = min.unwrap_or(f64::NEG_INFINITY);
    let high = max.unwrap_or(f64::INFINITY);
    let speed = if min.zip(max).is_some_and(|(min, max)| max - min <= 2.0) { 0.005 } else { 0.1 };
    let size = [NUMBER_WIDTH, ui.spacing().interact_size.y];
    drag_field(ui, value, low..=high, if integer { 1.0 } else { speed }, if integer { 0 } else { 3 }, "", Some(size)).1
}

fn dropdown(ui: &mut egui::Ui, info: &ParamInfo, params: &Map<String, Value>, choices: &[(String, i32)]) -> Option<Value> {
    let current = enum_value(params, info);
    let shown = choices.iter().find(|choice| choice.1 == current).map_or("?", |choice| choice.0.as_str());
    let mut selected = None;
    egui::ComboBox::from_id_salt(&info.name).selected_text(shown).show_ui(ui, |ui| {
        for (name, value) in choices {
            if ui.selectable_label(*value == current, name).clicked() {
                selected = Some(*value);
                ui.close();
            }
        }
    });
    selected.map(|value| json!(value))
}

fn vector(ui: &mut egui::Ui, info: &ParamInfo, params: &Map<String, Value>, components: usize, min: Option<f64>, max: Option<f64>) -> Option<Value> {
    let mut values = vector_value(params, info, components);
    let low = min.unwrap_or(f64::NEG_INFINITY);
    let high = max.unwrap_or(f64::INFINITY);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for value in &mut values {
            let size = [VECTOR_FIELD_WIDTH, ui.spacing().interact_size.y];
            changed |= drag_field(ui, value, low..=high, 0.01, 3, "", Some(size)).1;
        }
    });
    changed.then(|| vector_json(info, &values))
}

/// A text field that keeps what is being typed and writes it to the project only when it is left
/// and parses, so a bad expression never reaches the shaders. Escape puts the saved text back; text
/// that does not parse stays with its error until it is fixed.
fn expression(ui: &mut egui::Ui, language: Language, info: &ParamInfo, params: &Map<String, Value>) -> Option<Value> {
    let saved = expression_value(params, info);
    let id = ui.scope_id().with("expression-text");
    let draft = draft_text(ui.ctx(), id);
    let had_draft = draft.is_some();
    let mut text = draft.unwrap_or_else(|| saved.clone());
    let response = ui.add(egui::TextEdit::singleline(&mut text).id(id.with("edit")).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
    let error = expression_error(&text, &info.vars);
    if let Some(message) = &error {
        ui.colored_label(theme::refused(), message);
    } else if !info.vars.is_empty() {
        ui.colored_label(theme::text_dim(), language.t(Message::Variables(info.vars.join(", "))));
    }
    if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        remove_draft(ui.ctx(), id);
        return None;
    }
    if response.has_focus() {
        store_draft(ui.ctx(), id, text, saved);
        return None;
    }
    if response.lost_focus() && error.is_none() && text != saved {
        remove_draft(ui.ctx(), id);
        return Some(json!(text));
    }
    if error.is_some() && (response.lost_focus() || had_draft) {
        store_draft(ui.ctx(), id, text, saved);
        return None;
    }
    remove_draft(ui.ctx(), id);
    None
}

#[cfg(test)]
pub(super) fn store_test_draft(ctx: &egui::Context, text: &str, saved: &str) {
    store_draft(ctx, egui::Id::unique("test-draft"), text.to_owned(), saved.to_owned());
}

#[cfg(test)]
pub(super) fn has_test_draft(ctx: &egui::Context) -> bool {
    draft_text(ctx, egui::Id::unique("test-draft")).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use d3d12_backend::ParamKind;
    use egui::{Event, Key, Modifiers, RawInput};

    fn expression_info() -> ParamInfo {
        ParamInfo { name: "e".to_owned(), kind: ParamKind::Expr, default: None, min: None, max: None, description: String::new(), vars: vec!["x".to_owned()], choices: Vec::new() }
    }

    fn key(key: Key) -> Event {
        Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    /// One frame of the field for the saved expression `saved`: what it returned and its draft id.
    fn frame(ctx: &egui::Context, saved: &str, events: Vec<Event>) -> (Option<Value>, egui::Id) {
        let params = json!({ "e": saved }).as_object().unwrap().clone();
        let (mut result, mut id) = (None, egui::Id::NULL);
        let mut output = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| {
            id = ui.scope_id().with("expression-text");
            result = expression(ui, Language::default(), &expression_info(), &params);
        });
        output.textures_delta.clear();
        (result, id)
    }

    /// Focuses the field for `saved` and types `typed` before what is there.
    fn focus_and_type(ctx: &egui::Context, saved: &str, typed: &str) -> egui::Id {
        let (_, id) = frame(ctx, saved, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(id.with("edit")));
        frame(ctx, saved, vec![]);
        frame(ctx, saved, vec![key(Key::Home), Event::Text(typed.to_owned())]);
        id
    }

    #[test]
    fn escape_cancels_the_typing_and_writes_nothing() {
        let ctx = egui::Context::default();
        let id = focus_and_type(&ctx, "x", "1+");
        assert_eq!(draft_text(&ctx, id).as_deref(), Some("1+x"), "the test must really type");
        let (result, _) = frame(&ctx, "x", vec![key(Key::Escape)]);
        assert_eq!(result, None);
        assert_eq!(draft_text(&ctx, id), None);
    }

    #[test]
    fn dropping_the_drafts_clears_them_and_takes_the_focus() {
        let ctx = egui::Context::default();
        let id = focus_and_type(&ctx, "x", "1+");
        assert!(ctx.memory(|memory| memory.has_focus(id.with("edit"))), "the test must really focus the field");
        assert!(drop_expression_drafts(&ctx), "typed text was lost");
        assert_eq!(draft_text(&ctx, id), None);
        assert!(!ctx.memory(|memory| memory.has_focus(id.with("edit"))));
        assert!(!drop_expression_drafts(&ctx), "nothing is left to lose");
    }

    #[test]
    fn a_draft_whose_field_stopped_drawing_is_not_reported_as_lost() {
        let ctx = egui::Context::default();
        let id = focus_and_type(&ctx, "", "(");
        ctx.memory_mut(|memory| memory.surrender_focus(id.with("edit")));
        frame(&ctx, "", vec![]);
        assert!(draft_text(&ctx, id).is_some(), "the invalid text is kept");
        for _ in 0..3 {
            let mut output = ctx.run_ui(RawInput::default(), |_| {});
            output.textures_delta.clear();
        }
        assert!(!drop_expression_drafts(&ctx));
        assert_eq!(draft_text(&ctx, id), None, "and it is purged all the same");
    }

    #[test]
    fn a_draft_equal_to_the_project_is_not_reported_as_lost() {
        let ctx = egui::Context::default();
        let (_, id) = frame(&ctx, "x", vec![]);
        ctx.memory_mut(|memory| memory.request_focus(id.with("edit")));
        frame(&ctx, "x", vec![]);
        frame(&ctx, "x", vec![]);
        assert_eq!(draft_text(&ctx, id).as_deref(), Some("x"));
        assert!(!drop_expression_drafts(&ctx));
    }

    #[test]
    fn leaving_the_field_commits_a_valid_expression() {
        let ctx = egui::Context::default();
        let id = focus_and_type(&ctx, "x", "1+");
        ctx.memory_mut(|memory| memory.surrender_focus(id.with("edit")));
        let (result, _) = frame(&ctx, "x", vec![]);
        assert_eq!(result, Some(json!("1+x")));
    }

    #[test]
    fn an_invalid_expression_stays_with_its_error_after_focus_is_lost() {
        let ctx = egui::Context::default();
        let id = focus_and_type(&ctx, "", "(");
        ctx.memory_mut(|memory| memory.surrender_focus(id.with("edit")));
        for _ in 0..3 {
            assert_eq!(frame(&ctx, "", vec![]).0, None);
            assert_eq!(draft_text(&ctx, id).as_deref(), Some("("), "the text is kept");
        }
    }
}
