//! The bar across the top: file menu, project name, encoder and export.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::EditorState;
use crate::i18n::{Language, Message};
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Align2, CornerRadius, FontId, Layout, Pos2, Rect, Vec2};

const EXPORT_BAR_WIDTH: f32 = 200.0;
const NAME_MARGIN: f32 = 16.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    ui.allocate_ui_with_layout(rect.size(), Layout::left_to_right(Align::Center), |ui| {
        ui.add_space(10.0);
        let (language, agent_access) = (state.language(), state.agent_access);
        file_menu(ui, language, state.ui.theme, agent_access, state.is_exporting(), requests);
        if state.agent_connected {
            ui.colored_label(theme::accent(), language.t(Message::AgentConnected));
        }
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
    let name = state.project_file().and_then(std::path::Path::file_stem).map_or_else(|| state.language().t(Message::Untitled), |stem| stem.to_string_lossy().into_owned());
    let dirty_label = state.language().t(Message::Dirty);
    let font = FontId::proportional(15.0);
    let painter = ui.painter_at(rect);
    let mut job = LayoutJob::simple_singleline(name.clone(), font.clone(), theme::text());
    let pill_width = if state.is_dirty() { painter.layout_no_wrap(dirty_label.clone(), FontId::proportional(12.0), theme::text_dim()).size().x + 18.0 } else { 0.0 };
    let pill_room = if state.is_dirty() { pill_width + 8.0 } else { 0.0 };
    job.wrap = TextWrapping::truncate_at_width((free.width() - pill_room - NAME_MARGIN).max(1.0));
    let galley = painter.layout_job(job);
    let total = galley.size().x + pill_room;
    let left = name_left(rect.center().x, total, free.left() + NAME_MARGIN / 2.0, free.right() - NAME_MARGIN / 2.0);
    let name_rect = Rect::from_min_size(Pos2::new(left, rect.center().y - galley.size().y / 2.0), galley.size());
    painter.galley(name_rect.min, galley, theme::text());
    ui.interact(name_rect, ui.scope_id().with("project-name"), egui::Sense::hover()).on_hover_text(name);
    if state.is_dirty() {
        let pill = Rect::from_min_size(Pos2::new(left + total - pill_width, rect.center().y - 11.0), Vec2::new(pill_width, 22.0));
        painter.rect_filled(pill, CornerRadius::same(11), theme::panel_raised());
        painter.text(pill.center(), Align2::CENTER_CENTER, dirty_label, FontId::proportional(12.0), theme::text_dim());
    }
}

/// During an export only the agent access stays usable, so the user can revoke it at any time.
fn file_menu(ui: &mut egui::Ui, language: Language, theme_choice: theme::ThemeChoice, agent_access: bool, exporting: bool, requests: &mut Vec<UiRequest>) {
    ui.menu_button(language.t(Message::MenuFile), |ui| {
        ui.add_enabled_ui(!exporting, |ui| {
            for (label, request) in [(Message::MenuNewProject, UiRequest::NewProject), (Message::MenuOpen, UiRequest::OpenProject), (Message::MenuSave, UiRequest::SaveProject), (Message::MenuSaveAs, UiRequest::SaveProjectAs)] {
                if ui.button(language.t(label)).clicked() {
                    requests.push(request);
                    ui.close();
                }
            }
        });
        ui.separator();
        let mut access = agent_access;
        if ui.checkbox(&mut access, language.t(Message::MenuAgentAccess)).clicked() {
            requests.push(UiRequest::SetAgentAccess(access));
            ui.close();
        }
        ui.separator();
        ui.add_enabled_ui(!exporting, |ui| {
            ui.menu_button(language.t(Message::MenuLanguage), |ui| {
                for choice in Language::ALL {
                    if ui.radio(choice == language, choice.native_name()).clicked() {
                        requests.push(UiRequest::SetLanguage(choice));
                        ui.close();
                    }
                }
            });
            ui.menu_button(language.t(Message::MenuTheme), |ui| {
                for choice in theme::ThemeChoice::ALL {
                    let label = language.t(match choice {
                        theme::ThemeChoice::System => Message::ThemeSystem,
                        theme::ThemeChoice::Dark => Message::ThemeDark,
                        theme::ThemeChoice::Light => Message::ThemeLight,
                    });
                    let mut current = choice == theme_choice;
                    if ui.checkbox(&mut current, label).clicked() {
                        requests.push(UiRequest::SetTheme(choice));
                        ui.close();
                    }
                }
            });
        });
    });
}

fn export_area(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let language = state.language();
    if let Some(progress) = state.export_progress() {
        if ui.button(language.t(Message::Cancel)).clicked() {
            requests.push(UiRequest::CancelExport);
        }
        ui.add(egui::ProgressBar::new(progress.fraction()).desired_width(EXPORT_BAR_WIDTH).text(language.t(Message::ExportProgress { done: progress.done, total: progress.total })));
        return;
    }
    if icons::label_button(ui, Icon::Export, &language.t(Message::ExportVideo), theme::accent(), theme::on_accent(), None).clicked() {
        requests.push(UiRequest::Export);
    }
    let selected = state.encoders.get(state.encoder).map_or_else(String::new, |option| language.text(&option.label));
    let options = state.encoders.clone();
    egui::ComboBox::from_id_salt("encoder").width(150.0).selected_text(selected).show_ui(ui, |ui| {
        for (index, option) in options.iter().enumerate() {
            ui.add_enabled_ui(option.available, |ui| {
                ui.selectable_value(&mut state.encoder, index, language.text(&option.label));
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
