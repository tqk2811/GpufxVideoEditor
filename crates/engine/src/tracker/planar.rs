//! The planar (four corner, perspective) tracker: follows a flat quadrilateral, such as a screen,
//! a sign or a page, through a run of grayscale frames.
//!
//! Each step takes the strongest corners inside the quad on the previous frame, follows them into
//! the next frame with pyramidal Lucas-Kanade flow (kept only when they come back when followed
//! backwards), and fits one homography to them with RANSAC ([`super::homography`]). The quad's
//! corners move by it. How sure the step is comes from how many corners agreed and how well the
//! quad, warped back to a square, still looks like the object. Below
//! [`TrackerOptions::lost_threshold`] the tracker searches for that look around where the last
//! move would bring the quad, moved and scaled (its shape kept); if that fails too the step is
//! lost (Q7: the run stops and the sample is marked for a fix by hand).

use super::homography::{self, Homography};
use super::image::Pyramid;
use super::template::{ncc, PATCH};
use super::{features, flow, GrayImage, TrackBox, TrackStep, TrackerOptions, SEARCH_SCALES};

/// Four corners in pixels, in the order top-left, top-right, bottom-right, bottom-left of the
/// object.
pub type Quad = [[f64; 2]; 4];

/// The turned box that stands for `quad`: its middle is the mean of the corners, its width and
/// height the mean lengths of the opposite sides, its angle that of the top side.
pub fn quad_box(quad: &Quad) -> TrackBox {
    let cx = quad.iter().map(|p| p[0]).sum::<f64>() / 4.0;
    let cy = quad.iter().map(|p| p[1]).sum::<f64>() / 4.0;
    let length = |a: [f64; 2], b: [f64; 2]| (b[0] - a[0]).hypot(b[1] - a[1]);
    let w = (length(quad[0], quad[1]) + length(quad[3], quad[2])) / 2.0;
    let h = (length(quad[0], quad[3]) + length(quad[1], quad[2])) / 2.0;
    let angle = (quad[1][1] - quad[0][1]).atan2(quad[1][0] - quad[0][0]).to_degrees();
    TrackBox { cx, cy, w, h, angle }
}

/// The corners of `area` in the order of a [`Quad`].
pub fn box_quad(area: &TrackBox) -> Quad {
    let (sine, cosine) = area.angle.to_radians().sin_cos();
    [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]].map(|[u, v]| {
        let (u, v) = (u * area.w, v * area.h);
        [area.cx + cosine * u - sine * v, area.cy + sine * u + cosine * v]
    })
}

const UNIT_SQUARE: Quad = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// The quad of `pyramid` warped to a [`PATCH`] x [`PATCH`] square, from the level whose pixels are
/// about the size of a sample; `None` when the quad cannot be warped.
pub fn warped_patch(pyramid: &Pyramid, quad: &Quad) -> Option<Vec<f32>> {
    let map = Homography::from_quads(&UNIT_SQUARE, quad)?;
    let area = quad_box(quad);
    let longest = area.w.max(area.h).max(1.0);
    let wanted = (longest / PATCH as f64).max(1.0).log2().floor() as usize;
    let level = wanted.min(pyramid.levels.len() - 1);
    let image = &pyramid.levels[level];
    let factor = (1u32 << level) as f64;
    let mut patch = Vec::with_capacity(PATCH * PATCH);
    for j in 0..PATCH {
        let v = (j as f64 + 0.5) / PATCH as f64;
        for i in 0..PATCH {
            let u = (i as f64 + 0.5) / PATCH as f64;
            let [x, y] = map.apply([u, v])?;
            patch.push(image.sample((x / factor) as f32, (y / factor) as f32));
        }
    }
    Some(patch)
}

/// `quad` moved by (`dx`, `dy`) and scaled `scale` times around its middle.
fn moved(quad: &Quad, dx: f64, dy: f64, scale: f64) -> Quad {
    let area = quad_box(quad);
    quad.map(|[x, y]| [area.cx + (x - area.cx) * scale + dx, area.cy + (y - area.cy) * scale + dy])
}

/// Follows one flat quadrilateral from frame to frame.
pub struct PlanarTracker {
    options: TrackerOptions,
    previous: Pyramid,
    quad: Quad,
    template: Vec<f32>,
    /// The map of the last step, the guess for the next.
    motion: Homography,
}

