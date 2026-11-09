//! The bar across the top: file menu, project name, encoder and export.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::EditorState;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Vec2};

const EXPORT_BAR_WIDTH: f32 = 200.0;
const DIRTY_LABEL: &str = "Đã chỉnh sửa";
const UNTITLED: &str = "Chưa đặt tên";

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    project_name(ui, state, rect);
    ui.allocate_ui_with_layout(rect.size(), Layout::left_to_right(Align::Center), |ui| {
        ui.add_space(10.0);
        ui.add_enabled_ui(!state.is_exporting(), |ui| file_menu(ui, requests));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            export_area(ui, state, requests);
        });
    });
}

fn project_name(ui: &egui::Ui, state: &EditorState, rect: Rect) {
    let name = state.project_file().and_then(std::path::Path::file_stem).map_or_else(|| UNTITLED.to_owned(), |stem| stem.to_string_lossy().into_owned());
    let font = FontId::proportional(15.0);
    let painter = ui.painter_at(rect);
    let galley = painter.layout_no_wrap(name, font.clone(), theme::TEXT);
    let pill_width = if state.is_dirty() { painter.layout_no_wrap(DIRTY_LABEL.to_owned(), FontId::proportional(12.0), theme::TEXT_DIM).size().x + 18.0 } else { 0.0 };
    let total = galley.size().x + if state.is_dirty() { pill_width + 8.0 } else { 0.0 };
    let left = rect.center().x - total / 2.0;
    painter.galley(Pos2::new(left, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT);
    if state.is_dirty() {
        let pill = Rect::from_min_size(Pos2::new(left + total - pill_width, rect.center().y - 11.0), Vec2::new(pill_width, 22.0));
        painter.rect_filled(pill, CornerRadius::same(11), theme::PANEL_RAISED);
        painter.text(pill.center(), Align2::CENTER_CENTER, DIRTY_LABEL, FontId::proportional(12.0), theme::TEXT_DIM);
    }
}

fn file_menu(ui: &mut egui::Ui, requests: &mut Vec<UiRequest>) {
    ui.menu_button("Tệp", |ui| {
        for (label, request) in [("Dự án mới", UiRequest::NewProject), ("Mở...", UiRequest::OpenProject), ("Lưu", UiRequest::SaveProject), ("Lưu thành...", UiRequest::SaveProjectAs)] {
            if ui.button(label).clicked() {
                requests.push(request);
                ui.close();
            }
        }
    });
}

fn export_area(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    if let Some(progress) = state.export_progress() {
        if ui.button("Hủy").clicked() {
            requests.push(UiRequest::CancelExport);
        }
        ui.add(egui::ProgressBar::new(progress.fraction()).desired_width(EXPORT_BAR_WIDTH).text(format!("Đang xuất {} / {}", progress.done, progress.total)));
        return;
    }
    if icons::label_button(ui, Icon::Export, "Xuất video", theme::ACCENT, Color32::WHITE, None).clicked() {
        requests.push(UiRequest::Export);
    }
    let selected = state.encoders.get(state.encoder).map_or_else(String::new, |option| option.label.clone());
    let options = state.encoders.clone();
    egui::ComboBox::from_id_salt("encoder").width(150.0).selected_text(selected).show_ui(ui, |ui| {
        for (index, option) in options.iter().enumerate() {
            ui.add_enabled_ui(option.available, |ui| {
                ui.selectable_value(&mut state.encoder, index, &option.label);
            });
        }
    });
}
