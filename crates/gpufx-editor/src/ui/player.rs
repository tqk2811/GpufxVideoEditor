//! The centre panel: the picture and the transport row under it.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::EditorState;
use crate::i18n::Message;
use crate::presenter::PreviewTexture;
use crate::preview::plan::fit_size;
use crate::timecode::{aspect_label, format_timecode};
use crate::timeline::view::content_end;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};

const HEADER_HEIGHT: f32 = 34.0;
const CONTROLS_HEIGHT: f32 = 40.0;
const PLAY_HALF_WIDTH: f32 = 16.0;
/// The fullscreen button, with what keeps the row from touching the panel edge.
const FULLSCREEN_WIDTH: f32 = 40.0;
const FULLSCREEN_BAR_WIDTH: f32 = 560.0;
const FULLSCREEN_BAR_MARGIN: f32 = 24.0;
/// How long the fullscreen controls stay with no pointer movement and no key press.
const FULLSCREEN_CONTROLS_SECONDS: f64 = 3.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, preview: Option<PreviewTexture>) {
    let rect = ui.max_rect();
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_HEIGHT.min(rect.height())));
    let controls = Rect::from_min_max(Pos2::new(rect.left(), (rect.bottom() - CONTROLS_HEIGHT).max(header.bottom())), rect.max);
    let picture = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), Pos2::new(rect.right(), controls.top()));

    let painter = ui.painter_at(rect);
    painter.text(header.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, state.language().t(Message::Player), FontId::proportional(14.0), theme::TEXT);
    painter.hline(rect.x_range(), header.bottom(), Stroke::new(1.0, theme::BORDER));
    draw_picture(ui, picture, preview);
    painter.hline(rect.x_range(), controls.top(), Stroke::new(1.0, theme::BORDER));
    ui.scope_builder(UiBuilder::new().max_rect(controls.shrink2(Vec2::new(10.0, 0.0))), |ui| transport(ui, state, requests, controls));
}

/// Where the controls float over the picture in fullscreen.
pub(super) fn fullscreen_bar(screen: Rect) -> Rect {
    let width = FULLSCREEN_BAR_WIDTH.min(screen.width() - 2.0 * FULLSCREEN_BAR_MARGIN).max(0.0);
    let bottom = (screen.bottom() - FULLSCREEN_BAR_MARGIN).max(screen.top() + CONTROLS_HEIGHT);
    Rect::from_center_size(Pos2::new(screen.center().x, bottom - CONTROLS_HEIGHT / 2.0), Vec2::new(width, CONTROLS_HEIGHT))
}

fn controls_visible(idle_seconds: f64, over_bar: bool) -> bool {
    over_bar || idle_seconds < FULLSCREEN_CONTROLS_SECONDS
}

/// The picture on the whole screen, letterboxed, with the transport floating over it until the
/// pointer has been still for a while.
pub fn show_fullscreen(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, preview: Option<PreviewTexture>) {
    let screen = ui.max_rect();
    draw_picture(ui, screen, preview);
    let bar = fullscreen_bar(screen);
    let (now, pointer) = ui.ctx().input(|input| (input.time, input.pointer.latest_pos()));
    let idle = state.ui.fullscreen_activity.map_or(0.0, |at| now - at);
    if !controls_visible(idle, pointer.is_some_and(|at| bar.contains(at))) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        return;
    }
    // Nothing else may be due when the controls should go.
    ui.ctx().request_repaint_after_secs((FULLSCREEN_CONTROLS_SECONDS - idle).max(0.05) as f32);
    ui.painter().rect_filled(bar, CornerRadius::same(theme::PANEL_RADIUS), theme::PANEL.gamma_multiply(0.9));
    ui.scope_builder(UiBuilder::new().id_salt("fullscreen-transport").max_rect(bar.shrink2(Vec2::new(10.0, 0.0))), |ui| transport(ui, state, requests, bar));
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

