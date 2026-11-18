//! The classical tracker on made-up frames: a textured square that moves, turns and grows over a
//! nearly flat background.

use std::sync::atomic::{AtomicBool, Ordering};

use engine::tracker::flow::{track_point, FlowOptions};
use engine::tracker::{track_frames, GrayImage, Pyramid, RunEnd, Similarity, TrackBox, Tracker, TrackerOptions, LOST_CONFIDENCE};

const WIDTH: usize = 240;
const HEIGHT: usize = 180;
/// The side of the square at scale 1, in pixels.
const SIDE: f64 = 60.0;
/// The side of one cell of the square's texture, in pixels of the square at scale 1.
const CELL: f64 = 6.0;
const GRID: usize = 16;

/// Where the square is in one frame.
#[derive(Debug, Clone, Copy)]
struct Pose {
    cx: f64,
    cy: f64,
    scale: f64,
    angle: f64,
}

impl Pose {
    fn area(&self) -> TrackBox {
        TrackBox { cx: self.cx, cy: self.cy, w: SIDE * self.scale, h: SIDE * self.scale, angle: self.angle }
    }
}

/// A fixed grid of random brightness values, the texture of the square.
fn texture_grid() -> Vec<f64> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..(GRID + 1) * (GRID + 1))
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            0.1 + 0.8 * (state % 1000) as f64 / 1000.0
        })
        .collect()
}

/// The texture at (`u`, `v`) pixels of the square at scale 1, smoothly between the grid values.
fn texture(grid: &[f64], u: f64, v: f64) -> f64 {
    let (gx, gy) = (u / CELL, v / CELL);
    let (ix, iy) = (gx.floor().clamp(0.0, (GRID - 1) as f64) as usize, gy.floor().clamp(0.0, (GRID - 1) as f64) as usize);
    let smooth = |t: f64| t * t * (3.0 - 2.0 * t);
    let (ax, ay) = (smooth((gx - ix as f64).clamp(0.0, 1.0)), smooth((gy - iy as f64).clamp(0.0, 1.0)));
    let at = |x: usize, y: usize| grid[y * (GRID + 1) + x];
    let top = at(ix, iy) * (1.0 - ax) + at(ix + 1, iy) * ax;
    let bottom = at(ix, iy + 1) * (1.0 - ax) + at(ix + 1, iy + 1) * ax;
    top * (1.0 - ay) + bottom * ay
}

fn background(x: f64, y: f64) -> f64 {
    0.5 + 0.03 * (x / 50.0).sin() + 0.03 * (y / 45.0).cos()
}

/// One frame with the square at `pose`, or none, 3x3 samples per pixel.
fn render(grid: &[f64], pose: Option<Pose>) -> GrayImage {
    let mut data = Vec::with_capacity(WIDTH * HEIGHT);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut sum = 0.0;
            for sy in 0..3 {
                for sx in 0..3 {
                    let (px, py) = (x as f64 + (sx as f64 - 1.0) / 3.0, y as f64 + (sy as f64 - 1.0) / 3.0);
                    let mut value = background(px, py);
                    if let Some(pose) = pose {
                        let (sine, cosine) = pose.angle.to_radians().sin_cos();
                        let (dx, dy) = (px - pose.cx, py - pose.cy);
                        let u = (cosine * dx + sine * dy) / pose.scale + SIDE / 2.0;
                        let v = (-sine * dx + cosine * dy) / pose.scale + SIDE / 2.0;
                        if (0.0..SIDE).contains(&u) && (0.0..SIDE).contains(&v) {
                            value = texture(grid, u, v);
                        }
                    }
                    sum += value;
                }
            }
            data.push((sum / 9.0) as f32);
        }
    }
    GrayImage::new(WIDTH, HEIGHT, data)
}

/// Tracks the square through `poses` from the first and checks every step against the truth:
/// middle within a pixel, size within 3 % and angle within 1.5 degrees.
fn check_run(poses: &[Pose], options: TrackerOptions) {
    let grid = texture_grid();
    let mut tracker = Tracker::new(render(&grid, Some(poses[0])), poses[0].area(), options);
    let frames = poses[1..].iter().map(|pose| Ok::<_, ()>(render(&grid, Some(*pose))));
    let (steps, end) = track_frames(&mut tracker, frames, &AtomicBool::new(false), |_, _| {}).unwrap();
    assert_eq!(end, RunEnd::Finished);
    assert_eq!(steps.len(), poses.len() - 1);
    for (i, (step, pose)) in steps.iter().zip(&poses[1..]).enumerate() {
        let truth = pose.area();
        let error = (step.area.cx - truth.cx).hypot(step.area.cy - truth.cy);
        assert!(error < 1.0, "frame {}: middle off by {error:.3} px ({step:?} vs {truth:?})", i + 1);
        assert!((step.area.w / truth.w - 1.0).abs() < 0.03, "frame {}: size {} vs {}", i + 1, step.area.w, truth.w);
        let turn = (step.area.angle - truth.angle + 180.0).rem_euclid(360.0) - 180.0;
        assert!(turn.abs() < 1.5, "frame {}: angle {} vs {}", i + 1, step.area.angle, truth.angle);
        assert!(step.confidence > 0.7 && !step.lost, "frame {}: confidence {}", i + 1, step.confidence);
    }
}

