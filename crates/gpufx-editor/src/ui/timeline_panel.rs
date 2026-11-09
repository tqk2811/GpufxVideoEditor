//! The timeline panel: toolbar, ruler, track headers and clips.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::editor::{clip_label, EditorState};
use crate::i18n::{Language, Message};
use crate::timeline::drag::{begin_drag, clamp_trim, drag_command};
use crate::timeline::hit::{hit_test, ClipZone, MIN_CLIP_WIDTH};
use crate::timeline::ruler::{format_time, major_step, ruler_ticks};
use crate::timeline::view::{content_end, ViewArea, Wheel, TRACK_HEIGHT};
use crate::visuals::levels::{bar_fraction, peak_bucket};
use crate::visuals::plan::{bar_span, bucket_step, bucket_time, split_body, tile_source_ticks, tile_ticks, visible_cells, BAR_GAP, BAR_WIDTH};
use crate::visuals::{ClipVisuals, SourceView};
use egui::{Align, Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Layout, Mesh, MouseWheelUnit, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, UiBuilder, Vec2};
use std::cell::RefCell;
use engine::{Clip, ClipKey, Time, TrackKind};

const TOOLBAR_HEIGHT: f32 = 36.0;
const RULER_HEIGHT: f32 = 26.0;
const HEADER_WIDTH: f32 = 64.0;
const LINE_SCROLL_POINTS: f32 = 50.0;
const CLIP_HEADER_HEIGHT: f32 = 14.0;
const CLIP_RADIUS: u8 = 4;
const ZOOM_SLIDER_WIDTH: f32 = 130.0;
const PLAYHEAD_MARKER: f32 = 6.0;
/// The picture shape used until a source says its own.
const DEFAULT_ASPECT: f32 = 16.0 / 9.0;
const BARS_COLOR: Color32 = Color32::from_rgb(96, 208, 158);
const BARS_MUTED: Color32 = Color32::from_rgb(120, 120, 130);

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    let toolbar_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), TOOLBAR_HEIGHT.min(rect.height())));
    let body = Rect::from_min_max(Pos2::new(rect.left(), toolbar_rect.bottom()), rect.max);
    ui.painter_at(rect).hline(rect.x_range(), toolbar_rect.bottom(), Stroke::new(1.0, theme::BORDER));
    ui.scope_builder(UiBuilder::new().max_rect(toolbar_rect.shrink2(Vec2::new(6.0, 0.0))), |ui| toolbar(ui, state, toolbar_rect));
    if body.height() > RULER_HEIGHT {
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| timeline(ui, state, requests, body));
    }
}

