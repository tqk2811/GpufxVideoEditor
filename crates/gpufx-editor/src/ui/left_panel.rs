//! The panel on the left: imported media, audio and the effect picker.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::{EditorState, LeftTab};
use crate::layout::grid_columns;
use crate::media::{MediaItem, MediaKind};
use crate::packages::{duplicated_names, filter_entries, list_label, short_name, PackageKind};
use crate::timecode::format_duration;
use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, UiBuilder, Vec2};

const TAB_HEIGHT: f32 = 52.0;
const CELL_MIN: f32 = 118.0;
const CELL_GAP: f32 = 8.0;
const CELL_CAPTION: f32 = 24.0;
const THUMB_RATIO: f32 = 0.6;
const PICKER_FOOTER_HEIGHT: f32 = 44.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    let tabs_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), TAB_HEIGHT.min(rect.height())));
    tabs(ui, state, tabs_rect);
    ui.painter_at(rect).hline(rect.x_range(), tabs_rect.bottom(), Stroke::new(1.0, theme::BORDER));
    let content = Rect::from_min_max(egui::pos2(rect.left(), tabs_rect.bottom()), rect.max).shrink(10.0);
    if content.width() < 1.0 || content.height() < 1.0 {
        return;
    }
    ui.scope_builder(UiBuilder::new().max_rect(content), |ui| match state.ui.left_tab {
        LeftTab::Media => library(ui, state, requests, MediaKind::Video),
        LeftTab::Audio => library(ui, state, requests, MediaKind::Audio),
        LeftTab::Effects => picker(ui, state),
    });
}

fn tabs(ui: &mut egui::Ui, state: &mut EditorState, rect: Rect) {
    let entries = [(LeftTab::Media, Icon::Film, "Phương tiện"), (LeftTab::Audio, Icon::Music, "Âm thanh"), (LeftTab::Effects, Icon::Sparkle, "Hiệu ứng")];
    let width = rect.width() / entries.len() as f32;
    for (index, (tab, icon, label)) in entries.into_iter().enumerate() {
        let cell = Rect::from_min_size(rect.min + Vec2::new(width * index as f32, 0.0), Vec2::new(width, rect.height()));
        let response = ui.interact(cell, ui.scope_id().with(("left-tab", index)), Sense::click());
        let selected = state.ui.left_tab == tab;
        if response.hovered() && !selected {
            ui.painter().rect_filled(cell.shrink(3.0), CornerRadius::same(theme::CONTROL_RADIUS), theme::PANEL_RAISED);
        }
        let color = if selected { theme::ACCENT_TEXT } else { theme::TEXT_DIM };
        let painter = ui.painter_at(cell);
        let icon_rect = Rect::from_center_size(egui::pos2(cell.center().x, cell.top() + 18.0), Vec2::splat(16.0));
        icons::paint(&painter, icon_rect, icon, color);
        painter.text(egui::pos2(cell.center().x, cell.top() + 37.0), Align2::CENTER_CENTER, label, FontId::proportional(12.0), color);
        if selected {
            painter.hline(cell.shrink2(Vec2::new(14.0, 0.0)).x_range(), cell.bottom() - 2.0, Stroke::new(2.0, theme::ACCENT));
        }
        if response.clicked() {
            state.ui.left_tab = tab;
        }
    }
}

fn search_box(ui: &mut egui::Ui, text: &mut String, hint: &str) {
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY));
}

fn library(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, kind: MediaKind) {
    let audio_only = kind == MediaKind::Audio;
    if icons::label_button(ui, Icon::Upload, "Nhập từ máy tính", theme::ACCENT_SOFT, theme::ACCENT_TEXT, Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportMedia { audio_only });
    }
    let hint = if audio_only { "Tìm trong âm thanh" } else { "Tìm trong phương tiện" };
    search_box(ui, if audio_only { &mut state.ui.audio_search } else { &mut state.ui.media_search }, hint);
    let search = if audio_only { state.ui.audio_search.clone() } else { state.ui.media_search.clone() };
    let items: Vec<MediaItem> = state.library.visible(kind, &search).into_iter().cloned().collect();
    if items.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new("Chưa có tệp nào. Nhập tệp để thêm vào đây.").color(theme::TEXT_DIM)).wrap());
        return;
    }
    ui.colored_label(theme::TEXT_DIM, "Bấm vào ô hoặc nút + để thêm vào vị trí đầu phát.");
    let mut chosen = None;
    egui::ScrollArea::vertical().id_salt("media-grid").auto_shrink([false, false]).show(ui, |ui| {
        let width = ui.available_width();
        let columns = grid_columns(width, CELL_MIN, CELL_GAP);
        let cell_width = (width - CELL_GAP * (columns as f32 - 1.0)) / columns as f32;
        let cell_height = cell_width * THUMB_RATIO + CELL_CAPTION;
        for row in items.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = CELL_GAP;
                for item in row {
                    if media_cell(ui, item, Vec2::new(cell_width, cell_height), kind) {
                        chosen = Some(item.clone());
                    }
                }
            });
        }
    });
    if let Some(item) = chosen {
        state.add_media_at_playhead(&item);
    }
}

