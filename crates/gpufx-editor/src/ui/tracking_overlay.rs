//! Motion tracking on the player, for the selected video clip: the box drawn to pick an object,
//! the path of the chosen track and its box at the playhead, which a drag moves or resizes into a
//! sample placed by hand.

use crate::editor::EditorState;
use crate::track_edit::{box_corners, LOW_CONFIDENCE};
use egui::{Color32, CursorIcon, Id, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use engine::{ClipKey, FrameMapping, Time, TrackSample};

const PATH_WIDTH: f32 = 1.5;
const BOX_WIDTH: f32 = 2.0;
const GRIP_SIZE: f32 = 8.0;
const GRIP_HIT_SIZE: f32 = 14.0;
/// The most points of the path drawn; a longer track is thinned evenly.
const MAX_PATH_POINTS: usize = 2000;

const PICK_COLOR: Color32 = Color32::from_rgb(255, 210, 0);
const TRACKED_COLOR: Color32 = Color32::from_rgb(0, 200, 255);
const MANUAL_COLOR: Color32 = Color32::from_rgb(255, 210, 0);
const LOST_COLOR: Color32 = Color32::from_rgb(255, 60, 60);

/// The colour of a sample: placed by hand, lost (low confidence) or tracked.
pub(super) fn sample_color(sample: &TrackSample) -> Color32 {
    if sample.manual {
        MANUAL_COLOR
    } else if sample.confidence < LOW_CONFIDENCE {
        LOST_COLOR
    } else {
        TRACKED_COLOR
    }
}

fn to_screen(point: [f64; 2], picture: Rect) -> Pos2 {
    picture.min + Vec2::new(point[0] as f32 * picture.width(), point[1] as f32 * picture.height())
}

fn to_frame(pos: Pos2, picture: Rect) -> [f64; 2] {
    [f64::from((pos.x - picture.left()) / picture.width()), f64::from((pos.y - picture.top()) / picture.height())]
}

/// The video clip whose tracking shows: selected, on a video track that is shown and unlocked,
/// following nothing.
fn target(state: &EditorState) -> Option<ClipKey> {
    let key = state.selection()?;
    let track = state.project().tracks.get(key.track)?;
    let clip = track.clips.get(key.clip)?;
    (track.kind == engine::TrackKind::Video && !track.hidden && !clip.is_still() && clip.text.is_none() && clip.censor.is_none() && clip.follow.is_none()).then_some(key)
}

/// Whether the player is drawing a box for a new track, so the placement frame steps aside.
pub(super) fn picking(state: &EditorState) -> bool {
    target(state).is_some_and(|key| state.ui.tracking_pick == Some(key))
}

/// Draws the tracking of the selected clip over `picture`, inside `area`, and handles its drags.
pub(super) fn show(ui: &mut egui::Ui, state: &mut EditorState, area: Rect, picture: Rect) {
    if picture.width() < 1.0 || picture.height() < 1.0 || state.is_exporting() {
        return;
    }
    let Some(key) = target(state) else { return };
    if state.ui.tracking_pick == Some(key) {
        pick(ui, state, key, area, picture);
        return;
    }
    let Some(track) = state.motion_track_for(key).cloned() else { return };
    let Some((mapping, now)) = state.clip_frame_now(key) else { return };
    let painter = ui.painter_at(area);
    draw_path(&painter, state, key, &track, &mapping, picture);
    let Some(point) = track.point_at(now, 0.0) else { return };
    let tolerance = state.sample_tolerance(key);
    let on_frame = crate::track_edit::sample_near(&track, now, tolerance);
    let shown = TrackSample { source_time: now, cx: point.cx, cy: point.cy, w: point.w, h: point.h, angle: point.angle, confidence: on_frame.map_or(1.0, |s| s.confidence), manual: on_frame.is_some_and(|s| s.manual) };
    let corners = box_corners(&shown).map(|corner| to_screen(mapping.to_frame(corner), picture));
    let color = if on_frame.is_some() { sample_color(&shown) } else { TRACKED_COLOR.gamma_multiply(0.7) };
    painter.add(Shape::closed_line(corners.to_vec(), Stroke::new(BOX_WIDTH, color)));
    if state.is_playing() || state.is_track_locked(key.track) {
        return;
    }
    let scope = ui.scope_id().with(("tracking-box", key, track.id));
    let body = ui.interact(Rect::from_points(&corners).intersect(area), scope.with("body"), Sense::drag());
    edit_box(ui, state, track.id, &mapping, picture, &body, shown, None);
    body.on_hover_and_drag_cursor(CursorIcon::Move);
    if shown.angle != 0.0 {
        // Corners resize along the axes of the picture, which a turned box does not have.
        return;
    }
    for (index, corner) in corners.into_iter().enumerate() {
        let response = ui.interact(Rect::from_center_size(corner, Vec2::splat(GRIP_HIT_SIZE)).intersect(area), scope.with(("grip", index)), Sense::drag());
        edit_box(ui, state, track.id, &mapping, picture, &response, shown, Some(index));
        response.on_hover_and_drag_cursor(if index % 2 == 0 { CursorIcon::ResizeNwSe } else { CursorIcon::ResizeNeSw });
        painter.rect_filled(Rect::from_center_size(corner, Vec2::splat(GRIP_SIZE)), 0.0, color);
    }
}

/// The path of `track` over the part of the source clip `key` plays, drawn with the clip's place
/// at the playhead, each step coloured by its sample; hand placed samples get a dot.
fn draw_path(painter: &egui::Painter, state: &EditorState, key: ClipKey, track: &engine::MotionTrack, mapping: &FrameMapping, picture: Rect) {
    let Some(clip) = state.project().tracks.get(key.track).and_then(|lane| lane.clips.get(key.clip)) else { return };
    let (a, b) = (clip.source_time_at(Time::ZERO), clip.source_time_at(clip.duration));
    let (low, high) = (a.min(b), a.max(b));
    let first = track.samples.partition_point(|s| s.source_time < low);
    let last = track.samples.partition_point(|s| s.source_time <= high);
    let samples = &track.samples[first..last];
    let step = samples.len().div_ceil(MAX_PATH_POINTS).max(1);
    let points: Vec<(Pos2, &TrackSample)> = samples.iter().step_by(step).map(|s| (to_screen(mapping.to_frame([s.cx, s.cy]), picture), s)).collect();
    for pair in points.windows(2) {
        painter.line_segment([pair[0].0, pair[1].0], Stroke::new(PATH_WIDTH, sample_color(pair[1].1).gamma_multiply(0.8)));
    }
    for (at, sample) in samples.iter().filter(|s| s.manual).map(|s| (to_screen(mapping.to_frame([s.cx, s.cy]), picture), s)) {
        painter.circle_filled(at, 3.0, sample_color(sample));
    }
}

/// Where [`edit_box`] keeps the box at the press of handle `id`.
fn start_id(id: Id) -> Id {
    id.with("start")
}

/// Runs one handle of the box: the body moves it, corner `grip` resizes it against the opposite
/// corner, in the picture of the clip. Each step shows the box as a sample placed by hand at the
/// frame of the playhead; the release makes it one undo step.
#[allow(clippy::too_many_arguments)]
fn edit_box(ui: &egui::Ui, state: &mut EditorState, track_id: u32, mapping: &FrameMapping, picture: Rect, response: &egui::Response, shown: TrackSample, grip: Option<usize>) {
    let id = start_id(response.id);
    if response.drag_started() {
        ui.data_mut(|data| data.insert_temp(id, shown));
    }
    if response.dragged() {
        let start = ui.data(|data| data.get_temp::<TrackSample>(id));
        let pointer = ui.ctx().input(|input| input.pointer.press_origin().zip(input.pointer.latest_pos()));
        if let Some((start, (from, to))) = start.zip(pointer) {
            let (a, b) = (mapping.to_source(to_frame(from, picture)), mapping.to_source(to_frame(to, picture)));
            let next = match grip {
                None => TrackSample { cx: start.cx + b[0] - a[0], cy: start.cy + b[1] - a[1], ..start },
                Some(index) => {
                    let corners = box_corners(&start);
                    let fixed = corners[(index + 2) % 4];
                    let w = (b[0] - fixed[0]).abs().max(crate::track_edit::MIN_BOX_SIDE);
                    let h = (b[1] - fixed[1]).abs().max(crate::track_edit::MIN_BOX_SIDE);
                    TrackSample { cx: fixed[0] + (b[0] - fixed[0]).signum() * w / 2.0, cy: fixed[1] + (b[1] - fixed[1]).signum() * h / 2.0, w, h, ..start }
                }
            };
            state.live_track_sample(track_id, next);
        }
    }
    if response.drag_stopped() {
        state.commit_live();
        ui.data_mut(|data| data.remove::<TrackSample>(id));
    }
}

/// Drawing a box around the object to start a track: a drag over the picture, released.
fn pick(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, area: Rect, picture: Rect) {
    let response = ui.interact(picture.intersect(area), ui.scope_id().with(("tracking-pick", key)), Sense::drag());
    let response = response.on_hover_cursor(CursorIcon::Crosshair);
    let painter = ui.painter_at(area);
    painter.rect_stroke(picture, 0.0, Stroke::new(1.0, PICK_COLOR.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    let drawn = ui.ctx().input(|input| input.pointer.press_origin().zip(input.pointer.latest_pos())).map(|(from, to)| Rect::from_two_pos(from, to).intersect(picture));
    if let Some(rect) = drawn.filter(|_| response.dragged() || response.drag_stopped()) {
        painter.rect_stroke(rect, 0.0, Stroke::new(BOX_WIDTH, PICK_COLOR), egui::StrokeKind::Middle);
        if response.drag_stopped() {
            let [x, y] = to_frame(rect.min, picture);
            let [right, bottom] = to_frame(rect.max, picture);
            state.create_motion_track(key, [x, y, right - x, bottom - y]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_goes_to_the_screen_and_back() {
        let picture = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 100.0));
        let at = to_screen([0.25, 0.5], picture);
        assert_eq!(at, Pos2::new(60.0, 70.0));
        let back = to_frame(at, picture);
        assert!((back[0] - 0.25).abs() < 1e-6 && (back[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn lost_samples_are_red_and_hand_placed_ones_yellow() {
        let sample = TrackSample { source_time: Time::ZERO, cx: 0.5, cy: 0.5, w: 0.1, h: 0.1, angle: 0.0, confidence: 0.2, manual: false };
        assert_eq!(sample_color(&sample), LOST_COLOR);
        assert_eq!(sample_color(&TrackSample { manual: true, ..sample }), MANUAL_COLOR);
        assert_eq!(sample_color(&TrackSample { confidence: 0.9, ..sample }), TRACKED_COLOR);
    }
}