fn poses(count: usize, pose: impl Fn(f64) -> Pose) -> Vec<Pose> {
    (0..count).map(|i| pose(i as f64)).collect()
}

#[test]
fn a_moving_square_is_followed_within_a_pixel() {
    check_run(&poses(30, |i| Pose { cx: 70.0 + 2.5 * i, cy: 70.0 + 1.5 * i, scale: 1.0, angle: 0.0 }), TrackerOptions::default());
}

#[test]
fn a_fast_moving_square_is_followed_within_a_pixel() {
    check_run(&poses(12, |i| Pose { cx: 50.0 + 11.0 * i, cy: 90.0 - 3.0 * i, scale: 1.0, angle: 0.0 }), TrackerOptions::default());
}

#[test]
fn a_turning_square_is_followed_with_its_angle() {
    check_run(&poses(30, |i| Pose { cx: 120.0, cy: 90.0, scale: 1.0, angle: 3.0 * i }), TrackerOptions::default());
}

#[test]
fn a_growing_square_is_followed_with_its_size() {
    check_run(&poses(30, |i| Pose { cx: 120.0 + 0.5 * i, cy: 90.0, scale: 0.8 * 1.015f64.powf(i), angle: 0.0 }), TrackerOptions::default());
}

#[test]
fn a_square_that_moves_turns_and_shrinks_at_once_is_followed() {
    check_run(&poses(25, |i| Pose { cx: 80.0 + 2.0 * i, cy: 100.0 - 1.0 * i, scale: 1.1 * 0.99f64.powf(i), angle: -2.0 * i }), TrackerOptions::default());
}

#[test]
fn without_rotation_the_box_keeps_its_angle() {
    let grid = texture_grid();
    let start = Pose { cx: 80.0, cy: 90.0, scale: 1.0, angle: 0.0 };
    let options = TrackerOptions { rotation: false, ..TrackerOptions::default() };
    let mut tracker = Tracker::new(render(&grid, Some(start)), start.area(), options);
    for i in 1..10 {
        let pose = Pose { cx: 80.0 + 3.0 * f64::from(i), ..start };
        let step = tracker.step(render(&grid, Some(pose)));
        assert_eq!(step.area.angle, 0.0);
        assert!((step.area.cx - pose.cx).abs() < 1.0, "{step:?}");
    }
}

#[test]
fn a_square_that_vanishes_stops_the_run_with_low_confidence() {
    let grid = texture_grid();
    let start = Pose { cx: 80.0, cy: 90.0, scale: 1.0, angle: 0.0 };
    let mut tracker = Tracker::new(render(&grid, Some(start)), start.area(), TrackerOptions::default());
    let frames = (1..20).map(|i| Ok::<_, ()>(render(&grid, (i < 8).then(|| Pose { cx: 80.0 + 2.0 * f64::from(i), ..start }))));
    let (steps, end) = track_frames(&mut tracker, frames, &AtomicBool::new(false), |_, _| {}).unwrap();
    assert_eq!(end, RunEnd::Lost);
    assert_eq!(steps.len(), 8, "the first frame without the square is the last step");
    let last = steps.last().unwrap();
    assert!(last.lost && last.confidence < LOST_CONFIDENCE, "{last:?}");
    assert!((last.area.cx - 94.0).abs() < 1.0, "a lost step stays where the square was last: {last:?}");
    assert!(steps[..7].iter().all(|step| !step.lost && step.confidence > 0.7));
}

#[test]
fn a_square_hidden_for_a_frame_is_found_again_by_its_look() {
    let grid = texture_grid();
    let start = Pose { cx: 80.0, cy: 90.0, scale: 1.0, angle: 0.0 };
    let options = TrackerOptions { lost_threshold: 0.5, ..TrackerOptions::default() };
    let mut tracker = Tracker::new(render(&grid, Some(start)), start.area(), options);
    // The square jumps farther than the flow can follow; the search finds it.
    let jumped = Pose { cx: 150.0, cy: 100.0, ..start };
    let step = tracker.step(render(&grid, Some(jumped)));
    assert!(!step.lost, "{step:?}");
    assert!((step.area.cx - jumped.cx).hypot(step.area.cy - jumped.cy) < 1.0, "{step:?}");
}

