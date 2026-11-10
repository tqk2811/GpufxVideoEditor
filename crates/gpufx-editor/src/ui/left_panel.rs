//! The panel on the left: imported media, audio and the effect picker.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::{EditorState, LeftTab};
use crate::i18n::{Language, Message};
use crate::layout::grid_columns;
use crate::media::{MediaItem, MediaKind};
use crate::packages::{duplicated_names, filter_entries, list_label, short_name, PackageKind};
use crate::timecode::format_duration;
use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, UiBuilder, Vec2};

const RAIL_WIDTH: f32 = 68.0;
const RAIL_CELL_HEIGHT: f32 = 56.0;
const TAB_FONT: f32 = 12.0;
const TAB_FONT_MIN: f32 = 9.5;
const TAB_PADDING: f32 = 6.0;
const CELL_MIN: f32 = 118.0;
const CELL_GAP: f32 = 8.0;
const CELL_CAPTION: f32 = 24.0;
const THUMB_RATIO: f32 = 0.6;
const PICKER_FOOTER_HEIGHT: f32 = 44.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    let rail = Rect::from_min_size(rect.min, Vec2::new(RAIL_WIDTH.min(rect.width()), rect.height()));
    rail_tabs(ui, state, rail);
    ui.painter_at(rect).vline(rail.right(), rect.y_range(), Stroke::new(1.0, theme::border()));
    let content = Rect::from_min_max(egui::pos2(rail.right(), rect.top()), rect.max).shrink(10.0);
    if content.width() < 1.0 || content.height() < 1.0 {
        return;
    }
    ui.scope_builder(UiBuilder::new().max_rect(content), |ui| match state.ui.left_tab {
        LeftTab::Media => library(ui, state, requests, MediaKind::Video),
        LeftTab::Audio => library(ui, state, requests, MediaKind::Audio),
        LeftTab::Effects | LeftTab::Transitions => picker(ui, state),
    });
}

/// The height of one entry of the rail: all `count` entries stay reachable in a panel too short
/// for the usual height.
fn rail_cell_height(available: f32, count: usize) -> f32 {
    if RAIL_CELL_HEIGHT * count as f32 <= available {
        RAIL_CELL_HEIGHT
    } else {
        available / count.max(1) as f32
    }
}

/// The centre of the rail entry `index` in a left panel at `panel`, for tests that click it.
#[cfg(test)]
pub(super) fn rail_cell_center(panel: Rect, index: usize) -> egui::Pos2 {
    egui::pos2(panel.left() + 1.0 + RAIL_WIDTH / 2.0, panel.top() + 1.0 + RAIL_CELL_HEIGHT * (index as f32 + 0.5))
}

/// The icons on the left edge, one per tab.
fn rail_tabs(ui: &mut egui::Ui, state: &mut EditorState, rail: Rect) {
    let language = state.language();
    let entries = [
        (LeftTab::Media, Icon::Film, Message::TabMedia),
        (LeftTab::Audio, Icon::Music, Message::TabAudio),
        (LeftTab::Effects, Icon::Sparkle, Message::TabEffects),
        (LeftTab::Transitions, Icon::Transition, Message::Transitions),
    ];
    let height = rail_cell_height(rail.height(), entries.len());
    for (index, (tab, icon, label)) in entries.into_iter().enumerate() {
        let cell = Rect::from_min_size(rail.min + Vec2::new(0.0, height * index as f32), Vec2::new(rail.width(), height));
        let response = ui.interact(cell, ui.scope_id().with(("left-tab", index)), Sense::click());
        let selected = state.ui.left_tab == tab;
        let painter = ui.painter_at(cell);
        if selected {
            painter.rect_filled(cell.shrink(3.0), CornerRadius::same(theme::CONTROL_RADIUS), theme::accent_soft());
            painter.vline(cell.left() + 1.0, cell.y_range(), Stroke::new(2.0, theme::accent()));
        } else if response.hovered() {
            painter.rect_filled(cell.shrink(3.0), CornerRadius::same(theme::CONTROL_RADIUS), theme::panel_raised());
        }
        let color = if selected { theme::accent_text() } else { theme::text_dim() };
        let icon_rect = Rect::from_center_size(egui::pos2(cell.center().x, cell.center().y - 8.0), Vec2::splat(16.0));
        icons::paint(&painter, icon_rect, icon, color);
        let label = language.t(label);
        let natural = painter.layout_no_wrap(label.clone(), FontId::proportional(TAB_FONT), color).size().x;
        painter.text(egui::pos2(cell.center().x, cell.center().y + 12.0), Align2::CENTER_CENTER, label, FontId::proportional(tab_font_size(natural, cell.width())), color);
        if response.clicked() {
            state.ui.left_tab = tab;
            match tab {
                LeftTab::Effects => state.picker.set_kind(PackageKind::Effect),
                LeftTab::Transitions => state.picker.set_kind(PackageKind::Transition),
                LeftTab::Media | LeftTab::Audio => {}
            }
        }
    }
}

