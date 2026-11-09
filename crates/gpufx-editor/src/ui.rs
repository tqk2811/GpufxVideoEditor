//! Draws the editor and forwards input. Decisions live in `editor` and `timeline`.

use crate::editor::{clip_label, EditorState, Status};
use crate::timeline::drag::{begin_drag, drag_command};
use crate::timeline::hit::{hit_test, ClipZone};
use crate::timeline::ruler::{format_time, major_step, ruler_ticks};
use crate::timeline::view::{content_end, ViewArea, Wheel, TRACK_HEIGHT};
use egui::{Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Key, Modifiers, MouseWheelUnit, Pos2, Rect, Sense, Stroke, StrokeKind, UiBuilder, Vec2};
use engine::{Time, TrackKind};

const HEADER_WIDTH: f32 = 130.0;
const RULER_HEIGHT: f32 = 28.0;
const LINE_SCROLL_POINTS: f32 = 50.0;

const BACKGROUND: Color32 = Color32::from_rgb(26, 26, 30);
const ROW_EVEN: Color32 = Color32::from_rgb(36, 36, 42);
const ROW_ODD: Color32 = Color32::from_rgb(42, 42, 48);
const RULER: Color32 = Color32::from_rgb(52, 52, 60);
const TICK: Color32 = Color32::from_rgb(150, 150, 160);
const VIDEO_CLIP: Color32 = Color32::from_rgb(62, 108, 170);
const AUDIO_CLIP: Color32 = Color32::from_rgb(58, 138, 96);
const SELECTED: Color32 = Color32::from_rgb(250, 210, 90);
const PLAYHEAD: Color32 = Color32::from_rgb(235, 80, 70);
const REFUSED: Color32 = Color32::from_rgb(255, 120, 110);

/// Work the window does after the frame, outside the UI pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRequest {
    PickClipFile { track: usize },
}

pub fn show(root: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    handle_shortcuts(root.ctx(), state);
    egui::Panel::top("toolbar").show(root, |ui| toolbar(ui, state));
    egui::Panel::bottom("status").show(root, |ui| status_line(ui, state));
    egui::CentralPanel::default().show(root, |ui| timeline(ui, state, requests));
}

fn handle_shortcuts(ctx: &egui::Context, state: &mut EditorState) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    ctx.input_mut(|input| {
        if input.consume_key(Modifiers::COMMAND, Key::Z) {
            state.undo();
        }
        if input.consume_key(Modifiers::COMMAND, Key::Y) {
            state.redo();
        }
        if input.consume_key(Modifiers::NONE, Key::S) {
            state.split_selected();
        }
        if input.consume_key(Modifiers::NONE, Key::Delete) {
            state.delete_selected();
        }
        // Reserved for playback.
        input.consume_key(Modifiers::NONE, Key::Space);
    });
}

fn toolbar(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        if ui.button("Add video track").clicked() {
            state.add_track(TrackKind::Video);
        }
        if ui.button("Add audio track").clicked() {
            state.add_track(TrackKind::Audio);
        }
        ui.separator();
        if ui.add_enabled(state.can_undo(), egui::Button::new("Undo")).clicked() {
            state.undo();
        }
        if ui.add_enabled(state.can_redo(), egui::Button::new("Redo")).clicked() {
            state.redo();
        }
        ui.separator();
        ui.label(format!("Playhead {}", format_time(state.playhead(), 1)));
    });
}

fn status_line(ui: &mut egui::Ui, state: &EditorState) {
    match state.status() {
        Status::Idle => ui.label("Ready"),
        Status::Info(message) => ui.label(message),
        Status::Refused(message) => ui.colored_label(REFUSED, format!("Refused: {message}")),
    };
}

fn timeline(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let tracks_left = rect.left() + HEADER_WIDTH;
    let ruler_rect = Rect::from_min_max(Pos2::new(tracks_left, rect.top()), Pos2::new(rect.right(), rect.top() + RULER_HEIGHT));
    let tracks_rect = Rect::from_min_max(Pos2::new(tracks_left, ruler_rect.bottom()), rect.max);
    let header_rect = Rect::from_min_max(Pos2::new(rect.left(), ruler_rect.bottom()), Pos2::new(tracks_left, rect.bottom()));

    let area = ViewArea {
        content_end: content_end(state.project()),
        width: f64::from(tracks_rect.width()),
        track_count: state.project().tracks.len(),
        rows_height: f64::from(tracks_rect.height()),
    };
    state.view.clamp_scroll(&area);
    handle_wheel(ui, state, rect, tracks_rect, &area);

    let ruler_response = ui.interact(ruler_rect, ui.scope_id().with("ruler"), Sense::click_and_drag());
    if ruler_response.is_pointer_button_down_on() {
        if let Some(pointer) = ruler_response.interact_pointer_pos() {
            state.set_playhead(state.view.x_to_time(f64::from(pointer.x - tracks_left)));
        }
    }

    let tracks_response = ui.interact(tracks_rect, ui.scope_id().with("tracks"), Sense::click_and_drag());
    handle_tracks(ui, state, &tracks_response, tracks_rect);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
    draw_rows(ui, state, tracks_rect);
    draw_ruler(&painter, state, ruler_rect);
    headers(ui, state, header_rect, requests);
    draw_playhead(&painter, state, ruler_rect, tracks_rect);
}

