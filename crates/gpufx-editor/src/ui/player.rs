//! The centre panel: the picture and the transport row under it.

use super::icons::{self, Icon};
use super::{placement, theme, UiRequest};
use crate::editor::{EditorState, Status, OUTPUT_FRAME_DURATIONS, OUTPUT_PRESETS};
use crate::i18n::Message;
use crate::presenter::PreviewTexture;
use crate::preview::plan::fit_size;
use crate::timecode::{aspect_label, format_fps, format_timecode, fps_from_frame_duration, frame_duration_from_fps};
use crate::timeline::view::content_end;
use crate::media::MediaItem;
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
const FULLSCREEN_ERROR_GAP: f32 = 8.0;
const FULLSCREEN_ERROR_PADDING: f32 = 8.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, preview: Option<PreviewTexture>) {
    let rect = ui.max_rect();
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_HEIGHT.min(rect.height())));
    let controls = Rect::from_min_max(Pos2::new(rect.left(), (rect.bottom() - CONTROLS_HEIGHT).max(header.bottom())), rect.max);
    let area = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), Pos2::new(rect.right(), controls.top()));

    let painter = ui.painter_at(rect);
    painter.text(header.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, state.language().t(Message::Player), FontId::proportional(14.0), theme::text());
    painter.hline(rect.x_range(), header.bottom(), Stroke::new(1.0, theme::border()));
    draw_picture(ui, area, preview);
    let output = &state.project().output;
    let frame = picture_rect(area, (output.width, output.height));
    placement::show(ui, state, area, frame);
    handle_sticker_drop(ui, state, frame);
    painter.hline(rect.x_range(), controls.top(), Stroke::new(1.0, theme::border()));
    ui.scope_builder(UiBuilder::new().max_rect(controls.shrink2(Vec2::new(10.0, 0.0))), |ui| transport(ui, state, requests, controls));
}

/// Takes a sticker of the library released over the picture: it goes on a new track from the
/// playhead, where it fell. While one is held there, the picture is outlined and the sticker
/// follows the pointer at the size it will take.
fn handle_sticker_drop(ui: &egui::Ui, state: &mut EditorState, frame: Rect) {
    let ctx = ui.ctx().clone();
    let Some(item) = egui::DragAndDrop::payload::<MediaItem>(&ctx).filter(|item| crate::media::is_sticker(&item.source)) else { return };
    let Some(pointer) = ctx.pointer_latest_pos().filter(|pointer| frame.contains(*pointer)) else { return };
    if ctx.input(|input| input.pointer.any_released()) {
        if egui::DragAndDrop::take_payload::<MediaItem>(&ctx).is_some() {
            let center = (f64::from((pointer.x - frame.left()) / frame.width().max(1.0)), f64::from((pointer.y - frame.top()) / frame.height().max(1.0)));
            state.drop_sticker_on_player(&item, center);
        }
        return;
    }
    let painter = ui.painter_at(frame);
    painter.rect_stroke(frame.shrink(1.0), CornerRadius::ZERO, Stroke::new(2.0, theme::accent()), egui::StrokeKind::Inside);
    let poster = state.visuals.borrow_mut().poster(&item.source, None);
    let aspect = state.sticker_library.aspect_of(&item.source).map(|aspect| aspect as f32).or(poster.map(|(_, aspect)| aspect)).unwrap_or(1.0).max(0.01);
    let width = frame.width() * crate::editor::STICKER_WIDTH as f32;
    let ghost = Rect::from_center_size(pointer, Vec2::new(width, width / aspect));
    if let Some((texture, _)) = poster {
        painter.image(texture, ghost, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::from_white_alpha(200));
    }
    painter.rect_stroke(ghost, CornerRadius::ZERO, Stroke::new(1.5, theme::accent()), egui::StrokeKind::Outside);
    ctx.request_repaint();
}

/// Where the controls float over the picture in fullscreen.
pub(super) fn fullscreen_bar(screen: Rect) -> Rect {
    let width = FULLSCREEN_BAR_WIDTH.min(screen.width() - 2.0 * FULLSCREEN_BAR_MARGIN).max(0.0);
    let bottom = (screen.bottom() - FULLSCREEN_BAR_MARGIN).max(screen.top() + CONTROLS_HEIGHT);
    Rect::from_center_size(Pos2::new(screen.center().x, bottom - CONTROLS_HEIGHT / 2.0), Vec2::new(width, CONTROLS_HEIGHT))
}

fn controls_visible(idle_seconds: f64, over_bar: bool, has_error: bool) -> bool {
    has_error || over_bar || idle_seconds < FULLSCREEN_CONTROLS_SECONDS
}

/// When to look again so the controls can go. Nothing is due once they have stayed past their time.
fn hide_repaint_after(idle_seconds: f64, has_error: bool) -> Option<f32> {
    (!has_error && idle_seconds < FULLSCREEN_CONTROLS_SECONDS).then(|| (FULLSCREEN_CONTROLS_SECONDS - idle_seconds).max(0.05) as f32)
}