impl PlanarTracker {
    /// Starts at `quad` of `first`. Only [`TrackerOptions::rotation`] is ignored: a plane always
    /// turns with its corners.
    pub fn new(first: GrayImage, quad: Quad, options: TrackerOptions) -> Self {
        let previous = Pyramid::new(first, options.levels);
        let template = warped_patch(&previous, &quad).unwrap_or_else(|| vec![0.0; PATCH * PATCH]);
        Self { options, previous, quad, template, motion: Homography::IDENTITY }
    }

    /// Where the quad is now.
    pub fn quad(&self) -> Quad {
        self.quad
    }

    /// Whether every corner of `quad` (pixels of the frame) lies within
    /// [`crate::tracking::MAX_QUAD_COORDINATE`] parts of the picture, as a sample must.
    fn within_reach(&self, quad: &Quad) -> bool {
        let base = self.previous.base();
        let (width, height) = (base.width.max(1) as f64, base.height.max(1) as f64);
        // A hair inside the bound, so the corners still pass once turned into parts.
        let limit = crate::tracking::MAX_QUAD_COORDINATE * (1.0 - 1.0e-9);
        quad.iter().all(|[x, y]| ((x + 0.5) / width).abs() <= limit && ((y + 0.5) / height).abs() <= limit)
    }

    fn step_of(&self, quad: Quad, confidence: f64, lost: bool, recovered: bool) -> TrackStep {
        TrackStep { area: quad_box(&quad), confidence, lost, recovered, quad: Some(quad) }
    }

    /// Follows the quad into `frame`. A lost step leaves the tracker where it was. A quad with a
    /// corner farther than a sample may hold ([`crate::tracking::MAX_QUAD_COORDINATE`]) is lost too.
    pub fn step(&mut self, frame: GrayImage) -> TrackStep {
        let next = Pyramid::new(frame, self.options.levels);
        let mut best_unsure = 0.0f64;
        if let Some((quad, motion, agreeing)) = self.follow_flow(&next).filter(|(quad, _, _)| self.within_reach(quad)) {
            if let Some(patch) = warped_patch(&next, &quad) {
                let confidence = ncc(&self.template, &patch).max(0.0) * agreeing.sqrt();
                if confidence >= self.options.lost_threshold {
                    self.accept(next, quad, motion, &patch, confidence);
                    return self.step_of(quad, confidence, false, false);
                }
                best_unsure = confidence;
            }
        }
        // The search starts where the last step's map would bring the quad.
        let mapped = self.quad.map(|p| self.motion.apply(p));
        let predicted = match mapped {
            [Some(a), Some(b), Some(c), Some(d)] if homography::is_convex(&[a, b, c, d]) => [a, b, c, d],
            _ => self.quad,
        };
        let (found, score) = self.search(&next, &predicted);
        if score >= self.options.recover_threshold && self.within_reach(&found) {
            if let Some(patch) = warped_patch(&next, &found) {
                self.accept(next, found, Homography::IDENTITY, &patch, score);
                return self.step_of(found, score, false, true);
            }
        }
        let confidence = best_unsure.max(score).clamp(0.0, (self.options.lost_threshold - 1.0e-3).max(0.0));
        self.step_of(self.quad, confidence, true, false)
    }

    fn accept(&mut self, next: Pyramid, quad: Quad, motion: Homography, patch: &[f32], confidence: f64) {
        if confidence > 0.8 {
            let rate = self.options.template_rate as f32;
            for (kept, seen) in self.template.iter_mut().zip(patch) {
                *kept += (seen - *kept) * rate;
            }
        }
        self.previous = next;
        self.quad = quad;
        self.motion = motion;
    }

