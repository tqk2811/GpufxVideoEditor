//! Censor regions on the player: drawing a new one (a dragged rectangle or ellipse, a polygon
//! clicked point by point, a path drawn by hand), and the points of the selected polygon region,
//! dragged, added on a double-click of a side and removed with a right-click.

use crate::censor_draw::{freehand_polygon, CensorDrawMode, FREEHAND_TOLERANCE};
use crate::editor::EditorState;
use crate::i18n::Message;
use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Modifiers, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};
use engine::polygon::{nearest_edge, MAX_POLYGON_POINTS, MIN_POLYGON_POINTS};
use engine::tracker::homography::Homography;
use engine::{ClipKey, RegionShape};

const DRAW_COLOR: Color32 = Color32::from_rgb(255, 210, 0);
const LINE_WIDTH: f32 = 1.5;
const POINT_SIZE: f32 = 7.0;
const POINT_HIT_SIZE: f32 = 14.0;
/// How near the first point, in pixels, a click closes the polygon.
const CLOSE_RADIUS: f32 = 8.0;
/// How near a side, in pixels, a double-click adds a point on it.
const EDGE_HIT: f64 = 6.0;
/// Points of a hand-drawn path closer than this many pixels to the last are skipped.
const FREEHAND_STEP: f32 = 1.0;

fn to_screen(point: [f64; 2], picture: Rect) -> Pos2 {
    picture.min + Vec2::new(point[0] as f32 * picture.width(), point[1] as f32 * picture.height())
}

fn to_frame(pos: Pos2, picture: Rect) -> [f64; 2] {
    unclamped_frame(picture.clamp(pos), picture)
}

fn unclamped_frame(pos: Pos2, picture: Rect) -> [f64; 2] {
    [f64::from((pos.x - picture.left()) / picture.width()), f64::from((pos.y - picture.top()) / picture.height())]
}

/// Whether a censor region is being drawn, so the other frames on the player step aside.
pub(super) fn drawing(state: &EditorState) -> bool {
    state.censor_draw().is_some()
}

/// Draws the censor region being drawn, or the points of the selected polygon region, over
/// `picture` inside `area`, and handles their pointer.
pub(super) fn show(ui: &mut egui::Ui, state: &mut EditorState, area: Rect, picture: Rect) {
    if picture.width() < 1.0 || picture.height() < 1.0 || state.is_exporting() {
        return;
    }
    if let Some(mode) = state.censor_draw().map(|draw| draw.mode) {
        draw(ui, state, area, picture, mode);
        return;
    }
    edit_points(ui, state, area, picture);
}

/// What a click does while points are clicked: closes the polygon on its first point, or adds one.
#[derive(Debug, PartialEq)]
enum PointsClick {
    Close,
    Add,
}

fn points_click(points: &[Pos2], click: Pos2) -> PointsClick {
    if points.len() >= MIN_POLYGON_POINTS && points[0].distance(click) <= CLOSE_RADIUS {
        PointsClick::Close
    } else {
        PointsClick::Add
    }
}

/// `points` without a point that lands within a few pixels of the one before it, as the first
/// click of the double-click that closes a polygon leaves.
fn without_repeats(points: &[[f64; 2]], picture: Rect) -> Vec<[f64; 2]> {
    let mut kept: Vec<[f64; 2]> = Vec::with_capacity(points.len());
    for point in points {
        if kept.last().is_none_or(|last| to_screen(*last, picture).distance(to_screen(*point, picture)) > 3.0) {
            kept.push(*point);
        }
    }
    kept
}