fn handle_wheel(ui: &egui::Ui, state: &mut EditorState, rect: Rect, tracks_rect: Rect, area: &ViewArea) {
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    let cursor_x = ui.ctx().pointer_latest_pos().map_or(0.0, |p| f64::from(p.x - tracks_rect.left()));
    let view = &mut state.view;
    ui.ctx().input_mut(|input| {
        let mut consumed = false;
        input.events.retain(|event| {
            let Event::MouseWheel { unit, delta, modifiers, .. } = event else {
                return true;
            };
            let points = match unit {
                MouseWheelUnit::Point => *delta,
                MouseWheelUnit::Line => *delta * LINE_SCROLL_POINTS,
                MouseWheelUnit::Page => Vec2::new(delta.x * tracks_rect.width(), delta.y * tracks_rect.height()),
            };
            let wheel = Wheel {
                ctrl: modifiers.command,
                shift: modifiers.shift,
                delta_x: f64::from(points.x),
                delta_y: f64::from(points.y),
                cursor_x,
            };
            let used = view.apply_wheel(wheel, area);
            consumed |= used;
            !used
        });
        if consumed {
            input.smooth_scroll_delta = Vec2::ZERO;
        }
    });
}

fn handle_tracks(ui: &egui::Ui, state: &mut EditorState, response: &egui::Response, tracks_rect: Rect) {
    let track_count = state.project().tracks.len();
    let local = |pos: Pos2| (f64::from(pos.x - tracks_rect.left()), f64::from(pos.y - tracks_rect.top()));

    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let (x, y) = local(pointer);
            let hit = state.view.track_at_y(y, track_count).and_then(|track| hit_test(state.project(), &state.view, track, x));
            state.select(hit.map(|hit| hit.key));
        }
    }

    if response.drag_started() {
        let press = ui.ctx().input(|input| input.pointer.press_origin());
        if let Some(press) = press {
            let (x, y) = local(press);
            let hit = state.view.track_at_y(y, track_count).and_then(|track| hit_test(state.project(), &state.view, track, x));
            if let Some(hit) = hit {
                state.select(Some(hit.key));
                state.drag = begin_drag(state.project(), hit, state.view.x_to_time(x));
            }
        }
    }

    if response.drag_stopped() {
        if let (Some(drag), Some(pointer)) = (state.drag, response.interact_pointer_pos()) {
            let (x, y) = local(pointer);
            let command = drag_command(&drag, state.view.x_to_time(x), state.view.track_at_y(y, track_count));
            state.finish_drag(command);
        }
        state.drag = None;
    }

    let cursor = if state.drag.is_some() {
        Some(CursorIcon::Grabbing)
    } else {
        response.hover_pos().and_then(|pointer| {
            let (x, y) = local(pointer);
            let track = state.view.track_at_y(y, track_count)?;
            let hit = hit_test(state.project(), &state.view, track, x)?;
            Some(match hit.zone {
                ClipZone::Body => CursorIcon::Grab,
                ClipZone::StartEdge | ClipZone::EndEdge => CursorIcon::ResizeHorizontal,
            })
        })
    };
    if let Some(cursor) = cursor {
        ui.ctx().set_cursor_icon(cursor);
    }
}

fn row_rect(state: &EditorState, tracks_rect: Rect, track: usize, track_count: usize) -> Rect {
    let top = tracks_rect.top() + state.view.track_top(track, track_count) as f32;
    Rect::from_min_size(Pos2::new(tracks_rect.left(), top), Vec2::new(tracks_rect.width(), TRACK_HEIGHT as f32))
}

