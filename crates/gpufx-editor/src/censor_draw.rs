//! Drawing a censor region on the preview: a dragged rectangle or ellipse, a polygon clicked point
//! by point, or a path drawn by hand that closes on release. The finished shape becomes a censor
//! clip at the playhead whose box fits it. The preview side is `ui/censor_overlay.rs`.

use engine::polygon::{closed_polygon, fit_in_box, MAX_POLYGON_POINTS, MIN_POLYGON_POINTS};
use engine::{CensorContent, ClipPlacement, RegionShape, CENSOR_ASPECT_RANGE, MAX_PLACEMENT_WIDTH, MIN_PLACEMENT_WIDTH};

use crate::editor::EditorState;

/// The smallest side of a drawn box, in parts of the frame: a shorter drag is taken for a click.
pub const MIN_DRAWN_SIZE: f64 = 0.005;
/// How far a hand-drawn path may stray from its simplified polygon, in pixels of the preview.
pub const FREEHAND_TOLERANCE: f64 = 1.5;

/// How the next censor region is drawn on the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CensorDrawMode {
    /// Drag a rectangle.
    Rect,
    /// Drag the box of an ellipse.
    Ellipse,
    /// Click each point; click the first point again or press Enter to close, or double-click to
    /// add a last point and close.
    Points,
    /// Hold the button and draw; the release closes the path.
    Freehand,
}

/// A censor region being drawn: its mode and the points so far, in parts of the frame (the
/// clicked points, or the path drawn by hand).
#[derive(Debug, Clone, PartialEq)]
pub struct CensorDraw {
    pub mode: CensorDrawMode,
    pub points: Vec<[f64; 2]>,
}

/// A hand-drawn `path` (in parts of a frame shown `scale` pixels wide and high) as a closed polygon,
/// simplified in pixels with `tolerance`. `None` when too little of it is left.
pub fn freehand_polygon(path: &[[f64; 2]], scale: (f64, f64), tolerance: f64) -> Option<Vec<[f64; 2]>> {
    let (sx, sy) = (scale.0.max(1.0), scale.1.max(1.0));
    let pixels: Vec<[f64; 2]> = path.iter().map(|p| [p[0] * sx, p[1] * sy]).collect();
    let polygon = closed_polygon(&pixels, tolerance)?;
    Some(polygon.into_iter().map(|p| [p[0] / sx, p[1] / sy]).collect())
}

/// The censor region and its box for a shape drawn over the frame: `shape` with `points` in parts
/// of the frame (only for a polygon) inside the box `bounds` (left, top, right, bottom, in parts of
/// the frame), for an output of `output` pixels. `None` for a box too small to see.
pub fn drawn_censor(shape: RegionShape, points: &[[f64; 2]], bounds: (f64, f64, f64, f64), output: (u32, u32)) -> Option<(CensorContent, ClipPlacement)> {
    let (left, top, right, bottom) = bounds;
    let (width, height) = (right - left, bottom - top);
    if !(width >= MIN_DRAWN_SIZE && height >= MIN_DRAWN_SIZE) {
        return None;
    }
    if shape == RegionShape::Polygon && !(MIN_POLYGON_POINTS..=MAX_POLYGON_POINTS).contains(&points.len()) {
        return None;
    }
    // The box a clip can take: its shape and width kept in range. A drawing too thin or too
    // narrow for them gets a box grown around its middle, so the points keep their true place
    // inside it; only a drawing wider than the widest box is squeezed.
    let output_aspect = f64::from(output.0) / f64::from(output.1.max(1));
    let aspect = (width * output_aspect / height).clamp(*CENSOR_ASPECT_RANGE.start(), *CENSOR_ASPECT_RANGE.end());
    let mut box_width = width.max(MIN_PLACEMENT_WIDTH);
    if box_width * output_aspect / aspect < height {
        box_width = height * aspect / output_aspect;
    }
    let box_width = box_width.min(MAX_PLACEMENT_WIDTH);
    let box_height = box_width * output_aspect / aspect;
    let (box_left, box_top) = (left - (box_width - width) / 2.0, top - (box_height - height) / 2.0);
    let local = if shape == RegionShape::Polygon {
        points.iter().map(|p| [((p[0] - box_left) / box_width).clamp(0.0, 1.0), ((p[1] - box_top) / box_height).clamp(0.0, 1.0)]).collect()
    } else {
        Vec::new()
    };
    let censor = CensorContent { shape, points: local, aspect, ..CensorContent::default() };
    let placement = ClipPlacement { x: box_left, y: box_top, width: box_width };
    Some((censor, placement))
}

impl EditorState {
    /// Starts drawing a censor region in `mode` on the preview, or stops when that mode is on.
    pub fn toggle_censor_draw(&mut self, mode: CensorDrawMode) {
        let same = self.ui.censor_draw.as_ref().is_some_and(|draw| draw.mode == mode);
        self.end_censor_draw();
        if !same {
            self.cancel_track_pick();
            self.ui.censor_draw = Some(CensorDraw { mode, points: Vec::new() });
        }
    }

