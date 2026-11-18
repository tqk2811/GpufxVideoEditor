//! The planar tracker on made-up frames: a textured flat square seen in perspective, its four
//! corners moving under a known homography per frame.

use std::sync::atomic::AtomicBool;

use engine::tracker::homography::Homography;
use engine::tracker::{track_frames, GrayImage, PlanarTracker, Quad, RunEnd, TrackerOptions};

const WIDTH: usize = 320;
const HEIGHT: usize = 240;
/// The side of the texture square, in texture units.
const SIDE: f64 = 96.0;
const CELL: f64 = 6.0;
const GRID: usize = 16;

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

const TEXTURE_SQUARE: Quad = [[0.0, 0.0], [SIDE, 0.0], [SIDE, SIDE], [0.0, SIDE]];

/// One frame with the texture square drawn at the corners `quad` (pixel centres at whole numbers),
/// 3x3 samples per pixel.
fn render(grid: &[f64], quad: &Quad) -> GrayImage {
    let back = Homography::from_quads(quad, &TEXTURE_SQUARE).unwrap();
    let mut data = Vec::with_capacity(WIDTH * HEIGHT);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut sum = 0.0;
            for sy in 0..3 {
                for sx in 0..3 {
                    let (px, py) = (x as f64 + (sx as f64 - 1.0) / 3.0, y as f64 + (sy as f64 - 1.0) / 3.0);
                    let value = match back.apply([px, py]) {
                        Some([u, v]) if (0.0..SIDE).contains(&u) && (0.0..SIDE).contains(&v) => texture(grid, u, v),
                        _ => background(px, py),
                    };
                    sum += value;
                }
            }
            data.push((sum / 9.0) as f32);
        }
    }
    GrayImage::new(WIDTH, HEIGHT, data)
}

/// The corners at frame `k` of a run: the square turns away in depth (its right side shrinks),
/// slides and turns a little.
fn corners_at(k: usize) -> Quad {
    let t = k as f64;
    let base: Quad = [[100.0, 70.0], [210.0, 70.0], [210.0, 170.0], [100.0, 170.0]];
    let squeeze = 1.2 * t; // the right side gets shorter
    let slide = [1.5 * t, 0.6 * t];
    let tilt = 0.4 * t; // the top leans right
    [
        [base[0][0] + slide[0] + tilt, base[0][1] + slide[1]],
        [base[1][0] + slide[0] + tilt - 0.5 * squeeze, base[1][1] + slide[1] + squeeze],
        [base[2][0] + slide[0] - 0.5 * squeeze, base[2][1] + slide[1] - squeeze],
        [base[3][0] + slide[0], base[3][1] + slide[1]],
    ]
}

#[test]
fn follows_four_corners_under_perspective_within_a_pixel() {
    let grid = texture_grid();
    let frames_count = 16;
    let mut tracker = PlanarTracker::new(render(&grid, &corners_at(0)), corners_at(0), TrackerOptions::default());
    let frames = (1..frames_count).map(|k| Ok::<_, ()>(render(&grid, &corners_at(k))));
    let (steps, end) = track_frames(&mut tracker, frames, &AtomicBool::new(false), |_, _| {}).unwrap();
    assert_eq!(end, RunEnd::Finished);
    for (index, step) in steps.iter().enumerate() {
        let truth = corners_at(index + 1);
        let quad = step.quad.expect("a planar step has corners");
        for (found, wanted) in quad.iter().zip(&truth) {
            let error = (found[0] - wanted[0]).hypot(found[1] - wanted[1]);
            assert!(error < 1.0, "frame {}: corner {found:?} off {wanted:?} by {error:.3} px", index + 1);
        }
        assert!(!step.lost && step.confidence > 0.5, "frame {}: {step:?}", index + 1);
    }
}

#[test]
fn stops_lost_when_the_plane_is_gone() {
    let grid = texture_grid();
    let start = corners_at(0);
    let mut tracker = PlanarTracker::new(render(&grid, &start), start, TrackerOptions::default());
    let empty = GrayImage::new(WIDTH, HEIGHT, (0..WIDTH * HEIGHT).map(|i| background((i % WIDTH) as f64, (i / WIDTH) as f64) as f32).collect());
    let (steps, end) = track_frames(&mut tracker, [Ok::<_, ()>(empty)], &AtomicBool::new(false), |_, _| {}).unwrap();
    assert_eq!(end, RunEnd::Lost);
    assert!(steps[0].lost && steps[0].confidence < 0.5);
    assert_eq!(steps[0].quad, Some(start), "a lost step leaves the corners where they were");
}

#[test]
fn finds_the_plane_again_after_a_jump() {
    let grid = texture_grid();
    let start = corners_at(0);
    let mut tracker = PlanarTracker::new(render(&grid, &start), start, TrackerOptions::default());
    // A jump far beyond what the flow follows, the shape kept.
    let jumped = start.map(|[x, y]| [x + 40.0, y + 25.0]);
    let step = tracker.step(render(&grid, &jumped));
    assert!(!step.lost, "{step:?}");
    let quad = step.quad.unwrap();
    for (found, wanted) in quad.iter().zip(&jumped) {
        assert!((found[0] - wanted[0]).hypot(found[1] - wanted[1]) < 1.5, "{quad:?} vs {jumped:?}");
    }
}

#[test]
fn a_planar_step_becomes_a_sample_with_its_quad_and_box() {
    let quad: Quad = [[9.5, 19.5], [109.5, 19.5], [109.5, 69.5], [9.5, 69.5]];
    let step = engine::tracker::TrackStep { area: engine::tracker::planar::quad_box(&quad), confidence: 0.9, lost: false, recovered: false, quad: Some(quad) };
    let sample = step.to_sample(200, 100, engine::Time::from_ticks(5));
    let parts = sample.quad.unwrap();
    assert!((parts[0][0] - 0.05).abs() < 1e-9 && (parts[0][1] - 0.2).abs() < 1e-9, "{parts:?}");
    assert!((sample.cx - 0.3).abs() < 1e-9 && (sample.cy - 0.45).abs() < 1e-9);
    assert!((sample.w - 0.5).abs() < 1e-9 && (sample.h - 0.5).abs() < 1e-9 && sample.angle.abs() < 1e-9);
    for (back, original) in engine::tracker::quad_pixels(&parts, 200, 100).iter().zip(&quad) {
        assert!((back[0] - original[0]).abs() < 1e-9 && (back[1] - original[1]).abs() < 1e-9);
    }
}