fn toolbar(ui: &mut egui::Ui, state: &mut EditorState, toolbar_rect: Rect) {
    let language = state.language();
    ui.allocate_ui_with_layout(ui.max_rect().size(), Layout::left_to_right(Align::Center), |ui| {
        ui.add_enabled_ui(state.can_undo(), |ui| {
            if icons::button(ui, Icon::Undo, &language.t(Message::UndoShortcut), false).clicked() {
                state.undo();
            }
        });
        ui.add_enabled_ui(state.can_redo(), |ui| {
            if icons::button(ui, Icon::Redo, &language.t(Message::RedoShortcut), false).clicked() {
                state.redo();
            }
        });
        ui.separator();
        let has_selection = state.selection().is_some();
        ui.add_enabled_ui(has_selection, |ui| {
            if icons::button(ui, Icon::Scissors, &language.t(Message::SplitShortcut), false).clicked() {
                state.split_selected();
            }
            if icons::button(ui, Icon::TrimLeft, &language.t(Message::TrimBefore), false).clicked() {
                state.trim_start_to_playhead();
            }
            if icons::button(ui, Icon::TrimRight, &language.t(Message::TrimAfter), false).clicked() {
                state.trim_end_to_playhead();
            }
            if icons::button(ui, Icon::Trash, &language.t(Message::DeleteShortcut), false).clicked() {
                state.delete_selected();
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| zoom_controls(ui, state, toolbar_rect));
    });
}

/// The zoom slider moves around the middle of the track area, as the wheel moves around the pointer.
fn zoom_controls(ui: &mut egui::Ui, state: &mut EditorState, toolbar_rect: Rect) {
    let width = f64::from((toolbar_rect.width() - HEADER_WIDTH).max(1.0));
    let area = ViewArea { content_end: content_end(state.project()), width, track_count: state.project().tracks.len(), rows_height: 0.0 };
    let center = width / 2.0;
    let language = state.language();
    if icons::button(ui, Icon::ZoomIn, &language.t(Message::ZoomIn), false).clicked() {
        state.view.zoom_at(1.25, center, &area);
    }
    let mut fraction = state.view.zoom_fraction();
    ui.spacing_mut().slider_width = ZOOM_SLIDER_WIDTH;
    if ui.add(egui::Slider::new(&mut fraction, 0.0..=1.0).show_value(false)).changed() {
        state.view.set_zoom_fraction(fraction, center, &area);
    }
    if icons::button(ui, Icon::ZoomOut, &language.t(Message::ZoomOut), false).clicked() {
        state.view.zoom_at(0.8, center, &area);
    }
}

fn timeline(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, rect: Rect) {
    ui.allocate_rect(rect, Sense::hover());
    let tracks_left = rect.left() + HEADER_WIDTH;
    let ruler_rect = Rect::from_min_max(Pos2::new(tracks_left, rect.top()), Pos2::new(rect.right(), rect.top() + RULER_HEIGHT));
    let tracks_rect = Rect::from_min_max(Pos2::new(tracks_left, ruler_rect.bottom()), rect.max);
    let header_rect = Rect::from_min_max(Pos2::new(rect.left(), ruler_rect.bottom()), Pos2::new(tracks_left, rect.bottom()));

    let area = view_area(state, tracks_rect);
    state.view.clamp_scroll(&area);
    handle_wheel(ui, state, rect, tracks_rect, &area);

    let ruler_response = ui.interact(ruler_rect, ui.scope_id().with("ruler"), Sense::click_and_drag());
    if ruler_response.is_pointer_button_down_on() {
        if let Some(pointer) = ruler_response.interact_pointer_pos() {
            state.scrub_to(state.view.x_to_time(f64::from(pointer.x - tracks_left)));
        }
    } else {
        state.end_scrub();
    }

    let tracks_response = ui.interact(tracks_rect, ui.scope_id().with("tracks"), Sense::click_and_drag());
    handle_tracks(ui, state, &tracks_response, tracks_rect);
    // A drop can shrink the content, so the limits are measured again.
    state.view.clamp_scroll(&view_area(state, tracks_rect));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);
    draw_rows(ui, state, tracks_rect);
    draw_ruler(&painter, state, ruler_rect);
    add_track_menu(ui, state, Rect::from_min_max(rect.min, Pos2::new(tracks_left, ruler_rect.bottom())));
    headers(ui, state, header_rect, requests);
    painter.vline(tracks_left, rect.y_range(), Stroke::new(1.0, theme::BORDER));
    draw_playhead(&painter, state, ruler_rect, tracks_rect);
}

fn add_track_menu(ui: &mut egui::Ui, state: &mut EditorState, rect: Rect) {
    let language = state.language();
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.menu_button(language.t(Message::AddTrack), |ui| {
            if ui.button(language.t(Message::AddVideoTrack)).clicked() {
                state.add_track(TrackKind::Video);
                ui.close();
            }
            if ui.button(language.t(Message::AddAudioTrack)).clicked() {
                state.add_track(TrackKind::Audio);
                ui.close();
            }
        });
    });
}

