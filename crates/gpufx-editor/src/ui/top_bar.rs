//! The bar across the top: file menu, project name, encoder and export.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::EditorState;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Vec2};

const EXPORT_BAR_WIDTH: f32 = 200.0;
const NAME_MARGIN: f32 = 16.0;
const DIRTY_LABEL: &str = "Đã chỉnh sửa";
const UNTITLED: &str = "Chưa đặt tên";

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    ui.allocate_ui_with_layout(rect.size(), Layout::left_to_right(Align::Center), |ui| {
        ui.add_space(10.0);
        ui.add_enabled_ui(!state.is_exporting(), |ui| file_menu(ui, requests));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            export_area(ui, state, requests);
            project_name(ui, state, rect, ui.available_rect_before_wrap());
        });
    });
}

/// The left edge of something `width` wide, centred on `center` when it fits between `left` and
/// `right`, and pushed inside them when it does not.
fn name_left(center: f32, width: f32, left: f32, right: f32) -> f32 {
    (center - width / 2.0).min(right - width).max(left)
}

/// The name in the middle of the window, cut short to the room `free` between the other controls.
fn project_name(ui: &egui::Ui, state: &EditorState, rect: Rect, free: Rect) {
    let name = state.project_file().and_then(std::path::Path::file_stem).map_or_else(|| UNTITLED.to_owned(), |stem| stem.to_string_lossy().into_owned());
    let font = FontId::proportional(15.0);
    let painter = ui.painter_at(rect);
    let mut job = LayoutJob::simple_singleline(name.clone(), font.clone(), theme::TEXT);
    let pill_width = if state.is_dirty() { painter.layout_no_wrap(DIRTY_LABEL.to_owned(), FontId::proportional(12.0), theme::TEXT_DIM).size().x + 18.0 } else { 0.0 };
    let pill_room = if state.is_dirty() { pill_width + 8.0 } else { 0.0 };
    job.wrap = TextWrapping::truncate_at_width((free.width() - pill_room - NAME_MARGIN).max(1.0));
    let galley = painter.layout_job(job);
    let total = galley.size().x + pill_room;
    let left = name_left(rect.center().x, total, free.left() + NAME_MARGIN / 2.0, free.right() - NAME_MARGIN / 2.0);
    let name_rect = Rect::from_min_size(Pos2::new(left, rect.center().y - galley.size().y / 2.0), galley.size());
    painter.galley(name_rect.min, galley, theme::TEXT);
    ui.interact(name_rect, ui.scope_id().with("project-name"), egui::Sense::hover()).on_hover_text(name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_stays_centred_when_it_can_and_inside_the_free_room_when_not() {
        assert_eq!(name_left(500.0, 100.0, 200.0, 800.0), 450.0);
        assert_eq!(name_left(500.0, 100.0, 480.0, 800.0), 480.0, "the file menu is in the way");
        assert_eq!(name_left(500.0, 100.0, 200.0, 520.0), 420.0, "the export area is in the way");
    }
}
