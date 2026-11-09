//! Widgets for package parameters. Each returns the new parameter map when the user changed one;
//! the caller decides what that edit means.

use super::numeric::drag_field;
use super::theme;
use crate::params::{bool_value, color_json, color_value, display_name, editable, enum_value, expression_value, number_json, number_value, vector_json, vector_value, widget_for, with_value, ParamWidget};
use d3d12_backend::{expression_error, ParamInfo};
use serde_json::{json, Map, Value};

const NUMBER_WIDTH: f32 = 64.0;
const VECTOR_FIELD_WIDTH: f32 = 54.0;

/// The rows of every editable parameter in `infos`, with `params` as their values.
pub fn rows(ui: &mut egui::Ui, id: egui::Id, infos: &[ParamInfo], params: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut changed = None;
    for info in editable(infos) {
        ui.push_id((id, info.name.as_str()), |ui| {
            if let Some(next) = row(ui, info, params) {
                changed = Some(next);
            }
        });
    }
    changed
}

fn row(ui: &mut egui::Ui, info: &ParamInfo, params: &Map<String, Value>) -> Option<Map<String, Value>> {
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
        ParamWidget::Expression => expression(ui, info, params),
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
fn expression(ui: &mut egui::Ui, info: &ParamInfo, params: &Map<String, Value>) -> Option<Value> {
    let saved = expression_value(params, info);
    let id = ui.scope_id().with("expression-text");
    let draft = ui.data(|data| data.get_temp::<String>(id));
    let had_draft = draft.is_some();
    let mut text = draft.unwrap_or_else(|| saved.clone());
    let response = ui.add(egui::TextEdit::singleline(&mut text).id(id.with("edit")).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
    let error = expression_error(&text, &info.vars);
    if let Some(message) = &error {
        ui.colored_label(theme::REFUSED, message);
    } else if !info.vars.is_empty() {
        ui.colored_label(theme::TEXT_DIM, format!("Biến: {}", info.vars.join(", ")));
    }
    if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|data| data.remove::<String>(id));
        return None;
    }
    if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, text.clone()));
        return None;
    }
    if response.lost_focus() && error.is_none() && text != saved {
        ui.data_mut(|data| data.remove::<String>(id));
        return Some(json!(text));
    }
    if error.is_some() && (response.lost_focus() || had_draft) {
        ui.data_mut(|data| data.insert_temp(id, text.clone()));
        return None;
    }
    ui.data_mut(|data| data.remove::<String>(id));
    None
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
            result = expression(ui, &expression_info(), &params);
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
        assert_eq!(ctx.data(|data| data.get_temp::<String>(id)).as_deref(), Some("1+x"), "the test must really type");
        let (result, _) = frame(&ctx, "x", vec![key(Key::Escape)]);
        assert_eq!(result, None);
        assert_eq!(ctx.data(|data| data.get_temp::<String>(id)), None);
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
            assert_eq!(ctx.data(|data| data.get_temp::<String>(id)).as_deref(), Some("("), "the text is kept");
        }
    }
}
