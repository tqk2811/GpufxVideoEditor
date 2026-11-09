//! The panel on the right: the project figures, or everything about the selected clip.

use super::param_widgets;
use super::theme;
use crate::editor::{applied_to, clip_label, transition_duration_limits, ClipField, EditorState};
use crate::packages::short_name;
use crate::timecode::{aspect_label, format_fps, format_timecode, from_seconds, to_seconds};
use crate::timeline::view::content_end;
use egui::{Rect, Stroke, UiBuilder, Vec2};
use engine::{ClipKey, Time, Transition, TrackKind};
use serde_json::{Map, Value};

const HEADER_HEIGHT: f32 = 34.0;
const GAIN_RANGE: std::ops::RangeInclusive<f64> = -60.0..=24.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState) {
    let rect = ui.max_rect();
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_HEIGHT.min(rect.height())));
    let key = state.selection().filter(|key| clip_of(state, *key).is_some());
    let title = if key.is_some() { "Clip" } else { "Dự án" };
    ui.painter_at(rect).text(header.left_center() + Vec2::new(12.0, 0.0), egui::Align2::LEFT_CENTER, title, egui::FontId::proportional(14.0), theme::ACCENT_TEXT);
    ui.painter_at(rect).hline(rect.x_range(), header.bottom(), Stroke::new(1.0, theme::BORDER));
    let body = Rect::from_min_max(egui::pos2(rect.left(), header.bottom()), rect.max).shrink2(Vec2::new(10.0, 8.0));
    if body.width() < 1.0 || body.height() < 1.0 {
        return;
    }
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| match key {
            Some(key) => clip_inspector(ui, state, key),
            None => project_summary(ui, state),
        });
    });
}

fn clip_of(state: &EditorState, key: ClipKey) -> Option<&engine::Clip> {
    state.project().tracks.get(key.track)?.clips.get(key.clip)
}

fn field(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::TEXT_DIM, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.monospace(value));
    });
}

fn project_summary(ui: &mut egui::Ui, state: &EditorState) {
    let output = &state.project().output;
    field(ui, "Kích thước", &format!("{}\u{d7}{}", output.width, output.height));
    field(ui, "Tốc độ khung hình", &format_fps(output.frame_duration));
    field(ui, "Tỉ lệ", &aspect_label(output.width, output.height));
    field(ui, "Thời lượng", &format_timecode(content_end(state.project()), output.frame_duration));
    ui.add_space(8.0);
    ui.separator();
    ui.colored_label(theme::TEXT_DIM, "Chọn một clip trên timeline để chỉnh.");
}

fn clip_inspector(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey) {
    let Some(clip) = clip_of(state, key).cloned() else { return };
    let track_kind = state.project().tracks[key.track].kind;
    ui.add(egui::Label::new(egui::RichText::new(clip_label(&clip.source)).strong()).truncate());
    time_field(ui, state, "Bắt đầu", to_seconds(clip.start), ClipField::Start);
    time_field(ui, state, "Thời lượng", to_seconds(clip.duration), ClipField::Duration);
    ui.separator();

    let mut gain = clip.gain_db;
    ui.colored_label(theme::TEXT_DIM, "Âm lượng");
    let changed = ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = ui.add(egui::Slider::new(&mut gain, GAIN_RANGE).show_value(false)).changed();
        changed |= ui.add(egui::DragValue::new(&mut gain).range(GAIN_RANGE).speed(0.1).max_decimals(1).suffix(" dB").update_while_editing(false)).changed();
        changed
    });
    if changed.inner {
        state.live_gain(gain);
    }
    let mut muted = clip.muted;
    if ui.checkbox(&mut muted, "Tắt tiếng").changed() {
        state.set_muted(muted);
    }

    let (has_before, has_after) = transitions_beside(state, key);
    fade_row(ui, state, "Âm vào", clip.fade_in, clip.duration.saturating_sub(clip.fade_out), !has_before, |state, value| state.live_fades(value, clip.fade_out));
    fade_row(ui, state, "Âm ra", clip.fade_out, clip.duration.saturating_sub(clip.fade_in), !has_after, |state, value| state.live_fades(clip.fade_in, value));

    if track_kind == TrackKind::Video {
        ui.separator();
        effects(ui, state, key);
        transitions(ui, state, key);
    }
}

fn transitions_beside(state: &EditorState, key: ClipKey) -> (bool, bool) {
    let transitions = &state.project().tracks[key.track].transitions;
    (key.clip > 0 && transitions.iter().any(|tr| tr.after_clip + 1 == key.clip), transitions.iter().any(|tr| tr.after_clip == key.clip))
}