    /// The quad moved by the flow of its corners into `next`, the map, and the part of the corners
    /// that agreed with it; `None` when too few could be followed or agreed, or the new quad is
    /// folded or changed size too much for one frame.
    fn follow_flow(&self, next: &Pyramid) -> Option<(Quad, Homography, f64)> {
        let options = &self.options;
        let quad = self.quad;
        let area = quad_box(&quad);
        let small = area.w.min(area.h);
        let spacing = (small / 12.0).clamp(2.0, 10.0) as f32;
        let inset = (options.flow.radius as f64).min(small / 5.0);
        let xs = quad.map(|p| p[0]);
        let ys = quad.map(|p| p[1]);
        let bounds = (xs.iter().copied().fold(f64::MAX, f64::min).floor() as isize, ys.iter().copied().fold(f64::MAX, f64::min).floor() as isize, xs.iter().copied().fold(f64::MIN, f64::max).ceil() as isize, ys.iter().copied().fold(f64::MIN, f64::max).ceil() as isize);
        // More corners than the similarity tracker: a homography has eight unknowns, not four.
        let corners = features::good_features(self.previous.base(), bounds, |x, y| homography::quad_contains(&quad, [f64::from(x), f64::from(y)], inset), options.max_corners * 2, 0.01, spacing);
        let needed = options.min_points.max(8);
        if corners.len() < needed {
            return None;
        }
        let mut pairs = Vec::with_capacity(corners.len());
        for corner in &corners {
            let start = [f64::from(corner[0]), f64::from(corner[1])];
            let guessed = self.motion.apply(start).unwrap_or(start);
            let guess = [guessed[0] as f32, guessed[1] as f32];
            let Some(found) = flow::track_point(&self.previous, next, *corner, guess, &options.flow) else { continue };
            let Some(back) = flow::track_point(next, &self.previous, found, *corner, &options.flow) else { continue };
            if (back[0] - corner[0]).hypot(back[1] - corner[1]) > options.round_trip {
                continue;
            }
            pairs.push((start, [f64::from(found[0]), f64::from(found[1])]));
        }
        if pairs.len() < needed {
            return None;
        }
        let (model, mask) = homography::ransac(&pairs, options.ransac_threshold, options.ransac_iterations)?;
        let agreeing = mask.iter().filter(|inside| **inside).count();
        if agreeing < needed {
            return None;
        }
        let mut moved = [[0.0; 2]; 4];
        for (to, from) in moved.iter_mut().zip(&quad) {
            *to = model.apply(*from)?;
        }
        let growth = homography::quad_area(&moved) / homography::quad_area(&quad).max(1.0e-9);
        if !homography::is_convex(&moved) || !(0.25..=4.0).contains(&growth) {
            return None;
        }
        Some((moved, model, agreeing as f64 / corners.len() as f64))
    }

    /// The quad like `around` (its shape kept), moved within [`TrackerOptions::search_reach`] of its
    /// size and scaled by one of the search scales, whose warped patch best matches the remembered
    /// look, and that score. A coarse grid first, then finer steps around the best place.
    fn search(&self, pyramid: &Pyramid, around: &Quad) -> (Quad, f64) {
        let score = |quad: &Quad| warped_patch(pyramid, quad).map_or(-1.0, |patch| ncc(&self.template, &patch));
        let mut best = (*around, score(around));
        let base = pyramid.base();
        let area = quad_box(around);
        let longest = area.w.max(area.h).max(4.0);
        for &scale in &SEARCH_SCALES {
            let span = self.options.search_reach * longest;
            let step = (longest * scale / 8.0).max(1.0);
            let count = (span / step).ceil() as i64;
            for j in -count..=count {
                for i in -count..=count {
                    let (dx, dy) = (i as f64 * step, j as f64 * step);
                    let (cx, cy) = (area.cx + dx, area.cy + dy);
                    if cx < 0.0 || cy < 0.0 || cx > base.width as f64 || cy > base.height as f64 {
                        continue;
                    }
                    let candidate = moved(around, dx, dy, scale);
                    let value = score(&candidate);
                    if value > best.1 {
                        best = (candidate, value);
                    }
                }
            }
        }
        let mut step = (longest / 8.0).max(1.0);
        while step >= 0.25 {
            step *= 0.5;
            let centre = best.0;
            for j in -2..=2 {
                for i in -2..=2 {
                    if i == 0 && j == 0 {
                        continue;
                    }
                    let candidate = moved(&centre, i as f64 * step, j as f64 * step, 1.0);
                    let value = score(&candidate);
                    if value > best.1 {
                        best = (candidate, value);
                    }
                }
            }
        }
        best
    }
}

impl super::StepTracker<GrayImage> for PlanarTracker {
    fn step(&mut self, frame: GrayImage) -> TrackStep {
        PlanarTracker::step(self, frame)
    }
}