fn draw(ui: &mut egui::Ui, state: &mut EditorState, area: Rect, picture: Rect, mode: CensorDrawMode) {
    let id = ui.scope_id().with(("censor-draw", state.ui.censor_draw_round));
    let response = ui.interact(picture.intersect(area), id, Sense::click_and_drag());
    let response = response.on_hover_cursor(CursorIcon::Crosshair);
    let painter = ui.painter_at(area);
    let stroke = Stroke::new(LINE_WIDTH, DRAW_COLOR);
    let hint = state.language().t(Message::CensorDrawHint);
    painter.text(picture.left_top() + Vec2::new(8.0, 8.0), Align2::LEFT_TOP, hint, FontId::proportional(13.0), DRAW_COLOR);
    match mode {
        CensorDrawMode::Rect | CensorDrawMode::Ellipse => {
            let (shown, finished) = super::tracking_overlay::drawn_box(ui, &response, picture);
            if let Some(rect) = shown {
                if mode == CensorDrawMode::Rect {
                    painter.rect_stroke(rect, CornerRadius::ZERO, stroke, StrokeKind::Middle);
                } else {
                    painter.add(Shape::ellipse_stroke(rect.center(), rect.size() / 2.0, stroke));
                }
            }
            if let Some(rect) = finished {
                let (from, to) = (to_frame(rect.min, picture), to_frame(rect.max, picture));
                let shape = if mode == CensorDrawMode::Rect { RegionShape::Rect } else { RegionShape::Ellipse };
                state.finish_censor_box(shape, (from[0], from[1], to[0], to[1]));
            }
        }
        CensorDrawMode::Freehand => {
            if response.drag_started() {
                let origin = ui.ctx().input(|input| input.pointer.press_origin()).or(response.interact_pointer_pos());
                if let Some(points) = state.censor_draw_points() {
                    points.clear();
                    points.extend(origin.map(|origin| to_frame(origin, picture)));
                }
            }
            if response.dragged() {
                if let Some(latest) = ui.ctx().input(|input| input.pointer.latest_pos()) {
                    let point = to_frame(latest, picture);
                    if let Some(points) = state.censor_draw_points() {
                        if points.last().is_none_or(|last| to_screen(*last, picture).distance(to_screen(point, picture)) >= FREEHAND_STEP) {
                            points.push(point);
                        }
                    }
                }
            }
            let path: Vec<Pos2> = state.censor_draw().map_or(Vec::new(), |draw| draw.points.iter().map(|p| to_screen(*p, picture)).collect());
            if path.len() >= 2 {
                painter.add(Shape::line(path, stroke));
            }
            if response.drag_stopped() {
                let drawn = state.censor_draw().map(|draw| draw.points.clone()).unwrap_or_default();
                match freehand_polygon(&drawn, (f64::from(picture.width()), f64::from(picture.height())), FREEHAND_TOLERANCE) {
                    Some(polygon) => {
                        state.finish_censor_polygon(&polygon);
                    }
                    None => {
                        if let Some(points) = state.censor_draw_points() {
                            points.clear();
                        }
                    }
                }
            }
        }
        CensorDrawMode::Points => {
            // Enter belongs to a focused widget (a text field, a button) when there is one.
            let free = ui.ctx().memory(|memory| memory.focused().is_none());
            let enter = free && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
            let points = state.censor_draw().map(|draw| draw.points.clone()).unwrap_or_default();
            let screen: Vec<Pos2> = points.iter().map(|p| to_screen(*p, picture)).collect();
            if response.double_clicked() || enter {
                let closed = without_repeats(&points, picture);
                if closed.len() >= MIN_POLYGON_POINTS {
                    state.finish_censor_polygon(&closed);
                    return;
                }
            } else if response.clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    match points_click(&screen, pos) {
                        PointsClick::Close => {
                            state.finish_censor_polygon(&points);
                            return;
                        }
                        PointsClick::Add if points.len() < MAX_POLYGON_POINTS => {
                            if let Some(points) = state.censor_draw_points() {
                                points.push(to_frame(pos, picture));
                            }
                        }
                        PointsClick::Add => {}
                    }
                }
            }
            let mut screen: Vec<Pos2> = state.censor_draw().map_or(Vec::new(), |draw| draw.points.iter().map(|p| to_screen(*p, picture)).collect());
            for (index, point) in screen.iter().enumerate() {
                let size = if index == 0 { POINT_SIZE + 3.0 } else { POINT_SIZE };
                painter.rect_filled(Rect::from_center_size(*point, Vec2::splat(size)), CornerRadius::ZERO, DRAW_COLOR);
            }
            if let Some(hover) = response.hover_pos().filter(|_| !screen.is_empty()) {
                screen.push(hover);
            }
            if screen.len() >= 2 {
                painter.add(Shape::line(screen, stroke));
            }
        }
    }
}