/// The failures that stand now, one a line, for the fullscreen player, which has no status line.
fn fullscreen_errors(state: &EditorState) -> Option<String> {
    let language = state.language();
    let mut lines = Vec::new();
    if let Status::Error(message) = state.status() {
        lines.push(language.text(message));
    }
    lines.extend(state.preview_error().map(|message| language.t(Message::PreviewLine(message.to_owned()))));
    lines.extend(state.playback_message().map(|message| language.t(Message::AudioLine(message.to_owned()))));
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Paints `errors` just above the fullscreen bar.
fn draw_fullscreen_errors(ui: &egui::Ui, bar: Rect, screen: Rect, errors: String) {
    let galley = ui.painter().layout(errors, FontId::proportional(14.0), theme::refused(), (bar.width() - 2.0 * FULLSCREEN_ERROR_PADDING).max(1.0));
    let size = galley.size() + Vec2::splat(2.0 * FULLSCREEN_ERROR_PADDING);
    let top = (bar.top() - FULLSCREEN_ERROR_GAP - size.y).max(screen.top());
    let panel = Rect::from_min_size(Pos2::new(bar.center().x - size.x / 2.0, top), size);
    ui.painter().rect_filled(panel, CornerRadius::same(theme::PANEL_RADIUS), theme::panel().gamma_multiply(0.9));
    ui.painter().galley(panel.min + Vec2::splat(FULLSCREEN_ERROR_PADDING), galley, theme::refused());
}

/// The picture on the whole screen, letterboxed, with the transport floating over it until the
/// pointer has been still for a while. A failure stays on screen with the controls until it is over.
pub fn show_fullscreen(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, preview: Option<PreviewTexture>) {
    let screen = ui.max_rect();
    draw_picture(ui, screen, preview);
    let bar = fullscreen_bar(screen);
    let (now, pointer) = ui.ctx().input(|input| (input.time, input.pointer.latest_pos()));
    let idle = state.ui.fullscreen_activity.map_or(0.0, |at| now - at);
    let errors = fullscreen_errors(state);
    if !controls_visible(idle, pointer.is_some_and(|at| bar.contains(at)), errors.is_some()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        return;
    }
    // Nothing else may be due when the controls should go.
    if let Some(seconds) = hide_repaint_after(idle, errors.is_some()) {
        ui.ctx().request_repaint_after_secs(seconds);
    }
    if let Some(errors) = errors {
        draw_fullscreen_errors(ui, bar, screen, errors);
    }
    ui.painter().rect_filled(bar, CornerRadius::same(theme::PANEL_RADIUS), theme::panel().gamma_multiply(0.9));
    ui.scope_builder(UiBuilder::new().id_salt("fullscreen-transport").max_rect(bar.shrink2(Vec2::new(10.0, 0.0))), |ui| transport(ui, state, requests, bar));
}

/// Where a picture of `size` pixels lands in `area`, letterboxed.
fn picture_rect(area: Rect, size: (u32, u32)) -> Rect {
    let (width, height) = fit_size(size, (area.width(), area.height()));
    Rect::from_center_size(area.center(), Vec2::new(width, height))
}

fn draw_picture(ui: &egui::Ui, rect: Rect, preview: Option<PreviewTexture>) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
    let Some(texture) = preview else { return };
    let picture = picture_rect(rect, (texture.width, texture.height));
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
    let size = format!("{}  {}", size_label(output.width, output.height), format_fps(frame_duration));
    let current = format_timecode(state.playhead(), frame_duration);
    let total = format_timecode(content_end(state.project()), frame_duration);
    let total = format!("/ {total}");
    let width_of = |text: &str, style: egui::TextStyle| ui.painter().layout_no_wrap(text.to_owned(), style.resolve(ui.style()), theme::text()).size().x;
    let beside_play = controls.width() / 2.0 - PLAY_HALF_WIDTH - ui.spacing().item_spacing.x;
    let (show_total, show_aspect) = transport_fit(beside_play, width_of(&current, egui::TextStyle::Monospace), width_of(&total, egui::TextStyle::Monospace), width_of(&size, egui::TextStyle::Body), ui.spacing().item_spacing.x);
    ui.allocate_ui_with_layout(ui.max_rect().size(), Layout::left_to_right(Align::Center), |ui| {
        ui.colored_label(theme::accent_text(), egui::RichText::new(current).monospace());
        if show_total {
            ui.colored_label(theme::text_dim(), egui::RichText::new(total).monospace());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, tip) = if state.ui.fullscreen { (Icon::Exit, Message::ExitFullscreen) } else { (Icon::Fullscreen, Message::Fullscreen) };
            if icons::button(ui, icon, &state.language().t(tip), state.ui.fullscreen).clicked() {
                requests.push(UiRequest::ToggleFullscreen);
            }
            if show_aspect {
                output_size_menu(ui, state, size);
            }
        });
    });
    let button = Rect::from_center_size(controls.center(), Vec2::splat(PLAY_HALF_WIDTH * 2.0));
    let enabled = !state.is_exporting();
    let response = ui.interact(button, ui.scope_id().with("play"), if enabled { Sense::click() } else { Sense::hover() });
    if response.hovered() && enabled {
        ui.painter().rect_filled(button, CornerRadius::same(16), theme::panel_raised());
    }
    let icon = if state.is_playing() { Icon::Stop } else { Icon::Play };
    let tint = if enabled { theme::text() } else { theme::tick() };
    icons::paint(ui.painter(), button.shrink(7.0), icon, tint);
    if response.on_hover_text(state.language().t(if state.is_playing() { Message::StopShortcut } else { Message::PlayShortcut })).clicked() {
        state.toggle_playback();
    }
}

