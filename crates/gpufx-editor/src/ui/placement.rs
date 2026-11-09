//! The red frame on the player that moves and resizes the selected clip. The shape of the picture
//! never changes: every grip scales it around the side opposite the grip.

use crate::editor::EditorState;
use crate::visuals::SourceView;
use egui::{Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use engine::{set_placement, ClipKey, ClipPlacement, PlacedRect, TrackKind, MAX_PLACEMENT_OFFSET, MAX_PLACEMENT_WIDTH, MIN_PLACEMENT_WIDTH};

const FRAME_WIDTH: f32 = 1.5;
const GRIP_SIZE: f32 = 8.0;
const GRIP_HIT_SIZE: f32 = 14.0;

/// A grip, by where it sits on the frame: 0 is the left or top side, 1 the right or bottom one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grip {
    x: f64,
    y: f64,
    cursor: CursorIcon,
}

const GRIPS: [Grip; 8] = [
    Grip { x: 0.0, y: 0.0, cursor: CursorIcon::ResizeNwSe },
    Grip { x: 0.5, y: 0.0, cursor: CursorIcon::ResizeVertical },
    Grip { x: 1.0, y: 0.0, cursor: CursorIcon::ResizeNeSw },
    Grip { x: 0.0, y: 0.5, cursor: CursorIcon::ResizeHorizontal },
    Grip { x: 1.0, y: 0.5, cursor: CursorIcon::ResizeHorizontal },
    Grip { x: 0.0, y: 1.0, cursor: CursorIcon::ResizeNeSw },
    Grip { x: 0.5, y: 1.0, cursor: CursorIcon::ResizeVertical },
    Grip { x: 1.0, y: 1.0, cursor: CursorIcon::ResizeNwSe },
];

/// How much a side at `at` scales a length of `length` when dragged by `moved`. `None` for a grip
/// in the middle of this axis.
fn side_scale(at: f64, length: f64, moved: f64) -> Option<f64> {
    if at == 0.5 {
        None
    } else if at == 0.0 {
        Some((length - moved) / length)
    } else {
        Some((length + moved) / length)
    }
}

fn limited(x: f64, y: f64, width: f64) -> ClipPlacement {
    ClipPlacement { x: x.clamp(-MAX_PLACEMENT_OFFSET, MAX_PLACEMENT_OFFSET), y: y.clamp(-MAX_PLACEMENT_OFFSET, MAX_PLACEMENT_OFFSET), width }
}

/// `start` with its body dragged by `moved` parts of the frame.
fn moved_placement(start: PlacedRect, moved: (f64, f64)) -> ClipPlacement {
    limited(start.x + moved.0, start.y + moved.1, start.width.clamp(MIN_PLACEMENT_WIDTH, MAX_PLACEMENT_WIDTH))
}

/// `start` with `grip` dragged by `moved` parts of the frame. A corner follows the axis that asks
/// for the larger picture.
fn resized_placement(start: PlacedRect, grip: Grip, moved: (f64, f64)) -> ClipPlacement {
    let scale = match (side_scale(grip.x, start.width, moved.0), side_scale(grip.y, start.height, moved.1)) {
        (Some(across), Some(down)) => across.max(down),
        (Some(scale), None) | (None, Some(scale)) => scale,
        (None, None) => 1.0,
    };
    let width = (start.width * scale).clamp(MIN_PLACEMENT_WIDTH, MAX_PLACEMENT_WIDTH);
    let height = width * start.height / start.width;
    let (anchor_x, anchor_y) = (1.0 - grip.x, 1.0 - grip.y);
    limited(start.x + anchor_x * start.width - anchor_x * width, start.y + anchor_y * start.height - anchor_y * height, width)
}

/// `rect`, in parts of the frame, on the screen where the frame is `picture`.
fn on_screen(rect: PlacedRect, picture: Rect) -> Rect {
    let min = picture.min + Vec2::new(rect.x as f32 * picture.width(), rect.y as f32 * picture.height());
    Rect::from_min_size(min, Vec2::new(rect.width as f32 * picture.width(), rect.height as f32 * picture.height()))
}