/// The selected censor clip when it is a polygon shown at the playhead and may be edited, with
/// where its box stands now: the box in parts of the frame, its clockwise turn, and the corners it
/// is pinned to when it follows a track in perspective.
fn polygon_target(state: &EditorState) -> Option<(ClipKey, engine::CensorContent, engine::PlacedRect, f64, Option<[[f64; 2]; 4]>)> {
    if state.is_playing() {
        return None;
    }
    let key = state.selection()?;
    let track = state.project().tracks.get(key.track)?;
    let clip = track.clips.get(key.clip)?;
    let censor = clip.censor.as_ref().filter(|censor| censor.shape == RegionShape::Polygon)?;
    let at = state.playhead();
    if track.locked || track.hidden || at < clip.start || at >= clip.start.saturating_add(clip.duration) {
        return None;
    }
    let mut transform = clip.transform_at(at.saturating_sub(clip.start));
    if let Some(follow) = &clip.follow {
        engine::follow_transform(state.project(), follow, &mut transform, at);
    }
    let output = &state.project().output;
    let output_aspect = f64::from(output.width) / f64::from(output.height.max(1));
    let rect = transform.rect(output_aspect, censor.aspect);
    // As [`engine::CensorRegion::of`] gives the compositor: the shape is drawn into these corners.
    let quad = transform.pinned_corners(transform.unfollowed_rect(output_aspect, censor.aspect), output_aspect);
    Some((key, censor.clone(), rect, transform.rotation, quad))
}

/// Where the box of a region (its frame on screen, turned `rotation` clockwise degrees, or warped
/// onto the corners it is pinned to) puts point `local` of it, and back.
struct BoxOnScreen {
    frame: Rect,
    sine: f32,
    cosine: f32,
    picture: Rect,
    /// From the box (0 to 1 across it) to the frame and back, when it lies in perspective.
    warp: Option<(Homography, Homography)>,
}

impl BoxOnScreen {
    fn new(rect: engine::PlacedRect, rotation: f64, quad: Option<[[f64; 2]; 4]>, picture: Rect) -> Self {
        let min = picture.min + Vec2::new(rect.x as f32 * picture.width(), rect.y as f32 * picture.height());
        let frame = Rect::from_min_size(min, Vec2::new(rect.width as f32 * picture.width(), rect.height as f32 * picture.height()));
        let (sine, cosine) = (rotation as f32).to_radians().sin_cos();
        let unit = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let warp = quad.and_then(|quad| {
            let forward = Homography::from_quads(&unit, &quad)?;
            Some((forward, forward.inverse()?))
        });
        BoxOnScreen { frame, sine, cosine, picture, warp }
    }

    fn to_screen(&self, local: [f64; 2]) -> Pos2 {
        if let Some((forward, _)) = &self.warp {
            return forward.apply(local).map_or(self.frame.center(), |frame| to_screen(frame, self.picture));
        }
        let d = Vec2::new((local[0] as f32 - 0.5) * self.frame.width(), (local[1] as f32 - 0.5) * self.frame.height());
        self.frame.center() + Vec2::new(self.cosine * d.x - self.sine * d.y, self.sine * d.x + self.cosine * d.y)
    }

    /// The point of the box under `pos`, kept inside the box.
    fn to_local(&self, pos: Pos2) -> [f64; 2] {
        if let Some((_, backward)) = &self.warp {
            let local = backward.apply(unclamped_frame(pos, self.picture)).unwrap_or([0.5, 0.5]);
            return local.map(|value| if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.5 });
        }
        let d = pos - self.frame.center();
        let turned = Vec2::new(self.cosine * d.x + self.sine * d.y, self.cosine * d.y - self.sine * d.x);
        let part = |value: f32, length: f32| f64::from(value / length.max(1e-6) + 0.5).clamp(0.0, 1.0);
        [part(turned.x, self.frame.width()), part(turned.y, self.frame.height())]
    }
}