fn view_area(state: &EditorState, tracks_rect: Rect) -> ViewArea {
    ViewArea { content_end: content_end(state.project()), width: f64::from(tracks_rect.width()), track_count: state.project().tracks.len(), rows_height: f64::from(tracks_rect.height()) }
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
            let wheel = Wheel { ctrl: modifiers.command, shift: modifiers.shift, delta_x: f64::from(points.x), delta_y: f64::from(points.y), cursor_x };
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
    let height = f64::from(tracks_rect.height());
    let local = |pos: Pos2| (f64::from(pos.x - tracks_rect.left()), f64::from(pos.y - tracks_rect.top()));

    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let (x, y) = local(pointer);
            let hit = state.view.track_at_y(y, track_count, height).and_then(|track| hit_test(state.project(), &state.view, track, x));
            state.select(hit.map(|hit| hit.key));
        }
    }

    if response.drag_started_by(egui::PointerButton::Primary) {
        let press = ui.ctx().input(|input| input.pointer.press_origin());
        if let Some(press) = press {
            let (x, y) = local(press);
            let hit = state.view.track_at_y(y, track_count, height).and_then(|track| hit_test(state.project(), &state.view, track, x));
            if let Some(hit) = hit {
                state.select(Some(hit.key));
                state.drag = begin_drag(state.project(), hit, state.view.x_to_time(x));
            }
        }
    }

    if response.drag_stopped_by(egui::PointerButton::Primary) {
        if let (Some(drag), Some(pointer)) = (state.drag, response.interact_pointer_pos()) {
            let (x, y) = local(pointer);
            let command = clamp_trim(state.project(), drag_command(&drag, state.view.x_to_time(x), state.view.track_at_y(y, track_count, height)));
            state.finish_drag(command);
        }
        state.drag = None;
    }

    let cursor = if state.drag.is_some() {
        Some(CursorIcon::Grabbing)
    } else {
        response.hover_pos().and_then(|pointer| {
            let (x, y) = local(pointer);
            let track = state.view.track_at_y(y, track_count, height)?;
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
        painter.rect_filled(row, CornerRadius::ZERO, if t % 2 == 0 { theme::ROW_EVEN } else { theme::ROW_ODD });
        for (c, clip) in track.clips.iter().enumerate() {
            let key = ClipKey { track: t, clip: c };
            let rect = span_rect(state, tracks_rect, row, clip.start, clip.start.saturating_add(clip.duration));
            if rect.intersects(tracks_rect) {
                let dragged = state.drag.is_some_and(|drag| drag.key == key);
                draw_clip(&painter, &ClipPaint { clip, kind: track.kind, rect, clip_area: tracks_rect, selected: state.selection() == Some(key), dragged, visuals: Some(&state.visuals), pixels_per_second: state.view.pixels_per_second() });
            }
        }
        draw_transitions(&painter, state, tracks_rect, row, track);
    }
    draw_drag_preview(ui, state, tracks_rect);
}

/// One clip to draw. Lock or hide marks belong in [`draw_clip`].
pub struct ClipPaint<'a> {
    pub clip: &'a Clip,
    pub kind: TrackKind,
    pub rect: Rect,
    /// The part of the screen clips may draw in.
    pub clip_area: Rect,
    pub selected: bool,
    pub dragged: bool,
    /// The thumbnails and levels to draw, when there are any to draw from.
    pub visuals: Option<&'a RefCell<ClipVisuals>>,
    pub pixels_per_second: f64,
}

/// Paints a clip's body, name strip, pictures, level bars and selection.
pub fn draw_clip(painter: &egui::Painter, paint: &ClipPaint<'_>) {
    let (body, header) = match paint.kind {
        TrackKind::Video => (theme::VIDEO_CLIP, theme::VIDEO_CLIP_HEADER),
        TrackKind::Audio => (theme::AUDIO_CLIP, theme::AUDIO_CLIP_HEADER),
    };
    let dim = |color: Color32| if paint.dragged { color.gamma_multiply(0.5) } else { color };
    let radius = CornerRadius::same(CLIP_RADIUS);
    painter.rect_filled(paint.rect, radius, dim(body));
    let strip = paint.rect.with_max_y(paint.rect.top() + CLIP_HEADER_HEIGHT);
    painter.rect_filled(strip, CornerRadius { nw: CLIP_RADIUS, ne: CLIP_RADIUS, sw: 0, se: 0 }, dim(header));
    let text = painter.with_clip_rect(paint.rect.intersect(paint.clip_area));
    text.text(strip.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, clip_label(&paint.clip.source), FontId::proportional(11.0), theme::TEXT);
    if let Some(visuals) = paint.visuals {
        draw_clip_media(painter, paint, &mut visuals.borrow_mut(), paint.rect.with_min_y(strip.bottom()));
    }
    if paint.selected {
        painter.rect_stroke(paint.rect, radius, Stroke::new(2.0, theme::SELECTED), StrokeKind::Inside);
    }
}