fn size_label(width: u32, height: u32) -> String {
    format!("{width}\u{d7}{height}")
}

/// The size and frame rate of the output: lists of usual values and a width and height to type.
fn output_size_menu(ui: &mut egui::Ui, state: &mut EditorState, label: String) {
    let output = &state.project().output;
    let (width, height, frame_duration) = (output.width, output.height, output.frame_duration);
    let language = state.language();
    let draft_id = ui.scope_id().with("output-size-draft");
    let fps_draft_id = ui.scope_id().with("output-fps-draft");
    let enabled = !state.is_exporting();
    let menu = ui.add_enabled_ui(enabled, |ui| {
        ui.menu_button(label, |ui| {
            for (preset_width, preset_height) in OUTPUT_PRESETS {
                let text = format!("{}  ({})", size_label(preset_width, preset_height), aspect_label(preset_width, preset_height));
                if ui.selectable_label((preset_width, preset_height) == (width, height), text).clicked() {
                    state.set_output_size(preset_width, preset_height);
                    ui.close();
                }
            }
            ui.separator();
            ui.colored_label(theme::text_dim(), language.t(Message::OutputSizeCustom));
            let (mut draft_width, mut draft_height) = ui.data(|data| data.get_temp::<(u32, u32)>(draft_id)).unwrap_or((width, height));
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut draft_width).range(2..=7680).speed(2));
                ui.label("\u{d7}");
                ui.add(egui::DragValue::new(&mut draft_height).range(2..=7680).speed(2));
            });
            ui.data_mut(|data| data.insert_temp(draft_id, (draft_width, draft_height)));
            if ui.button(language.t(Message::Apply)).clicked() {
                state.set_output_size(draft_width, draft_height);
                ui.close();
            }
            ui.separator();
            ui.colored_label(theme::text_dim(), language.t(Message::FieldFrameRate));
            for preset in OUTPUT_FRAME_DURATIONS {
                if ui.selectable_label(preset == frame_duration, format_fps(preset)).clicked() {
                    state.set_frame_duration(preset);
                    ui.close();
                }
            }
            ui.separator();
            ui.colored_label(theme::text_dim(), language.t(Message::FrameRateCustom));
            let mut draft_fps = ui.data(|data| data.get_temp::<f64>(fps_draft_id)).unwrap_or_else(|| fps_from_frame_duration(frame_duration));
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut draft_fps).speed(0.1).max_decimals(3).suffix(" fps"));
            });
            ui.data_mut(|data| data.insert_temp(fps_draft_id, draft_fps));
            // Only 1 to 60 fps is taken; anything else leaves Apply off instead of being clamped.
            let custom = frame_duration_from_fps(draft_fps);
            if ui.add_enabled(custom.is_some(), egui::Button::new(language.t(Message::Apply))).clicked() {
                if let Some(custom) = custom {
                    state.set_frame_duration(custom);
                    ui.close();
                }
            }
        })
    });
    // What was typed and not applied does not come back the next time the menu opens.
    if menu.inner.inner.is_none() {
        ui.data_mut(|data| {
            data.remove_temp::<(u32, u32)>(draft_id);
            data.remove_temp::<f64>(fps_draft_id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fullscreen_controls_go_after_a_while_unless_the_pointer_is_on_them() {
        assert!(controls_visible(0.0, false, false));
        assert!(controls_visible(2.9, false, false));
        assert!(!controls_visible(3.0, false, false));
        assert!(controls_visible(100.0, true, false));
    }

    #[test]
    fn a_standing_error_keeps_the_fullscreen_controls() {
        assert!(controls_visible(100.0, false, true));
    }

    #[test]
    fn a_repaint_is_due_only_until_the_controls_have_had_their_time() {
        assert_eq!(hide_repaint_after(1.0, false), Some(2.0));
        assert_eq!(hide_repaint_after(2.99, false), Some(0.05));
        assert_eq!(hide_repaint_after(3.0, false), None);
        assert_eq!(hide_repaint_after(100.0, false), None);
        assert_eq!(hide_repaint_after(1.0, true), None, "an error keeps them, so nothing is due");
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