/// The points of the selected polygon region: each dragged on its own (one undo step a drag),
/// removed by a right-click while more than three are left, and a new one added where a side is
/// double-clicked.
fn edit_points(ui: &mut egui::Ui, state: &mut EditorState, area: Rect, picture: Rect) {
    let Some((key, censor, rect, rotation, quad)) = polygon_target(state) else { return };
    let on_screen = BoxOnScreen::new(rect, rotation, quad, picture);
    let screen: Vec<Pos2> = censor.points.iter().map(|p| on_screen.to_screen(*p)).collect();
    let painter = ui.painter_at(area);
    painter.add(Shape::closed_line(screen.clone(), Stroke::new(LINE_WIDTH, DRAW_COLOR)));
    let scope = ui.scope_id().with(("censor-points", key));
    let mut over_point = false;
    for (index, point) in screen.iter().enumerate() {
        let response = ui.interact(Rect::from_center_size(*point, Vec2::splat(POINT_HIT_SIZE)).intersect(area), scope.with(index), Sense::click_and_drag());
        over_point |= response.hovered() || response.dragged();
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if response.dragged() {
            if let Some(latest) = ui.ctx().input(|input| input.pointer.latest_pos()) {
                let mut points = censor.points.clone();
                points[index] = on_screen.to_local(latest);
                state.live_censor(engine::CensorContent { points, ..censor.clone() });
            }
        }
        if response.drag_stopped() {
            state.commit_live();
        }
        if response.secondary_clicked() && censor.points.len() > MIN_POLYGON_POINTS {
            let mut points = censor.points.clone();
            points.remove(index);
            state.set_censor(engine::CensorContent { points, ..censor.clone() });
            return;
        }
        painter.rect_filled(Rect::from_center_size(*point, Vec2::splat(POINT_SIZE)), CornerRadius::ZERO, DRAW_COLOR);
    }
    if over_point || censor.points.len() >= MAX_POLYGON_POINTS {
        return;
    }
    // A side takes a double-click through a widget of its own under the pointer, so a widget on
    // top (a menu, a dialog) keeps it, and the frame under it still takes its drag.
    let Some(hover) = ui.ctx().pointer_hover_pos().filter(|pos| area.contains(*pos)) else { return };
    if screen.iter().any(|point| (*point - hover).abs().max_elem() <= POINT_HIT_SIZE / 2.0) || inserted_at(&screen, hover).is_none() {
        return;
    }
    let side = ui.interact(Rect::from_center_size(hover, Vec2::splat(2.0 * EDGE_HIT as f32)).intersect(area), scope.with("side"), Sense::click());
    let Some(pos) = side.interact_pointer_pos().filter(|_| side.double_clicked()) else { return };
    if let Some(index) = inserted_at(&screen, pos) {
        let mut points = censor.points.clone();
        points.insert(index, on_screen.to_local(pos));
        state.set_censor(engine::CensorContent { points, ..censor });
    }
}