/// Which optional parts of the transport fit beside the centred play button: the total time on the
/// left, the aspect ratio on the right. `beside_play` is the room on each side of it.
fn transport_fit(beside_play: f32, current: f32, total: f32, aspect: f32, gap: f32) -> (bool, bool) {
    (current + gap + total <= beside_play, FULLSCREEN_WIDTH + gap + aspect <= beside_play)
}

fn transport(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, controls: Rect) {
    let output = &state.project().output;
    let frame_duration = output.frame_duration;
    let aspect = aspect_label(output.width, output.height);
    let current = format_timecode(state.playhead(), frame_duration);
    let total = format_timecode(content_end(state.project()), frame_duration);
    let total = format!("/ {total}");
    let width_of = |text: &str, style: egui::TextStyle| ui.painter().layout_no_wrap(text.to_owned(), style.resolve(ui.style()), theme::TEXT).size().x;
    let beside_play = controls.width() / 2.0 - PLAY_HALF_WIDTH - ui.spacing().item_spacing.x;
    let (show_total, show_aspect) = transport_fit(beside_play, width_of(&current, egui::TextStyle::Monospace), width_of(&total, egui::TextStyle::Monospace), width_of(&aspect, egui::TextStyle::Body), ui.spacing().item_spacing.x);
    ui.allocate_ui_with_layout(ui.max_rect().size(), Layout::left_to_right(Align::Center), |ui| {
        ui.colored_label(theme::ACCENT_TEXT, egui::RichText::new(current).monospace());
        if show_total {
            ui.colored_label(theme::TEXT_DIM, egui::RichText::new(total).monospace());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, tip) = if state.ui.fullscreen { (Icon::Exit, Message::ExitFullscreen) } else { (Icon::Fullscreen, Message::Fullscreen) };
            if icons::button(ui, icon, &state.language().t(tip), state.ui.fullscreen).clicked() {
                requests.push(UiRequest::ToggleFullscreen);
            }
            if show_aspect {
                ui.colored_label(theme::TEXT_DIM, aspect);
            }
        });
    });
    let button = Rect::from_center_size(controls.center(), Vec2::splat(PLAY_HALF_WIDTH * 2.0));
    let enabled = !state.is_exporting();
    let response = ui.interact(button, ui.scope_id().with("play"), if enabled { Sense::click() } else { Sense::hover() });
    if response.hovered() && enabled {
        ui.painter().rect_filled(button, CornerRadius::same(16), theme::PANEL_RAISED);
    }
    let icon = if state.is_playing() { Icon::Stop } else { Icon::Play };
    let tint = if enabled { theme::TEXT } else { theme::TICK };
    icons::paint(ui.painter(), button.shrink(7.0), icon, tint);
    if response.on_hover_text(state.language().t(if state.is_playing() { Message::StopShortcut } else { Message::PlayShortcut })).clicked() {
        state.toggle_playback();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fullscreen_controls_go_after_a_while_unless_the_pointer_is_on_them() {
        assert!(controls_visible(0.0, false));
        assert!(controls_visible(2.9, false));
        assert!(!controls_visible(3.0, false));
        assert!(controls_visible(100.0, true));
    }

    #[test]
    fn the_fullscreen_bar_stays_on_a_tiny_screen() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 60.0));
        let bar = fullscreen_bar(screen);
        assert!(bar.width() <= 100.0 && bar.height() == CONTROLS_HEIGHT && bar.top() >= 0.0, "{bar:?}");
    }

    #[test]
    fn the_optional_parts_of_the_transport_go_when_they_would_reach_the_play_button() {
        assert_eq!(transport_fit(300.0, 90.0, 110.0, 40.0, 6.0), (true, true));
        assert_eq!(transport_fit(150.0, 90.0, 110.0, 40.0, 6.0), (false, true));
        assert_eq!(transport_fit(60.0, 90.0, 110.0, 40.0, 6.0), (false, false));
    }
}