#[test]
fn a_run_stops_when_cancelled() {
    let grid = texture_grid();
    let start = Pose { cx: 80.0, cy: 90.0, scale: 1.0, angle: 0.0 };
    let mut tracker = Tracker::new(render(&grid, Some(start)), start.area(), TrackerOptions::default());
    let cancel = AtomicBool::new(false);
    let mut rendered = 0;
    let frames = (1..30).map(|i| {
        rendered += 1;
        Ok::<_, ()>(render(&grid, Some(Pose { cx: 80.0 + f64::from(i), ..start })))
    });
    let (steps, end) = track_frames(&mut tracker, frames, &cancel, |index, _| {
        if index == 3 {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .unwrap();
    assert_eq!(end, RunEnd::Cancelled);
    assert_eq!(steps.len(), 4);
    assert_eq!(rendered, 4, "no frame is read after the cancel");
}

#[test]
fn a_box_round_trips_through_a_sample() {
    let area = TrackBox { cx: 59.5, cy: 29.5, w: 24.0, h: 12.0, angle: 30.0 };
    let sample = area.to_sample(120, 60, engine::Time::from_ticks(3000), 0.9);
    assert!((sample.cx - 0.5).abs() < 1e-12 && (sample.cy - 0.5).abs() < 1e-12 && (sample.w - 0.2).abs() < 1e-12 && (sample.h - 0.2).abs() < 1e-12);
    let back = TrackBox::from_sample(&sample, 120, 60);
    assert!((back.cx - area.cx).abs() < 1e-9 && (back.h - area.h).abs() < 1e-9 && back.angle == 30.0);
}

#[test]
fn a_read_cut_short_by_the_cancel_ends_the_run_as_cancelled() {
    let grid = texture_grid();
    let start = Pose { cx: 80.0, cy: 90.0, scale: 1.0, angle: 0.0 };
    let mut tracker = Tracker::new(render(&grid, Some(start)), start.area(), TrackerOptions::default());
    let cancel = AtomicBool::new(false);
    // The decoder gives up with an error once it sees the cancel, before the loop checks it.
    let frames = (1..30).map(|i| {
        if i == 4 {
            cancel.store(true, Ordering::Relaxed);
            return Err("cancelled");
        }
        Ok(render(&grid, Some(Pose { cx: 80.0 + f64::from(i), ..start })))
    });
    let (steps, end) = track_frames(&mut tracker, frames, &cancel, |_, _| {}).expect("a cancel is not a failure");
    assert_eq!(end, RunEnd::Cancelled);
    assert_eq!(steps.len(), 3, "the steps made before the cancel stay");
    let failing = (1..3).map(|_| Err::<GrayImage, _>("broken"));
    let mut tracker = Tracker::new(render(&grid, Some(start)), start.area(), TrackerOptions::default());
    assert_eq!(track_frames(&mut tracker, failing, &AtomicBool::new(false), |_, _| {}).err(), Some("broken"), "without a cancel an error still fails the run");
}

#[test]
fn without_rotation_a_turned_object_keeps_its_scale() {
    let points = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0], [10.0, 10.0], [5.0, 3.0]];
    for degrees in [30.0f64, 60.0, 120.0, 170.0] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let pairs: Vec<_> = points.iter().map(|p| (*p, [cos * p[0] - sin * p[1] + 7.0, sin * p[0] + cos * p[1] - 2.0])).collect();
        let fitted = Similarity::fit(&pairs, false).unwrap();
        assert!((fitted.a - 1.0).abs() < 1e-9 && fitted.b == 0.0, "{degrees} degrees: {fitted:?}");
        let two = Similarity::from_two(pairs[0].0, pairs[1].0, pairs[0].1, pairs[1].1, false).unwrap();
        assert!((two.a - fitted.a).abs() < 1e-9, "both fits agree: {two:?} vs {fitted:?}");
    }
}

#[test]
fn a_texture_only_the_full_picture_shows_is_still_followed() {
    // A texture of period 4 that is zero on every even pixel: halving keeps only even pixels, so
    // the smaller level is flat while the full picture is strongly textured.
    let side = 128;
    let wave = |n: usize| [0.0, 1.0, 0.0, -1.0][n % 4];
    let data = (0..side * side).map(|i| 0.5 + 0.3 * wave(i % side) * wave(i / side)).collect();
    let pyramid = Pyramid::new(GrayImage::new(side, side, data), 2);
    assert_eq!(pyramid.levels.len(), 2);
    let found = track_point(&pyramid, &pyramid, [64.0, 64.0], [64.0, 64.0], &FlowOptions::default());
    let found = found.expect("a weak coarse level does not reject the point");
    assert!((found[0] - 64.0).abs() < 0.01 && (found[1] - 64.0).abs() < 0.01, "{found:?}");
}