/// Where a point double-clicked at `pos` goes among the points `screen` of a polygon: after the
/// start of the side within reach of it, if one is.
fn inserted_at(screen: &[Pos2], pos: Pos2) -> Option<usize> {
    let pixels: Vec<[f64; 2]> = screen.iter().map(|p| [f64::from(p.x), f64::from(p.y)]).collect();
    let (edge, distance) = nearest_edge(&pixels, [f64::from(pos.x), f64::from(pos.y)])?;
    (distance <= EDGE_HIT).then_some(edge + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::censor_draw::CensorDrawMode;
    use egui::PointerButton;

    #[test]
    fn a_click_on_the_first_point_closes_once_three_are_there() {
        let points = [Pos2::new(10.0, 10.0), Pos2::new(100.0, 10.0), Pos2::new(50.0, 80.0)];
        assert_eq!(points_click(&points, Pos2::new(13.0, 12.0)), PointsClick::Close);
        assert_eq!(points_click(&points, Pos2::new(30.0, 30.0)), PointsClick::Add);
        assert_eq!(points_click(&points[..2], Pos2::new(10.0, 10.0)), PointsClick::Add, "two points cannot close");
    }

    #[test]
    fn the_first_click_of_a_closing_double_click_is_dropped() {
        let picture = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let points = [[0.1, 0.1], [0.5, 0.1], [0.3, 0.5], [0.3005, 0.5]];
        assert_eq!(without_repeats(&points, picture).len(), 3);
    }

    #[test]
    fn a_double_click_near_a_side_inserts_after_its_start_the_closing_side_too() {
        let screen = [Pos2::new(0.0, 0.0), Pos2::new(100.0, 0.0), Pos2::new(100.0, 100.0), Pos2::new(0.0, 100.0)];
        assert_eq!(inserted_at(&screen, Pos2::new(50.0, 3.0)), Some(1));
        assert_eq!(inserted_at(&screen, Pos2::new(2.0, 50.0)), Some(4), "the side back to the first point");
        assert_eq!(inserted_at(&screen, Pos2::new(50.0, 50.0)), None);
    }

    #[test]
    fn a_turned_box_maps_its_points_to_the_screen_and_back() {
        let picture = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(640.0, 360.0));
        let on_screen = BoxOnScreen::new(engine::PlacedRect { x: 0.25, y: 0.25, width: 0.5, height: 0.5 }, 30.0, None, picture);
        let local = [0.2, 0.9];
        let back = on_screen.to_local(on_screen.to_screen(local));
        assert!((back[0] - local[0]).abs() < 1e-4 && (back[1] - local[1]).abs() < 1e-4, "{back:?}");
        let middle = on_screen.to_screen([0.5, 0.5]);
        assert!((middle - picture.center()).length() < 1e-3);
    }

    #[test]
    fn a_box_in_perspective_maps_its_points_onto_its_quadrilateral_and_back() {
        let picture = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(640.0, 360.0));
        let quad = [[0.3, 0.2], [0.7, 0.35], [0.7, 0.65], [0.3, 0.8]];
        let rect = engine::PlacedRect { x: 0.3, y: 0.2, width: 0.4, height: 0.6 };
        let on_screen = BoxOnScreen::new(rect, 0.0, Some(quad), picture);
        for (corner, local) in quad.iter().zip([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]) {
            assert!((on_screen.to_screen(local) - to_screen(*corner, picture)).length() < 1e-2, "corner {local:?}");
        }
        // The middle of the box is where the diagonals of the quadrilateral cross, not the middle of its bounds.
        let middle = on_screen.to_screen([0.5, 0.5]);
        assert!((middle - to_screen([1.275 / 2.25, 0.5], picture)).length() < 1e-2, "{middle:?}");
        let local = [0.2, 0.9];
        let back = on_screen.to_local(on_screen.to_screen(local));
        assert!((back[0] - local[0]).abs() < 1e-4 && (back[1] - local[1]).abs() < 1e-4, "{back:?}");
    }

    /// One frame of a 400 by 300 screen with `events`, the player's censor drawing over all of it.
    fn pass(ctx: &egui::Context, state: &mut EditorState, events: Vec<egui::Event>) {
        pass_with(ctx, state, events, false);
    }

    /// [`pass`], with a text field holding the keyboard focus when `typing`.
    fn pass_with(ctx: &egui::Context, state: &mut EditorState, events: Vec<egui::Event>, typing: bool) {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
        let mut text = String::new();
        let mut output = ctx.run_ui(input, |ui| {
            if typing {
                ui.text_edit_singleline(&mut text).request_focus();
            }
            show(ui, state, screen, screen);
        });
        output.textures_delta.clear();
    }

    fn enter() -> egui::Event {
        egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    #[test]
    fn enter_in_a_focused_text_field_does_not_close_the_polygon() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        state.toggle_censor_draw(CensorDrawMode::Points);
        for at in [Pos2::new(40.0, 30.0), Pos2::new(200.0, 30.0), Pos2::new(120.0, 150.0)] {
            click(&ctx, &mut state, at);
        }
        pass_with(&ctx, &mut state, Vec::new(), true);
        pass_with(&ctx, &mut state, vec![enter()], true);
        assert_eq!(state.censor_draw().map(|draw| draw.points.len()), Some(3), "still drawing");
        assert!(state.project().tracks.is_empty());
    }

    #[test]
    fn a_double_click_on_a_side_adds_a_point_there_and_elsewhere_nothing() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        assert!(state.finish_censor_polygon(&[[0.1, 0.1], [0.5, 0.1], [0.5, 0.5], [0.1, 0.5]]));
        let count = |state: &EditorState| {
            let key = state.selection().unwrap();
            state.project().tracks[key.track].clips[key.clip].censor.as_ref().unwrap().points.len()
        };
        let double = |state: &mut EditorState, at: Pos2| {
            pass(&ctx, state, vec![egui::Event::PointerMoved(at)]);
            for _ in 0..2 {
                pass(&ctx, state, vec![egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
                pass(&ctx, state, vec![egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
            }
            ctx.input_mut(|input| input.time += 1.0);
        };
        double(&mut state, Pos2::new(120.0, 100.0));
        assert_eq!(count(&state), 4, "inside, away from the sides");
        double(&mut state, Pos2::new(120.0, 32.0));
        assert_eq!(count(&state), 5, "on the top side");
    }

    fn click(ctx: &egui::Context, state: &mut EditorState, at: Pos2) {
        pass(ctx, state, vec![egui::Event::PointerMoved(at)]);
        pass(ctx, state, vec![egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        pass(ctx, state, vec![egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        // Far enough apart in time that two clicks are no double-click.
        ctx.input_mut(|input| input.time += 1.0);
    }

    #[test]
    fn clicked_points_closed_by_enter_make_a_polygon_censor_clip() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        state.toggle_censor_draw(CensorDrawMode::Points);
        for at in [Pos2::new(40.0, 30.0), Pos2::new(200.0, 30.0), Pos2::new(120.0, 150.0)] {
            click(&ctx, &mut state, at);
        }
        assert_eq!(state.censor_draw().map(|draw| draw.points.len()), Some(3));
        pass(&ctx, &mut state, vec![egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        assert!(state.censor_draw().is_none(), "the drawing ended");
        let key = state.selection().expect("the new clip is selected");
        let clip = &state.project().tracks[key.track].clips[key.clip];
        let censor = clip.censor.as_ref().unwrap();
        assert_eq!((censor.shape, censor.points.len()), (RegionShape::Polygon, 3));
        let placement = clip.placement.unwrap();
        assert!((placement.x - 0.1).abs() < 1e-6 && (placement.y - 0.1).abs() < 1e-6 && (placement.width - 0.4).abs() < 1e-6, "{placement:?}");
    }

    #[test]
    fn a_right_click_removes_a_point_but_keeps_three() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        assert!(state.finish_censor_polygon(&[[0.1, 0.1], [0.5, 0.1], [0.5, 0.5], [0.1, 0.5]]));
        let right = |at: Pos2, pressed| egui::Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
        let corner = Pos2::new(200.0, 150.0);
        for _ in 0..2 {
            pass(&ctx, &mut state, vec![egui::Event::PointerMoved(corner)]);
            pass(&ctx, &mut state, vec![right(corner, true)]);
            pass(&ctx, &mut state, vec![right(corner, false)]);
            ctx.input_mut(|input| input.time += 1.0);
        }
        let key = state.selection().unwrap();
        let points = &state.project().tracks[key.track].clips[key.clip].censor.as_ref().unwrap().points;
        assert_eq!(points.len(), 3, "{points:?}");
        assert!(!points.contains(&[1.0, 1.0]), "the corner under the pointer went");
    }
}