/// Whether the frame is shown: never while exporting, and during playback only while one of its
/// own handles is in the hand, so another held control (the timeline, a slider) does not bring it back.
fn visible(exporting: bool, playing: bool, own_handle_dragged: bool) -> bool {
    !exporting && (!playing || own_handle_dragged)
}

fn body_id(scope: Id) -> Id {
    scope.with("placement-body")
}

fn grip_id(scope: Id, index: usize) -> Id {
    scope.with(("placement-grip", index))
}

/// Where [`drag`] keeps the frame at the press of handle `id`.
fn start_id(id: Id) -> Id {
    id.with("start")
}

/// Whether handle `id` is in the hand: dragged now, or pressed and not yet through the frame of
/// its release, which still has to reach it.
fn in_hand(ctx: &egui::Context, id: Id) -> bool {
    ctx.is_being_dragged(id) || ctx.data(|data| data.get_temp::<PlacedRect>(start_id(id)).is_some())
}

/// Whether the body or a grip of the frame is in the hand.
fn own_handle_dragged(ctx: &egui::Context, scope: Id) -> bool {
    in_hand(ctx, body_id(scope)) || (0..GRIPS.len()).any(|index| in_hand(ctx, grip_id(scope, index)))
}

/// `rect` limited to `area`, for a handle at rest. A handle in the hand keeps all of `rect`: the
/// pointer, and so the grip with it, goes where the panel ends when the picture is enlarged, and
/// a handle with no room left would end the drag.
fn handle_rect(rect: Rect, area: Rect, dragged: bool) -> Rect {
    if dragged {
        rect
    } else {
        rect.intersect(area)
    }
}

/// The clip the frame belongs to, and the width over height of its source. Only a clip that is on
/// screen now and may be edited has a frame.
fn target(state: &EditorState) -> Option<(ClipKey, Option<ClipPlacement>, f64)> {
    let key = state.selection()?;
    let track = state.project().tracks.get(key.track)?;
    let clip = track.clips.get(key.clip)?;
    if track.kind != TrackKind::Video || track.locked || track.hidden || state.playhead() < clip.start || state.playhead() >= clip.start.saturating_add(clip.duration) {
        return None;
    }
    let mut visuals = state.visuals.borrow_mut();
    let slot = visuals.slot(&clip.source);
    match visuals.view(slot) {
        SourceView::Known(facts) if facts.has_video && facts.aspect > 0.0 => Some((key, clip.placement, f64::from(facts.aspect))),
        _ => None,
    }
}

/// How far the pointer has come since the press, in parts of the frame `picture`.
fn pointer_moved(ui: &egui::Ui, picture: Rect) -> Option<(f64, f64)> {
    let moved = ui.ctx().input(|input| input.pointer.press_origin().zip(input.pointer.latest_pos()).map(|(from, to)| to - from))?;
    Some((f64::from(moved.x / picture.width()), f64::from(moved.y / picture.height())))
}

/// Runs one drag handle: remembers the frame at the press, and shows `placement` of it until the release.
fn drag(ui: &egui::Ui, state: &mut EditorState, key: ClipKey, picture: Rect, response: &egui::Response, current: PlacedRect, placement: impl Fn(PlacedRect, (f64, f64)) -> ClipPlacement) {
    let id: Id = start_id(response.id);
    if response.drag_started() {
        ui.data_mut(|data| data.insert_temp(id, current));
    }
    if response.dragged() {
        let start = ui.data(|data| data.get_temp::<PlacedRect>(id));
        if let Some((start, moved)) = start.zip(pointer_moved(ui, picture)) {
            let wanted = placement(start, moved);
            state.live_edit(|project| set_placement(project, key, Some(wanted)));
        }
    }
    if response.drag_stopped() {
        state.commit_live();
        ui.data_mut(|data| data.remove_temp::<PlacedRect>(id));
    }
}

