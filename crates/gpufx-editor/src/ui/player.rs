//! The centre panel: the picture and the transport row under it.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::EditorState;
use crate::presenter::PreviewTexture;
use crate::preview::plan::fit_size;
use crate::timecode::{aspect_label, format_timecode};
use crate::timeline::view::content_end;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};

const HEADER_HEIGHT: f32 = 34.0;
const CONTROLS_HEIGHT: f32 = 40.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, preview: Option<PreviewTexture>) {
    let rect = ui.max_rect();
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_HEIGHT.min(rect.height())));
    let controls = Rect::from_min_max(Pos2::new(rect.left(), (rect.bottom() - CONTROLS_HEIGHT).max(header.bottom())), rect.max);
    let picture = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), Pos2::new(rect.right(), controls.top()));

    let painter = ui.painter_at(rect);
    painter.text(header.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, "Trình phát", FontId::proportional(14.0), theme::TEXT);
    painter.hline(rect.x_range(), header.bottom(), Stroke::new(1.0, theme::BORDER));
    draw_picture(ui, picture, preview);
    painter.hline(rect.x_range(), controls.top(), Stroke::new(1.0, theme::BORDER));
    ui.scope_builder(UiBuilder::new().max_rect(controls.shrink2(Vec2::new(10.0, 0.0))), |ui| transport(ui, state, requests, controls));
}

fn draw_picture(ui: &egui::Ui, rect: Rect, preview: Option<PreviewTexture>) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
    let Some(texture) = preview else { return };
    let (width, height) = fit_size((texture.width, texture.height), (rect.width(), rect.height()));
    let picture = Rect::from_center_size(rect.center(), Vec2::new(width, height));
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.image(texture.id, picture, uv, Color32::WHITE);
}

fn transport(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, controls: Rect) {
    let output = &state.project().output;
    let frame_duration = output.frame_duration;
    let aspect = aspect_label(output.width, output.height);
    let current = format_timecode(state.playhead(), frame_duration);
    let total = format_timecode(content_end(state.project()), frame_duration);
    ui.allocate_ui_with_layout(ui.max_rect().size(), Layout::left_to_right(Align::Center), |ui| {
        ui.colored_label(theme::ACCENT_TEXT, egui::RichText::new(current).monospace());
        ui.colored_label(theme::TEXT_DIM, egui::RichText::new(format!("/ {total}")).monospace());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, tip) = if state.ui.fullscreen { (Icon::Exit, "Thoát toàn màn hình") } else { (Icon::Fullscreen, "Toàn màn hình") };
            if icons::button(ui, icon, tip, state.ui.fullscreen).clicked() {
                requests.push(UiRequest::ToggleFullscreen);
            }
            ui.colored_label(theme::TEXT_DIM, aspect);
        });
    });
    let button = Rect::from_center_size(controls.center(), Vec2::splat(32.0));
    let enabled = !state.is_exporting();
    let response = ui.interact(button, ui.scope_id().with("play"), if enabled { Sense::click() } else { Sense::hover() });
    if response.hovered() && enabled {
        ui.painter().rect_filled(button, CornerRadius::same(16), theme::PANEL_RAISED);
    }
    let icon = if state.is_playing() { Icon::Stop } else { Icon::Play };
    let tint = if enabled { theme::TEXT } else { theme::TICK };
    icons::paint(ui.painter(), button.shrink(7.0), icon, tint);
    if response.on_hover_text(if state.is_playing() { "Dừng (Space)" } else { "Phát (Space)" }).clicked() {
        state.toggle_playback();
    }
}