/// The pictures and level bars under the name strip. Only what is on screen is asked for.
fn draw_clip_media(painter: &egui::Painter, paint: &ClipPaint<'_>, visuals: &mut ClipVisuals, body: Rect) {
    let clipped = painter.with_clip_rect(paint.rect.intersect(paint.clip_area));
    let slot = visuals.slot(&paint.clip.source);
    let view = visuals.view(slot);
    let (has_picture, has_sound, aspect, length) = match view {
        SourceView::Known(facts) => (facts.has_video, facts.has_audio, facts.aspect, facts.length),
        SourceView::Failed => (false, false, DEFAULT_ASPECT, None),
        SourceView::Pending => (paint.kind == TrackKind::Video, false, DEFAULT_ASPECT, None),
    };
    // An audio track has only sound, whatever else the file holds.
    let film = has_picture && paint.kind == TrackKind::Video;
    // A source nothing can be drawn from keeps the placeholder over the whole body.
    let placeholder = view == SourceView::Failed && paint.kind == TrackKind::Video;
    let (film_height, bars_height) = split_body(body.height(), film || placeholder, has_sound);
    if film_height > 0.0 {
        let strip = body.with_max_y(body.top() + film_height);
        draw_film_strip(&clipped, paint, visuals, slot, strip, aspect, length, film);
    }
    if bars_height > 0.0 {
        draw_level_bars(&clipped, paint, visuals, slot, body.with_min_y(body.bottom() - bars_height));
    } else if film_height == 0.0 {
        clipped.rect_filled(body, CornerRadius::ZERO, Color32::from_black_alpha(70));
    }
}

/// Contiguous pictures along the clip, anchored to its left edge so scrolling does not shift them.
#[allow(clippy::too_many_arguments)]
fn draw_film_strip(painter: &egui::Painter, paint: &ClipPaint<'_>, visuals: &mut ClipVisuals, slot: usize, strip: Rect, aspect: f32, length: Option<Time>, show_pictures: bool) {
    let tile_width = (strip.height() * aspect).max(8.0);
    let visible = paint.clip_area.x_range();
    let tint = if paint.dragged { Color32::from_white_alpha(128) } else { Color32::WHITE };
    let cells = visible_cells(f64::from(strip.left()), f64::from(tile_width), f64::from(strip.width()), f64::from(visible.min)..f64::from(visible.max));
    let step = bucket_step(tile_ticks(f64::from(tile_width), paint.pixels_per_second));
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    for index in cells {
        let left = strip.left() + index as f32 * tile_width;
        let tile = Rect::from_min_size(Pos2::new(left, strip.top()), Vec2::new(tile_width, strip.height()));
        let source = tile_source_ticks(paint.clip, f64::from(tile_width), paint.pixels_per_second, index);
        let texture = if show_pictures { visuals.tile(slot, bucket_time(source, step, length), step) } else { None };
        match texture {
            Some(texture) => painter.image(texture, tile, uv, tint),
            None => painter.rect_filled(tile, CornerRadius::ZERO, Color32::from_black_alpha(if index % 2 == 0 { 70 } else { 55 })),
        };
    }
}

/// One bar per few pixels, its height the loudest peak of that part of the source in dBFS.
fn draw_level_bars(painter: &egui::Painter, paint: &ClipPaint<'_>, visuals: &mut ClipVisuals, slot: usize, strip: Rect) {
    let pitch = BAR_WIDTH + BAR_GAP;
    let visible = paint.clip_area.x_range();
    let cells = visible_cells(f64::from(strip.left()), pitch, f64::from(strip.width()), f64::from(visible.min)..f64::from(visible.max));
    let Some(peaks) = visuals.peaks(slot) else {
        painter.rect_filled(strip, CornerRadius::ZERO, Color32::from_black_alpha(90));
        return;
    };
    let color = if paint.clip.muted { BARS_MUTED } else { BARS_COLOR };
    let color = if paint.dragged { color.gamma_multiply(0.5) } else { color };
    let mut mesh = Mesh::default();
    for index in cells {
        let span = bar_span(paint.clip, paint.pixels_per_second, index);
        let peak = peaks.max_between(peak_bucket(span.start), peak_bucket(span.end - 1) + 1);
        let height = bar_fraction(peak, paint.clip.gain_db) * strip.height();
        if height <= 0.0 {
            continue;
        }
        let left = strip.left() + index as f32 * pitch as f32;
        let bar = Rect::from_min_max(Pos2::new(left, strip.bottom() - height), Pos2::new(left + BAR_WIDTH as f32, strip.bottom()));
        mesh.add_colored_rect(bar, color);
    }
    painter.add(Shape::mesh(mesh));
}