/// Draws the frame of the selected clip over `picture`, inside `area`, and handles its grips.
pub(super) fn show(ui: &mut egui::Ui, state: &mut EditorState, area: Rect, picture: Rect) {
    if picture.width() < 1.0 || picture.height() < 1.0 || !visible(state.is_exporting(), state.is_playing(), own_handle_dragged(ui.ctx(), ui.scope_id())) {
        return;
    }
    let Some((key, placement, source_aspect)) = target(state) else { return };
    show_frame(ui, state, area, picture, key, placement, source_aspect);
}

fn show_frame(ui: &mut egui::Ui, state: &mut EditorState, area: Rect, picture: Rect, key: ClipKey, placement: Option<ClipPlacement>, source_aspect: f64) {
    let scope = ui.scope_id();
    let output = &state.project().output;
    let output_aspect = f64::from(output.width) / f64::from(output.height);
    let current = PlacedRect::of(placement, output_aspect, source_aspect);
    let frame = on_screen(current, picture);
    let body_id = body_id(scope);
    let body = ui.interact(handle_rect(frame, area, in_hand(ui.ctx(), body_id)), body_id, Sense::drag());
    drag(ui, state, key, picture, &body, current, moved_placement);
    body.on_hover_and_drag_cursor(CursorIcon::Move);
    let painter = ui.painter_at(area);
    painter.rect_stroke(frame, CornerRadius::ZERO, Stroke::new(FRAME_WIDTH, Color32::RED), StrokeKind::Middle);
    for (index, grip) in GRIPS.into_iter().enumerate() {
        let center = Pos2::new(frame.left() + grip.x as f32 * frame.width(), frame.top() + grip.y as f32 * frame.height());
        let id = grip_id(scope, index);
        let hit = handle_rect(Rect::from_center_size(center, Vec2::splat(GRIP_HIT_SIZE)), area, in_hand(ui.ctx(), id));
        if hit.is_positive() {
            let response = ui.interact(hit, id, Sense::drag());
            drag(ui, state, key, picture, &response, current, |start, moved| resized_placement(start, grip, moved));
            response.on_hover_and_drag_cursor(grip.cursor);
        }
        painter.rect_filled(Rect::from_center_size(center, Vec2::splat(GRIP_SIZE)), CornerRadius::ZERO, Color32::RED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: PlacedRect = PlacedRect { x: 0.25, y: 0.25, width: 0.5, height: 0.5 };

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn grip(x: f64, y: f64) -> Grip {
        *GRIPS.iter().find(|grip| grip.x == x && grip.y == y).expect("a grip")
    }

    #[test]
    fn the_body_moves_without_resizing() {
        let moved = moved_placement(START, (0.1, -0.2));
        assert!(close(moved.x, 0.35) && close(moved.y, 0.05) && close(moved.width, 0.5), "{moved:?}");
    }

    #[test]
    fn a_corner_keeps_the_opposite_corner_in_place() {
        let resized = resized_placement(START, grip(0.0, 0.0), (-0.25, -0.1));
        assert!(close(resized.width, 0.75), "{resized:?}");
        assert!(close(resized.x + resized.width, 0.75), "the right edge stays: {resized:?}");
        assert!(close(resized.y + 0.75, 0.75), "the bottom edge stays: {resized:?}");
    }

    #[test]
    fn a_side_keeps_the_opposite_side_and_the_shape() {
        let resized = resized_placement(START, grip(1.0, 0.5), (0.25, 0.4));
        assert!(close(resized.x, 0.25) && close(resized.width, 0.75), "{resized:?}");
        assert!(close(resized.y + 0.375, 0.5), "the height grows around the middle: {resized:?}");

        let resized = resized_placement(START, grip(0.5, 0.0), (0.4, -0.25));
        assert!(close(resized.width, 0.75) && close(resized.y, 0.0), "{resized:?}");
        assert!(close(resized.x + 0.375, 0.5), "the width grows around the middle: {resized:?}");
    }

    #[test]
    fn the_picture_can_grow_past_the_frame_up_to_the_limit() {
        let resized = resized_placement(START, grip(1.0, 1.0), (1.0, 1.0));
        assert!(close(resized.width, 1.5), "{resized:?}");
        assert!(resized_placement(START, grip(1.0, 1.0), (100.0, 100.0)).is_valid());
    }

    #[test]
    fn a_drag_past_the_far_side_stops_at_the_smallest_picture() {
        let resized = resized_placement(START, grip(1.0, 1.0), (-5.0, -5.0));
        assert!(close(resized.width, MIN_PLACEMENT_WIDTH) && resized.is_valid(), "{resized:?}");
    }

    #[test]
    fn a_shape_that_is_not_square_stays_the_same() {
        let start = PlacedRect { x: 0.0, y: 0.0, width: 0.4, height: 0.2 };
        let resized = resized_placement(start, grip(0.0, 1.0), (0.0, 0.2));
        assert!(close(resized.width, 0.8), "double height means double width: {resized:?}");
    }

    #[test]
    fn during_playback_only_a_handle_of_the_frame_in_the_hand_keeps_it() {
        assert!(visible(false, false, false));
        assert!(!visible(false, true, false), "playing, even while another control is held");
        assert!(visible(false, true, true), "playing, and a grip is in the hand");
        assert!(!visible(true, false, false), "exporting");
        assert!(!visible(true, true, true));
    }

    #[test]
    fn a_handle_in_the_hand_keeps_the_part_outside_the_panel() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 100.0));
        let grip = Rect::from_center_size(Pos2::new(150.0, 50.0), Vec2::splat(14.0));
        assert!(!handle_rect(grip, area, false).is_positive(), "at rest it is clipped away");
        assert_eq!(handle_rect(grip, area, true), grip);
    }

    fn pass(ctx: &egui::Context, state: &mut EditorState, area: Rect, events: Vec<egui::Event>) {
        let raw = egui::RawInput { events, screen_rect: Some(area), ..Default::default() };
        let key = ClipKey { track: 0, clip: 0 };
        let placement = state.project().tracks[0].clips[0].placement;
        let output = &state.project().output;
        let aspect = f64::from(output.width) / f64::from(output.height);
        let mut output = ctx.run_ui(raw, |ui| show_frame(ui, state, area, area, key, placement, aspect));
        output.textures_delta.clear();
    }

    fn button(at: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    fn width_now(state: &EditorState) -> f64 {
        let output = &state.project().output;
        let aspect = f64::from(output.width) / f64::from(output.height);
        PlacedRect::of(state.project().tracks[0].clips[0].placement, aspect, aspect).width
    }

    #[test]
    fn a_grip_dragged_past_the_panel_edge_keeps_enlarging_the_picture() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, std::path::Path::new("a.mp4"));
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 225.0));
        pass(&ctx, &mut state, area, vec![]);
        pass(&ctx, &mut state, area, vec![]);
        // The bottom right grip sits on the corner of the panel.
        let grip = Pos2::new(397.0, 222.0);
        pass(&ctx, &mut state, area, vec![egui::Event::PointerMoved(grip)]);
        pass(&ctx, &mut state, area, vec![button(grip, true)]);
        let mut widths = Vec::new();
        for step in 1..=6 {
            let to = grip + Vec2::new(40.0 * step as f32, 22.5 * step as f32);
            pass(&ctx, &mut state, area, vec![egui::Event::PointerMoved(to)]);
            pass(&ctx, &mut state, area, vec![]);
            widths.push(width_now(&state));
        }
        assert!(widths.windows(2).all(|pair| pair[1] > pair[0] + 1e-6), "every step past the edge grows the picture: {widths:?}");
        assert!(widths[5] > 1.5, "{widths:?}");
        let last = grip + Vec2::new(240.0, 135.0);
        pass(&ctx, &mut state, area, vec![button(last, false)]);
        pass(&ctx, &mut state, area, vec![]);
        assert!(!state.is_live(), "the release ends the edit");
        assert!(state.can_undo());
    }

    #[test]
    fn the_frame_lands_on_the_picture() {
        let picture = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 100.0));
        let rect = on_screen(PlacedRect { x: 0.5, y: 0.5, width: 0.25, height: 0.5 }, picture);
        assert_eq!(rect, Rect::from_min_size(Pos2::new(110.0, 70.0), Vec2::new(50.0, 50.0)));
    }
}