/// One tile of the grid. The picture slot is for a thumbnail later. True when it was clicked.
fn media_cell(ui: &mut egui::Ui, item: &MediaItem, size: Vec2, kind: MediaKind) -> bool {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(theme::CONTROL_RADIUS);
    painter.rect_filled(rect, radius, theme::PANEL_RAISED);
    let stroke = if response.hovered() { Stroke::new(1.0, theme::ACCENT) } else { Stroke::new(1.0, theme::BORDER) };
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    let thumb = Rect::from_min_size(rect.min, Vec2::new(rect.width(), rect.height() - CELL_CAPTION)).shrink(4.0);
    painter.rect_filled(thumb, CornerRadius::same(4), Color32::from_rgb(16, 16, 24));
    icons::paint(&painter, Rect::from_center_size(thumb.center(), Vec2::splat(22.0)), if kind == MediaKind::Audio { Icon::Music } else { Icon::Film }, theme::TEXT_DIM);
    let duration = match (item.duration, item.estimated) {
        (Some(duration), false) => format_duration(duration),
        (Some(duration), true) => format!("~{}", format_duration(duration)),
        (None, _) => "--:--".to_owned(),
    };
    let pill = painter.layout_no_wrap(duration, FontId::monospace(11.0), theme::TEXT);
    let pill_rect = Rect::from_min_size(egui::pos2(thumb.right() - pill.size().x - 8.0, thumb.top() + 3.0), pill.size() + Vec2::new(5.0, 2.0));
    painter.rect_filled(pill_rect, CornerRadius::same(3), Color32::from_black_alpha(180));
    painter.galley(pill_rect.min + Vec2::new(2.5, 1.0), pill, theme::TEXT);
    let plus = Rect::from_min_size(thumb.left_top() + Vec2::splat(3.0), Vec2::splat(18.0));
    painter.rect_filled(plus, CornerRadius::same(4), theme::ACCENT);
    icons::paint(&painter, plus.shrink(4.0), Icon::Plus, Color32::WHITE);
    let caption = rect.with_min_y(rect.bottom() - CELL_CAPTION).shrink2(Vec2::new(8.0, 0.0));
    painter.with_clip_rect(caption).text(caption.left_center(), Align2::LEFT_CENTER, &item.name, FontId::proportional(12.0), theme::TEXT);
    response.on_hover_text(&item.source).clicked()
}

/// The effect and transition lists with their filter.
fn picker(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        for (kind, label) in [(PackageKind::Effect, "Hiệu ứng"), (PackageKind::Transition, "Chuyển cảnh")] {
            if ui.selectable_label(state.picker.kind == kind, label).clicked() {
                state.picker.set_kind(kind);
            }
        }
    });
    search_box(ui, &mut state.picker.filter, "Tìm hiệu ứng");
    if !state.catalog.loaded {
        ui.colored_label(theme::TEXT_DIM, "Đang đọc danh sách gói...");
    } else if state.catalog.effects.is_empty() && state.catalog.transitions.is_empty() {
        ui.colored_label(theme::REFUSED, format!("Không tìm thấy gói nào. Đặt biến {}.", crate::project_file::PACKAGE_ROOT_VARIABLE));
    }
    picker_list(ui, state);
    let selected = state.picker.addable(&state.catalog);
    if ui.add_enabled(selected.is_some(), egui::Button::new("Thêm vào clip đã chọn").min_size(Vec2::new(ui.available_width(), 28.0))).clicked() {
        if let Some(package) = &selected {
            add_package(state, package);
        }
    }
    picker_problems(ui, state);
}

fn add_package(state: &mut EditorState, package: &str) {
    if state.picker.kind == PackageKind::Effect {
        state.add_effect_package(package);
    } else {
        state.add_transition_package(package);
    }
}

/// The height of a selectable label, without the spacing `show_rows` adds itself.
pub(super) fn selectable_row_height(ui: &egui::Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Body) + 2.0 * ui.spacing().button_padding.y).max(ui.spacing().interact_size.y)
}

fn picker_list(ui: &mut egui::Ui, state: &mut EditorState) {
    let row_height = selectable_row_height(ui);
    let duplicates = duplicated_names(state.catalog.entries(state.picker.kind));
    let entries: Vec<_> = filter_entries(state.catalog.entries(state.picker.kind), &state.picker.filter).into_iter().cloned().collect();
    let list_height = (ui.available_height() - PICKER_FOOTER_HEIGHT).max(row_height * 3.0);
    let mut double_clicked = None;
    egui::ScrollArea::vertical().id_salt("package-list").max_height(list_height).auto_shrink([false, false]).show_rows(ui, row_height, entries.len(), |ui, rows| {
        for entry in &entries[rows] {
            let selected = state.picker.selected.as_deref() == Some(&entry.package);
            let response = ui.selectable_label(selected, list_label(entry, &duplicates)).on_hover_text(if entry.description.is_empty() { entry.package.as_str() } else { entry.description.as_str() });
            if response.clicked() {
                state.picker.selected = Some(entry.package.clone());
            }
            if response.double_clicked() {
                double_clicked = Some(entry.package.clone());
            }
        }
    });
    if let Some(package) = double_clicked {
        add_package(state, &package);
    }
}

fn picker_problems(ui: &mut egui::Ui, state: &EditorState) {
    if state.catalog.problems.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(format!("{} gói không đọc được", state.catalog.problems.len())).id_salt("package-problems").show(ui, |ui| {
        for problem in &state.catalog.problems {
            ui.colored_label(theme::REFUSED, format!("{}: {}", short_name(&problem.package), problem.message));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_row_height_is_the_height_of_a_selectable_label() {
        let (mut label, mut computed) = (0.0, 0.0);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            label = ui.selectable_label(false, "Package").rect.height();
            computed = selectable_row_height(ui);
        });
        output.textures_delta.clear();
        assert_eq!(computed, label);
    }
}