fn draw_rows(ui: &egui::Ui, state: &EditorState, tracks_rect: Rect) {
    let painter = ui.painter_at(tracks_rect);
    let project = state.project();
    let track_count = project.tracks.len();
    for (t, track) in project.tracks.iter().enumerate() {
        let row = row_rect(state, tracks_rect, t, track_count);
        painter.rect_filled(row, CornerRadius::ZERO, if t % 2 == 0 { ROW_EVEN } else { ROW_ODD });
        let fill = if track.kind == TrackKind::Video { VIDEO_CLIP } else { AUDIO_CLIP };
        for (c, clip) in track.clips.iter().enumerate() {
            let key = engine::ClipKey { track: t, clip: c };
            let rect = span_rect(state, tracks_rect, row, clip.start, clip.start.saturating_add(clip.duration));
            if !rect.intersects(tracks_rect) {
                continue;
            }
            let dragged = state.drag.is_some_and(|drag| drag.key == key);
            let colour = if dragged { fill.gamma_multiply(0.5) } else { fill };
            painter.rect_filled(rect, CornerRadius::same(3), colour);
            if state.selection() == Some(key) {
                painter.rect_stroke(rect, CornerRadius::same(3), Stroke::new(2.0, SELECTED), StrokeKind::Inside);
            }
            let text = painter.with_clip_rect(rect.intersect(tracks_rect));
            text.text(rect.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, clip_label(&clip.source), FontId::proportional(12.0), Color32::WHITE);
        }
    }
    draw_drag_preview(ui, state, tracks_rect);
}

fn span_rect(state: &EditorState, tracks_rect: Rect, row: Rect, start: Time, end: Time) -> Rect {
    let left = tracks_rect.left() + state.view.time_to_x(start) as f32;
    let right = (tracks_rect.left() + state.view.time_to_x(end) as f32).max(left + 1.0);
    Rect::from_min_max(Pos2::new(left, row.top() + 3.0), Pos2::new(right, row.bottom() - 3.0))
}

fn draw_drag_preview(ui: &egui::Ui, state: &EditorState, tracks_rect: Rect) {
    let Some(drag) = state.drag else { return };
    let Some(pointer) = ui.ctx().pointer_latest_pos() else { return };
    let track_count = state.project().tracks.len();
    let x = f64::from(pointer.x - tracks_rect.left());
    let y = f64::from(pointer.y - tracks_rect.top());
    let command = drag_command(&drag, state.view.x_to_time(x), state.view.track_at_y(y, track_count));
    let Some(span) = command.landing(state.project()) else { return };
    let row = row_rect(state, tracks_rect, span.track, track_count);
    let rect = span_rect(state, tracks_rect, row, span.start, span.end);
    ui.painter_at(tracks_rect).rect_stroke(rect, CornerRadius::same(3), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
}

fn draw_ruler(painter: &egui::Painter, state: &EditorState, ruler_rect: Rect) {
    let ruler = painter.with_clip_rect(ruler_rect);
    ruler.rect_filled(ruler_rect, CornerRadius::ZERO, RULER);
    let step = major_step(&state.view);
    for tick in ruler_ticks(&state.view, f64::from(ruler_rect.width())) {
        let x = ruler_rect.left() + tick.x as f32;
        let height = if tick.major { 12.0 } else { 6.0 };
        ruler.line_segment([Pos2::new(x, ruler_rect.bottom() - height), Pos2::new(x, ruler_rect.bottom())], Stroke::new(1.0, TICK));
        if tick.major {
            ruler.text(Pos2::new(x + 3.0, ruler_rect.top() + 2.0), Align2::LEFT_TOP, format_time(tick.time, step), FontId::proportional(11.0), Color32::LIGHT_GRAY);
        }
    }
}

fn draw_playhead(painter: &egui::Painter, state: &EditorState, ruler_rect: Rect, tracks_rect: Rect) {
    let x = ruler_rect.left() + state.view.time_to_x(state.playhead()) as f32;
    if x < ruler_rect.left() || x > ruler_rect.right() {
        return;
    }
    painter.line_segment([Pos2::new(x, ruler_rect.top()), Pos2::new(x, tracks_rect.bottom())], Stroke::new(1.5, PLAYHEAD));
}

fn headers(ui: &mut egui::Ui, state: &EditorState, header_rect: Rect, requests: &mut Vec<UiRequest>) {
    let track_count = state.project().tracks.len();
    let kinds: Vec<TrackKind> = state.project().tracks.iter().map(|track| track.kind).collect();
    ui.scope_builder(UiBuilder::new().max_rect(header_rect), |ui| {
        ui.set_clip_rect(header_rect);
        for (t, kind) in kinds.into_iter().enumerate() {
            let top = header_rect.top() + state.view.track_top(t, track_count) as f32;
            let row = Rect::from_min_size(Pos2::new(header_rect.left(), top), Vec2::new(header_rect.width(), TRACK_HEIGHT as f32));
            if !row.intersects(header_rect) {
                continue;
            }
            let name = if kind == TrackKind::Video { "Video" } else { "Audio" };
            ui.painter().text(row.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, format!("{name} {t}"), FontId::proportional(13.0), Color32::LIGHT_GRAY);
            let button = Rect::from_min_size(Pos2::new(row.right() - 62.0, row.center().y - 11.0), Vec2::new(56.0, 22.0));
            if ui.put(button, egui::Button::new("+ Clip")).clicked() {
                requests.push(UiRequest::PickClipFile { track: t });
            }
        }
    });
}