/// The size that fits a tab label `natural` wide in a cell: the longest labels of some languages
/// would not fit at a narrow window, so they shrink, down to what is still readable.
fn tab_font_size(natural: f32, cell_width: f32) -> f32 {
    let room = (cell_width - TAB_PADDING).max(1.0);
    (TAB_FONT * (room / natural.max(1.0))).clamp(TAB_FONT_MIN, TAB_FONT)
}

fn search_box(ui: &mut egui::Ui, text: &mut String, hint: &str) {
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY));
}

fn library(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, kind: MediaKind) {
    let language = state.language();
    let audio_only = kind == MediaKind::Audio;
    if icons::label_button(ui, Icon::Upload, &language.t(Message::ImportFromComputer), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportMedia { audio_only });
    }
    let hint = language.t(if audio_only { Message::SearchAudio } else { Message::SearchMedia });
    search_box(ui, if audio_only { &mut state.ui.audio_search } else { &mut state.ui.media_search }, &hint);
    let search = if audio_only { state.ui.audio_search.clone() } else { state.ui.media_search.clone() };
    let items: Vec<MediaItem> = state.library.visible(kind, &search).into_iter().cloned().collect();
    if items.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(language.t(empty_message(&search))).color(theme::text_dim())).wrap());
        return;
    }
    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::AddHint)).color(theme::text_dim())).wrap());
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
                    if media_cell(ui, state, item, Vec2::new(cell_width, cell_height), kind) {
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

/// What an empty grid says: a filter that matches nothing is not an empty library.
fn empty_message(search: &str) -> Message {
    if search.trim().is_empty() {
        Message::LibraryEmpty
    } else {
        Message::NoSearchMatch
    }
}

/// One tile of the grid, with a thumbnail of the file once it is made. True on a first click: the
/// later clicks of a double click do not add the file again.
fn media_cell(ui: &mut egui::Ui, state: &EditorState, item: &MediaItem, size: Vec2, kind: MediaKind) -> bool {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    response.dnd_set_drag_payload(item.clone());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(theme::CONTROL_RADIUS);
    painter.rect_filled(rect, radius, theme::panel_raised());
    let stroke = if response.hovered() { Stroke::new(1.0, theme::accent()) } else { Stroke::new(1.0, theme::border()) };
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    let thumb = Rect::from_min_size(rect.min, Vec2::new(rect.width(), rect.height() - CELL_CAPTION)).shrink(4.0);
    painter.rect_filled(thumb, CornerRadius::same(4), theme::thumb_bg());
    // Only a tile scrolled into view asks for its picture.
    let poster = (kind == MediaKind::Video && ui.is_rect_visible(thumb)).then(|| state.visuals.borrow_mut().poster(&item.source, item.duration)).flatten();
    match poster {
        Some((texture, aspect)) => {
            let picture = Rect::from_center_size(thumb.center(), fit_size(thumb.size(), aspect));
            painter.image(texture, picture, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => icons::paint(&painter, Rect::from_center_size(thumb.center(), Vec2::splat(22.0)), if kind == MediaKind::Audio { Icon::Music } else { Icon::Film }, theme::text_dim()),
    }
    let duration = match (item.duration, item.estimated) {
        (Some(duration), false) => format_duration(duration),
        (Some(duration), true) => format!("~{}", format_duration(duration)),
        (None, _) => "--:--".to_owned(),
    };
    let pill = painter.layout_no_wrap(duration, FontId::monospace(11.0), theme::on_clip());
    let pill_rect = Rect::from_min_size(egui::pos2(thumb.right() - pill.size().x - 8.0, thumb.top() + 3.0), pill.size() + Vec2::new(5.0, 2.0));
    painter.rect_filled(pill_rect, CornerRadius::same(3), Color32::from_black_alpha(180));
    painter.galley(pill_rect.min + Vec2::new(2.5, 1.0), pill, theme::on_clip());
    let plus = Rect::from_min_size(thumb.left_top() + Vec2::splat(3.0), Vec2::splat(18.0));
    painter.rect_filled(plus, CornerRadius::same(4), theme::accent());
    icons::paint(&painter, plus.shrink(4.0), Icon::Plus, theme::on_accent());
    let caption = rect.with_min_y(rect.bottom() - CELL_CAPTION).shrink2(Vec2::new(8.0, 0.0));
    painter.with_clip_rect(caption).text(caption.left_center(), Align2::LEFT_CENTER, &item.name, FontId::proportional(12.0), theme::text());
    let response = response.on_hover_text(&item.source);
    response.clicked() && !response.double_clicked() && !response.triple_clicked()
}

/// The largest size of shape `aspect` inside `room`.
fn fit_size(room: Vec2, aspect: f32) -> Vec2 {
    let width = room.x.min(room.y * aspect);
    Vec2::new(width, width / aspect.max(0.01))
}

/// The effect and transition lists with their filter.
fn picker(ui: &mut egui::Ui, state: &mut EditorState) {
    let language: Language = state.language();
    search_box(ui, &mut state.picker.filter, &language.t(Message::SearchEffects));
    if !state.catalog.loaded {
        ui.colored_label(theme::text_dim(), language.t(Message::ReadingPackages));
    } else if state.catalog.effects.is_empty() && state.catalog.transitions.is_empty() {
        ui.colored_label(theme::refused(), language.t(Message::NoPackages(crate::project_file::PACKAGE_ROOT_VARIABLE.to_owned())));
    }
    picker_list(ui, state);
    let selected = state.picker.addable(&state.catalog);
    if ui.add_enabled(selected.is_some(), egui::Button::new(language.t(Message::AddToClip)).min_size(Vec2::new(ui.available_width(), 28.0))).clicked() {
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
    egui::CollapsingHeader::new(state.language().t(Message::PackageProblems(state.catalog.problems.len()))).id_salt("package-problems").show(ui, |ui| {
        for problem in &state.catalog.problems {
            ui.colored_label(theme::refused(), format!("{}: {}", short_name(&problem.package), problem.message));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poster_fits_its_tile_keeping_its_shape() {
        assert_eq!(fit_size(Vec2::new(160.0, 90.0), 16.0 / 9.0), Vec2::new(160.0, 90.0));
        assert_eq!(fit_size(Vec2::new(160.0, 90.0), 1.0), Vec2::new(90.0, 90.0));
        assert_eq!(fit_size(Vec2::new(90.0, 90.0), 2.0), Vec2::new(90.0, 45.0));
    }

    #[test]
    fn a_tab_label_shrinks_only_when_it_does_not_fit() {
        assert_eq!(tab_font_size(40.0, 100.0), TAB_FONT);
        let fitted = tab_font_size(70.0, 60.0);
        assert!(fitted < TAB_FONT && fitted >= TAB_FONT_MIN, "{fitted}");
        assert_eq!(tab_font_size(500.0, 60.0), TAB_FONT_MIN);
    }

    #[test]
    fn the_rail_cells_keep_their_height_unless_the_panel_is_too_short() {
        assert_eq!(rail_cell_height(600.0, 4), RAIL_CELL_HEIGHT);
        assert_eq!(rail_cell_height(4.0 * RAIL_CELL_HEIGHT, 4), RAIL_CELL_HEIGHT);
        let short = rail_cell_height(120.0, 4);
        assert_eq!(short, 30.0);
        assert!(short * 4.0 <= 120.0);
    }

    #[test]
    fn an_empty_grid_says_whether_a_filter_hides_the_files() {
        assert_eq!(empty_message("  "), Message::LibraryEmpty);
        assert_eq!(empty_message("beach"), Message::NoSearchMatch);
    }

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