    /// The censor region being drawn on the preview, if one is.
    pub fn censor_draw(&self) -> Option<&CensorDraw> {
        self.ui.censor_draw.as_ref()
    }

    /// The points of the censor region being drawn, to add to.
    pub fn censor_draw_points(&mut self) -> Option<&mut Vec<[f64; 2]>> {
        self.ui.censor_draw.as_mut().map(|draw| &mut draw.points)
    }

    /// Stops drawing a censor region, dropping what was drawn (Escape). Whether one was going.
    pub fn cancel_censor_draw(&mut self) -> bool {
        let going = self.ui.censor_draw.is_some();
        self.end_censor_draw();
        going
    }

    fn end_censor_draw(&mut self) {
        if self.ui.censor_draw.take().is_some() {
            self.ui.censor_draw_round = self.ui.censor_draw_round.wrapping_add(1);
        }
    }

    /// Ends the drawing with a dragged box (left, top, right, bottom in parts of the frame) for a
    /// rectangle or an ellipse. Whether a censor clip was added.
    pub fn finish_censor_box(&mut self, shape: RegionShape, bounds: (f64, f64, f64, f64)) -> bool {
        self.finish_censor(shape, &[], bounds)
    }

    /// Ends the drawing with a closed polygon of `points` in parts of the frame. Whether a censor
    /// clip was added; too few points, or a box too small, add nothing and keep drawing.
    pub fn finish_censor_polygon(&mut self, points: &[[f64; 2]]) -> bool {
        let Some((_, bounds)) = fit_in_box(points, 0.0) else { return false };
        self.finish_censor(RegionShape::Polygon, points, bounds)
    }