fn draw_transitions(painter: &egui::Painter, state: &EditorState, tracks_rect: Rect, row: Rect, track: &engine::Track) {
    for transition in &track.transitions {
        let Some(next) = track.clips.get(transition.after_clip + 1) else { continue };
        let start = next.start.saturating_sub(transition.duration.half_floor());
        let rect = span_rect(state, tracks_rect, row, start, start.saturating_add(transition.duration));
        if rect.intersects(tracks_rect) {
            painter.rect_filled(rect, CornerRadius::same(CLIP_RADIUS), theme::TRANSITION);
            painter.rect_stroke(rect, CornerRadius::same(CLIP_RADIUS), Stroke::new(1.0, theme::ACCENT_TEXT), StrokeKind::Inside);
        }
    }
}

fn span_rect(state: &EditorState, tracks_rect: Rect, row: Rect, start: Time, end: Time) -> Rect {
    let left = tracks_rect.left() + state.view.time_to_x(start) as f32;
    let right = (tracks_rect.left() + state.view.time_to_x(end) as f32).max(left + MIN_CLIP_WIDTH as f32);
    Rect::from_min_max(Pos2::new(left, row.top() + 3.0), Pos2::new(right, row.bottom() - 3.0))
}

fn draw_drag_preview(ui: &egui::Ui, state: &EditorState, tracks_rect: Rect) {
    let Some(drag) = state.drag else { return };
    let Some(pointer) = ui.ctx().pointer_latest_pos() else { return };
    let track_count = state.project().tracks.len();
    let height = f64::from(tracks_rect.height());
    let x = f64::from(pointer.x - tracks_rect.left());
    let y = f64::from(pointer.y - tracks_rect.top());
    let command = clamp_trim(state.project(), drag_command(&drag, state.view.x_to_time(x), state.view.track_at_y(y, track_count, height)));
    let Some(span) = command.landing(state.project()) else { return };
    let row = row_rect(state, tracks_rect, span.track, track_count);
    let rect = span_rect(state, tracks_rect, row, span.start, span.end);
    ui.painter_at(tracks_rect).rect_stroke(rect, CornerRadius::same(CLIP_RADIUS), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
}

fn draw_ruler(painter: &egui::Painter, state: &EditorState, ruler_rect: Rect) {
    let ruler = painter.with_clip_rect(ruler_rect);
    ruler.rect_filled(ruler_rect, CornerRadius::ZERO, theme::RULER);
    let step = major_step(&state.view);
    for tick in ruler_ticks(&state.view, f64::from(ruler_rect.width())) {
        let x = ruler_rect.left() + tick.x as f32;
        let height = if tick.major { 10.0 } else { 5.0 };
        ruler.line_segment([Pos2::new(x, ruler_rect.bottom() - height), Pos2::new(x, ruler_rect.bottom())], Stroke::new(1.0, theme::TICK));
        if tick.major {
            ruler.text(Pos2::new(x + 3.0, ruler_rect.top() + 3.0), Align2::LEFT_TOP, format_time(tick.time, step), FontId::monospace(11.0), theme::TEXT_DIM);
        }
    }
    ruler.hline(ruler_rect.x_range(), ruler_rect.bottom(), Stroke::new(1.0, theme::BORDER));
}

fn draw_playhead(painter: &egui::Painter, state: &EditorState, ruler_rect: Rect, tracks_rect: Rect) {
    let x = ruler_rect.left() + state.view.time_to_x(state.playhead()) as f32;
    if x < ruler_rect.left() || x > ruler_rect.right() {
        return;
    }
    let top = ruler_rect.top() + 1.0;
    painter.line_segment([Pos2::new(x, top), Pos2::new(x, tracks_rect.bottom())], Stroke::new(1.5, theme::PLAYHEAD));
    let marker = vec![Pos2::new(x - PLAYHEAD_MARKER, top), Pos2::new(x + PLAYHEAD_MARKER, top), Pos2::new(x + PLAYHEAD_MARKER, top + 7.0), Pos2::new(x, top + 13.0), Pos2::new(x - PLAYHEAD_MARKER, top + 7.0)];
    painter.add(Shape::convex_polygon(marker, theme::PLAYHEAD, Stroke::NONE));
}

/// The short label and tooltip of track `t`, numbered within its kind from the first of the kind.
fn track_label(language: Language, kinds: &[TrackKind], t: usize) -> (String, String) {
    let number = kinds[..=t].iter().filter(|kind| **kind == kinds[t]).count();
    match kinds[t] {
        TrackKind::Video => (format!("V{number}"), language.t(Message::VideoTrackNumber(number))),
        TrackKind::Audio => (format!("A{number}"), language.t(Message::AudioTrackNumber(number))),
    }
}

fn headers(ui: &mut egui::Ui, state: &EditorState, header_rect: Rect, requests: &mut Vec<UiRequest>) {
    let track_count = state.project().tracks.len();
    let kinds: Vec<TrackKind> = state.project().tracks.iter().map(|track| track.kind).collect();
    ui.scope_builder(UiBuilder::new().max_rect(header_rect), |ui| {
        ui.set_clip_rect(header_rect);
        for (t, &kind) in kinds.iter().enumerate() {
            let top = header_rect.top() + state.view.track_top(t, track_count) as f32;
            let row = Rect::from_min_size(Pos2::new(header_rect.left(), top), Vec2::new(header_rect.width(), TRACK_HEIGHT as f32));
            if !row.intersects(header_rect) {
                continue;
            }
            let icon = if kind == TrackKind::Video { Icon::Film } else { Icon::Music };
            let (label, tip) = track_label(state.language(), &kinds, t);
            let icon_rect = Rect::from_center_size(Pos2::new(row.left() + 18.0, row.center().y), Vec2::splat(14.0));
            icons::paint(ui.painter(), icon_rect, icon, theme::TEXT_DIM);
            let label_pos = Pos2::new(icon_rect.right() + 6.0, row.center().y);
            let label_rect = ui.painter().text(label_pos, Align2::LEFT_CENTER, label, FontId::proportional(12.0), theme::TEXT_DIM);
            let id = ui.scope_id().with(("track-icon", t));
            ui.interact(icon_rect.union(label_rect), id, Sense::hover()).on_hover_text(tip);
            let button = Rect::from_center_size(Pos2::new(row.right() - 18.0, row.center().y), Vec2::splat(22.0));
            ui.scope_builder(UiBuilder::new().max_rect(button).id_salt(("track-add", t)), |ui| {
                if icons::button(ui, Icon::Plus, &state.language().t(Message::AddClipFromFile), false).clicked() {
                    requests.push(UiRequest::PickClipFile { track: t });
                }
            });
        }
    });
}

/// Where the track rows start in a timeline panel at `panel`, for tests that point at a clip.
#[cfg(test)]
pub(super) fn tracks_origin(panel: Rect) -> Pos2 {
    Pos2::new(panel.left() + 1.0 + HEADER_WIDTH, panel.top() + 1.0 + TOOLBAR_HEIGHT + RULER_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_are_numbered_within_their_kind() {
        let kinds = [TrackKind::Video, TrackKind::Audio, TrackKind::Video];
        assert_eq!(track_label(Language::Vietnamese, &kinds, 0).0, "V1");
        assert_eq!(track_label(Language::Vietnamese, &kinds, 1).0, "A1");
        assert_eq!(track_label(Language::Vietnamese, &kinds, 2), ("V2".to_owned(), "Track video 2".to_owned()));
    }
}