fn time_field(ui: &mut egui::Ui, state: &mut EditorState, label: &str, actual: f64, make: fn(f64) -> ClipField) {
    let pending = state.ui.clip_field.filter(|field| std::mem::discriminant(field) == std::mem::discriminant(&make(0.0)));
    let mut value = match pending {
        Some(ClipField::Start(value) | ClipField::Duration(value)) => value,
        None => actual,
    };
    ui.horizontal(|ui| {
        ui.colored_label(theme::TEXT_DIM, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let response = ui.add(egui::DragValue::new(&mut value).range(0.0..=f64::MAX).speed(0.02).max_decimals(3).suffix(" s").update_while_editing(false));
            if response.changed() {
                state.ui.clip_field = Some(make(value));
            }
            if response.drag_stopped() || (response.changed() && !response.dragged()) {
                if let Some(field) = state.ui.clip_field.take() {
                    state.apply_clip_field(field);
                }
            }
        });
    });
}

fn fade_row(ui: &mut egui::Ui, state: &mut EditorState, label: &str, value: Time, longest: Time, enabled: bool, mut apply: impl FnMut(&mut EditorState, Time)) {
    ui.colored_label(theme::TEXT_DIM, label);
    let mut seconds = to_seconds(value);
    let max = to_seconds(longest).max(0.0);
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
            let mut changed = ui.add(egui::Slider::new(&mut seconds, 0.0..=max).show_value(false)).changed();
            changed |= ui.add(egui::DragValue::new(&mut seconds).range(0.0..=max).speed(0.02).max_decimals(2).suffix(" s").update_while_editing(false)).changed();
            if changed {
                apply(state, from_seconds(seconds));
            }
        });
    });
}

fn effects(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey) {
    let Some(clip) = clip_of(state, key).cloned() else { return };
    ui.strong("Hiệu ứng");
    if clip.effects.is_empty() {
        ui.colored_label(theme::TEXT_DIM, "Chưa có hiệu ứng. Thêm ở tab Hiệu ứng.");
    }
    let mut remove = None;
    for (index, effect) in clip.effects.iter().enumerate() {
        ui.push_id(("effect", index), |ui| {
            egui::CollapsingHeader::new(short_name(&effect.package)).default_open(true).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("Xóa").clicked() {
                        remove = Some(index);
                    }
                    if ui.small_button("Mặc định").clicked() {
                        state.live_effect_params(index, Map::new());
                    }
                });
                match state.catalog.params_of(&effect.package).map(<[d3d12_backend::ParamInfo]>::to_vec) {
                    Some(infos) => {
                        if let Some(next) = param_widgets::rows(ui, ui.scope_id(), &infos, &effect.params) {
                            state.live_effect_params(index, next);
                        }
                    }
                    None => {
                        ui.colored_label(theme::TEXT_DIM, "Không đọc được thông số của gói này.");
                    }
                }
            });
        });
    }
    if let Some(index) = remove {
        state.remove_effect_at(index);
    }
}

fn transitions(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey) {
    let (_, applied) = applied_to(state.project(), key);
    if applied.is_empty() {
        return;
    }
    ui.separator();
    ui.strong("Chuyển cảnh");
    let track = key.track;
    let mut remove = None;
    for (after_clip, _) in applied {
        let Some(transition) = state.project().tracks[track].transitions.iter().find(|tr| tr.after_clip == after_clip).cloned() else { continue };
        ui.push_id(("transition", after_clip), |ui| transition_section(ui, state, track, &transition, &mut remove));
    }
    if let Some(after_clip) = remove {
        state.remove_transition_after(track, after_clip);
    }
}

fn transition_section(ui: &mut egui::Ui, state: &mut EditorState, track: usize, transition: &Transition, remove: &mut Option<usize>) {
    egui::CollapsingHeader::new(short_name(&transition.package)).default_open(true).show(ui, |ui| {
        if ui.small_button("Xóa").clicked() {
            *remove = Some(transition.after_clip);
        }
        if let Some((shortest, longest)) = transition_duration_limits(state.project(), track, transition.after_clip) {
            let mut seconds = to_seconds(transition.duration);
            let (low, high) = (to_seconds(shortest), to_seconds(longest));
            ui.colored_label(theme::TEXT_DIM, "Thời lượng");
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
                let mut changed = ui.add(egui::Slider::new(&mut seconds, low..=high).show_value(false)).changed();
                changed |= ui.add(egui::DragValue::new(&mut seconds).range(low..=high).speed(0.02).max_decimals(2).suffix(" s").update_while_editing(false)).changed();
                if changed {
                    state.live_transition(track, transition.after_clip, transition.params.clone(), from_seconds(seconds));
                }
            });
        }
        let infos: Option<Vec<d3d12_backend::ParamInfo>> = state.catalog.params_of(&transition.package).map(<[_]>::to_vec);
        let params: Map<String, Value> = transition.params.clone();
        if let Some(infos) = infos {
            if let Some(next) = param_widgets::rows(ui, ui.scope_id(), &infos, &params) {
                state.live_transition(track, transition.after_clip, next, transition.duration);
            }
        }
    });
}