    fn finish_censor(&mut self, shape: RegionShape, points: &[[f64; 2]], bounds: (f64, f64, f64, f64)) -> bool {
        let Some((censor, placement)) = drawn_censor(shape, points, bounds, self.output_pixels()) else {
            // Clicked points stay, so a polygon too small to close can be drawn on; a dragged box
            // or a hand-drawn path starts over.
            let clicked = self.ui.censor_draw.as_ref().is_some_and(|draw| draw.mode == CensorDrawMode::Points);
            if let Some(points) = self.censor_draw_points().filter(|_| !clicked) {
                points.clear();
            }
            return false;
        };
        self.end_censor_draw();
        self.add_censor_clip_at_playhead(censor, placement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Time;

    #[test]
    fn a_drawn_box_becomes_the_box_of_the_clip_with_its_shape() {
        let (censor, placement) = drawn_censor(RegionShape::Ellipse, &[], (0.25, 0.25, 0.75, 0.5), (1920, 1080)).unwrap();
        assert_eq!(censor.shape, RegionShape::Ellipse);
        assert!(censor.points.is_empty());
        assert!((censor.aspect - 0.5 * 1920.0 / (0.25 * 1080.0)).abs() < 1e-9);
        assert_eq!((placement.x, placement.y, placement.width), (0.25, 0.25, 0.5));
    }

    #[test]
    fn a_polygon_is_kept_in_parts_of_its_box() {
        let points = [[0.2, 0.2], [0.6, 0.2], [0.4, 0.6]];
        let (censor, placement) = drawn_censor(RegionShape::Polygon, &points, (0.2, 0.2, 0.6, 0.6), (1000, 1000)).unwrap();
        assert_eq!(censor.shape, RegionShape::Polygon);
        assert!(censor.problem().is_none(), "{:?}", censor.problem());
        assert!((censor.points[2][0] - 0.5).abs() < 1e-9 && (censor.points[2][1] - 1.0).abs() < 1e-9);
        assert_eq!((placement.x, placement.y), (0.2, 0.2));
        assert!((placement.width - 0.4).abs() < 1e-9);
    }

    /// Where `local` of the box `placement` (of shape `aspect` in a square output) lands in the frame.
    fn in_frame(local: [f64; 2], placement: ClipPlacement, aspect: f64) -> [f64; 2] {
        [placement.x + local[0] * placement.width, placement.y + local[1] * placement.width / aspect]
    }

    #[test]
    fn a_drawing_past_the_widest_shape_keeps_its_points_where_they_were_drawn() {
        // 125 times wider than high: the box is held at 100 and grows around the middle.
        let points = [[0.0, 0.5], [1.0, 0.5], [0.5, 0.508]];
        let (censor, placement) = drawn_censor(RegionShape::Polygon, &points, (0.0, 0.5, 1.0, 0.508), (1000, 1000)).unwrap();
        assert!(censor.problem().is_none(), "{:?}", censor.problem());
        assert!((censor.aspect - 100.0).abs() < 1e-9);
        for (local, drawn) in censor.points.iter().zip(points) {
            let back = in_frame(*local, placement, censor.aspect);
            assert!((back[0] - drawn[0]).abs() < 1e-9 && (back[1] - drawn[1]).abs() < 1e-9, "{back:?} for {drawn:?}");
        }
        // A rectangle keeps the middle of what was drawn.
        let (censor, placement) = drawn_censor(RegionShape::Rect, &[], (0.0, 0.5, 1.0, 0.508), (1000, 1000)).unwrap();
        let middle = in_frame([0.5, 0.5], placement, censor.aspect);
        assert!((middle[0] - 0.5).abs() < 1e-9 && (middle[1] - 0.504).abs() < 1e-9, "{middle:?}");
    }

    #[test]
    fn a_drawing_narrower_than_the_narrowest_box_keeps_its_points() {
        let points = [[0.5, 0.2], [0.506, 0.2], [0.503, 0.7]];
        let (censor, placement) = drawn_censor(RegionShape::Polygon, &points, (0.5, 0.2, 0.506, 0.7), (1000, 1000)).unwrap();
        assert!(placement.width >= MIN_PLACEMENT_WIDTH);
        for (local, drawn) in censor.points.iter().zip(points) {
            let back = in_frame(*local, placement, censor.aspect);
            assert!((back[0] - drawn[0]).abs() < 1e-9 && (back[1] - drawn[1]).abs() < 1e-9, "{back:?} for {drawn:?}");
        }
    }

    #[test]
    fn clicked_points_too_small_to_close_are_kept_to_draw_on() {
        let mut state = EditorState::default();
        state.toggle_censor_draw(CensorDrawMode::Points);
        let tiny = [[0.5, 0.5], [0.501, 0.5], [0.5, 0.501]];
        state.censor_draw_points().unwrap().extend(tiny);
        assert!(!state.finish_censor_polygon(&tiny));
        assert_eq!(state.censor_draw().map(|draw| draw.points.len()), Some(3), "the points stay");
        state.toggle_censor_draw(CensorDrawMode::Freehand);
        state.censor_draw_points().unwrap().extend(tiny);
        assert!(!state.finish_censor_polygon(&tiny));
        assert_eq!(state.censor_draw().map(|draw| draw.points.len()), Some(0), "a hand-drawn path starts over");
    }

    #[test]
    fn a_click_or_a_line_makes_no_region() {
        assert!(drawn_censor(RegionShape::Rect, &[], (0.5, 0.5, 0.501, 0.7), (1920, 1080)).is_none());
        assert!(drawn_censor(RegionShape::Polygon, &[[0.1, 0.1], [0.5, 0.5]], (0.1, 0.1, 0.5, 0.5), (1920, 1080)).is_none());
    }

    #[test]
    fn a_hand_drawn_circle_is_simplified_in_pixels_of_the_preview() {
        let path: Vec<[f64; 2]> = (0..=500).map(|i| {
            let a = std::f64::consts::TAU * f64::from(i) / 500.0;
            [0.5 + 0.2 * a.cos(), 0.5 + 0.3 * a.sin()]
        }).collect();
        let polygon = freehand_polygon(&path, (800.0, 450.0), FREEHAND_TOLERANCE).unwrap();
        assert!(polygon.len() >= 8 && polygon.len() < 200, "{} points", polygon.len());
        assert!(engine::polygon::contains(&polygon, [0.5, 0.5]));
    }

    #[test]
    fn drawing_a_polygon_adds_a_censor_clip_at_the_playhead_and_ends_the_drawing() {
        let mut state = EditorState::default();
        state.toggle_censor_draw(CensorDrawMode::Points);
        assert_eq!(state.censor_draw().map(|draw| draw.mode), Some(CensorDrawMode::Points));
        assert!(state.finish_censor_polygon(&[[0.1, 0.2], [0.5, 0.2], [0.3, 0.6]]));
        assert!(state.censor_draw().is_none());
        let key = state.selection().expect("the new clip is selected");
        let clip = &state.project().tracks[key.track].clips[key.clip];
        assert_eq!(clip.start, Time::ZERO);
        let censor = clip.censor.as_ref().unwrap();
        assert_eq!((censor.shape, censor.points.len()), (RegionShape::Polygon, 3));
        let placement = clip.placement.unwrap();
        assert!((placement.x - 0.1).abs() < 1e-9 && (placement.y - 0.2).abs() < 1e-9 && (placement.width - 0.4).abs() < 1e-9);
        assert!(state.can_undo());
    }

    #[test]
    fn the_same_mode_toggles_off_and_escape_cancels() {
        let mut state = EditorState::default();
        state.toggle_censor_draw(CensorDrawMode::Rect);
        state.toggle_censor_draw(CensorDrawMode::Freehand);
        assert_eq!(state.censor_draw().map(|draw| draw.mode), Some(CensorDrawMode::Freehand));
        state.toggle_censor_draw(CensorDrawMode::Freehand);
        assert!(state.censor_draw().is_none());
        state.toggle_censor_draw(CensorDrawMode::Points);
        state.censor_draw_points().unwrap().push([0.5, 0.5]);
        assert!(state.cancel_censor_draw());
        assert!(!state.cancel_censor_draw());
        assert!(state.project().tracks.is_empty(), "nothing was added");
    }
}
