//! The classical object tracker: follows a box through a run of grayscale frames.
//!
//! Each step takes the strongest corners inside the box on the previous frame
//! ([`features::good_features`]), follows them into the next frame with pyramidal Lucas-Kanade
//! flow, keeping only those that come back to where they started when followed backwards, and
//! fits one similarity transform (move, scale and, when asked, turn) to them with RANSAC. The box
//! moves by that transform. How sure the step is comes from how many corners agreed and how well
//! the box still looks like the object ([`template`]); below [`TrackerOptions::lost_threshold`]
//! the tracker searches for the object's look around its last place at a few sizes, and if that
//! fails too the step is lost: the caller stops there and marks it for a fix by hand.
//!
//! Written from the papers named in each module; no code of other trackers was used.

pub mod features;
pub mod homography;
pub mod flow;
pub mod image;
pub mod planar;
pub mod similarity;
pub mod template;

use std::sync::atomic::{AtomicBool, Ordering};

pub use image::{GrayImage, Pyramid};
pub use planar::{PlanarTracker, Quad};
pub use similarity::Similarity;

use crate::time::Time;
use crate::tracking::TrackSample;

/// A turned box in a picture, in pixels: its middle, its size before turning, and its turn in
/// clockwise degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackBox {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub angle: f64,
}

impl TrackBox {
    /// Whether (`x`, `y`) lies inside the box shrunk by `inset` pixels on each side.
    pub fn contains(&self, x: f64, y: f64, inset: f64) -> bool {
        let (sine, cosine) = self.angle.to_radians().sin_cos();
        let (dx, dy) = (x - self.cx, y - self.cy);
        let u = cosine * dx + sine * dy;
        let v = -sine * dx + cosine * dy;
        u.abs() <= self.w / 2.0 - inset && v.abs() <= self.h / 2.0 - inset
    }

    /// The smallest upright pixel rectangle (x0, y0, x1, y1) holding the box.
    pub fn bounds(&self) -> (isize, isize, isize, isize) {
        let (sine, cosine) = self.angle.to_radians().sin_cos();
        let half_w = (self.w * cosine.abs() + self.h * sine.abs()) / 2.0;
        let half_h = (self.w * sine.abs() + self.h * cosine.abs()) / 2.0;
        ((self.cx - half_w).floor() as isize, (self.cy - half_h).floor() as isize, (self.cx + half_w).ceil() as isize, (self.cy + half_h).ceil() as isize)
    }

    /// The box of `sample` in a picture of `width` x `height` pixels. Sample coordinates are parts
    /// of the picture with 0 and 1 at its outer edges; pixel centres are whole numbers.
    pub fn from_sample(sample: &TrackSample, width: usize, height: usize) -> Self {
        let (width, height) = (width as f64, height as f64);
        Self { cx: sample.cx * width - 0.5, cy: sample.cy * height - 0.5, w: sample.w * width, h: sample.h * height, angle: sample.angle }
    }

    /// The sample of this box at `source_time`, for a picture of `width` x `height` pixels.
    pub fn to_sample(&self, width: usize, height: usize, source_time: Time, confidence: f64) -> TrackSample {
        let (width, height) = (width as f64, height as f64);
        TrackSample {
            source_time,
            cx: (self.cx + 0.5) / width,
            cy: (self.cy + 0.5) / height,
            w: (self.w / width).max(0.0),
            h: (self.h / height).max(0.0),
            angle: (self.angle + 180.0).rem_euclid(360.0) - 180.0,
            confidence: confidence.clamp(0.0, 1.0),
            manual: false,
            quad: None,
        }
    }
}

/// The corners of `quad` (parts of a picture of `width` x `height` pixels) in pixels, pixel
/// centres at whole numbers as in [`TrackBox::from_sample`].
pub fn quad_pixels(quad: &Quad, width: usize, height: usize) -> Quad {
    let (width, height) = (width as f64, height as f64);
    quad.map(|[x, y]| [x * width - 0.5, y * height - 0.5])
}

impl TrackStep {
    /// The sample of this step at `source_time`, for a picture of `width` x `height` pixels: with
    /// its corners for a planar step.
    pub fn to_sample(&self, width: usize, height: usize, source_time: Time) -> TrackSample {
        match self.quad {
            Some(quad) => {
                let (w, h) = (width as f64, height as f64);
                let parts = quad.map(|[x, y]| [(x + 0.5) / w, (y + 0.5) / h]);
                TrackSample::from_quad(source_time, parts, w / h.max(1.0), self.confidence.clamp(0.0, 1.0), false)
            }
            None => self.area.to_sample(width, height, source_time, self.confidence),
        }
    }
}

/// How the tracker works. The defaults suit frames a few hundred pixels high.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerOptions {
    /// Follow the turn of the object; without it the box keeps its angle.
    pub rotation: bool,
    /// Pyramid levels, the picture itself included.
    pub levels: usize,
    /// The most corners followed in one step.
    pub max_corners: usize,
    /// Fewer agreeing corners than this and the flow is not trusted.
    pub min_points: usize,
    pub flow: flow::FlowOptions,
    /// A corner more than this many pixels off the fitted transform does not agree with it.
    pub ransac_threshold: f64,
    pub ransac_iterations: usize,
    /// A corner followed forwards then backwards that lands farther than this from its start
    /// (pixels) is dropped.
    pub round_trip: f32,
    /// A step less sure than this is lost unless the search finds the object again.
    pub lost_threshold: f64,
    /// How well the search must match the object's look to take its answer.
    pub recover_threshold: f64,
    /// How far around the last place the search looks, in box sizes.
    pub search_reach: f64,
    /// How much of each sure step's look goes into the remembered look, 0 to 1.
    pub template_rate: f64,
}

impl Default for TrackerOptions {
    fn default() -> Self {
        Self {
            rotation: true,
            levels: 4,
            max_corners: 100,
            min_points: 5,
            flow: flow::FlowOptions::default(),
            ransac_threshold: 1.0,
            ransac_iterations: 200,
            round_trip: 1.0,
            lost_threshold: LOST_CONFIDENCE,
            recover_threshold: 0.75,
            search_reach: 1.5,
            template_rate: 0.1,
        }
    }
}

/// The default confidence under which a step counts as lost. A sample less sure than this is
/// shown as one to fix by hand.
pub const LOST_CONFIDENCE: f64 = 0.5;

/// The relative sizes the search tries.
pub(crate) const SEARCH_SCALES: [f64; 5] = [0.85, 0.93, 1.0, 1.08, 1.18];

/// What one step found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackStep {
    /// Where the box is; for a lost step, where it was last.
    pub area: TrackBox,
    /// How sure the step is, 0 to 1. Below the lost threshold for a lost step.
    pub confidence: f64,
    pub lost: bool,
    /// Found by the search rather than by the flow.
    pub recovered: bool,
    /// The four corners in pixels (top-left, top-right, bottom-right, bottom-left), for a planar
    /// step ([`PlanarTracker`]); `area` then stands for them.
    pub quad: Option<Quad>,
}

/// Follows one box from frame to frame.
pub struct Tracker {
    options: TrackerOptions,
    previous: Pyramid,
    area: TrackBox,
    template: Vec<f32>,
    /// The transform of the last step, the guess for the next.
    motion: Similarity,
}

impl Tracker {
    /// Starts at `area` of `first`.
    pub fn new(first: GrayImage, area: TrackBox, options: TrackerOptions) -> Self {
        let previous = Pyramid::new(first, options.levels);
        let template = template::sample_patch(&previous, &area);
        Self { options, previous, area, template, motion: Similarity::IDENTITY }
    }

    /// Where the box is now.
    pub fn area(&self) -> TrackBox {
        self.area
    }

    /// Follows the box into `frame`. A lost step leaves the tracker where it was.
    pub fn step(&mut self, frame: GrayImage) -> TrackStep {
        let next = Pyramid::new(frame, self.options.levels);
        let mut best_unsure = 0.0f64;
        if let Some((area, motion, agreeing)) = self.follow_flow(&next) {
            let patch = template::sample_patch(&next, &area);
            let confidence = template::ncc(&self.template, &patch).max(0.0) * agreeing.sqrt();
            if confidence >= self.options.lost_threshold {
                self.accept(next, area, motion, &patch, confidence);
                return TrackStep { area, confidence, lost: false, recovered: false, quad: None };
            }
            best_unsure = confidence;
        }
        // The search starts where the last step's move would bring the box.
        let [cx, cy] = self.motion.apply([self.area.cx, self.area.cy]);
        let predicted = TrackBox { cx, cy, ..self.area };
        let (found, score) = template::search(&next, &self.template, &predicted, &SEARCH_SCALES, self.options.search_reach);
        if score >= self.options.recover_threshold {
            let patch = template::sample_patch(&next, &found);
            self.accept(next, found, Similarity::IDENTITY, &patch, score);
            return TrackStep { area: found, confidence: score, lost: false, recovered: true, quad: None };
        }
        let confidence = best_unsure.max(score).clamp(0.0, (self.options.lost_threshold - 1.0e-3).max(0.0));
        TrackStep { area: self.area, confidence, lost: true, recovered: false, quad: None }
    }

    fn accept(&mut self, next: Pyramid, area: TrackBox, motion: Similarity, patch: &[f32], confidence: f64) {
        if confidence > 0.8 {
            let rate = self.options.template_rate as f32;
            for (kept, seen) in self.template.iter_mut().zip(patch) {
                *kept += (seen - *kept) * rate;
            }
        }
        self.previous = next;
        self.area = area;
        self.motion = motion;
    }

    /// The box moved by the flow of its corners into `next`, the transform, and the part of the
    /// corners that agreed with it; `None` when too few could be followed or agreed.
    fn follow_flow(&self, next: &Pyramid) -> Option<(TrackBox, Similarity, f64)> {
        let options = &self.options;
        let area = self.area;
        let spacing = (area.w.min(area.h) / 12.0).clamp(2.0, 10.0) as f32;
        // Corners whose window reaches past the box see the background, which does not move
        // with the object: keep them a window's half side inside, unless the box is small.
        let inset = (options.flow.radius as f64).min(area.w.min(area.h) / 5.0);
        let corners = features::good_features(self.previous.base(), area.bounds(), |x, y| area.contains(f64::from(x), f64::from(y), inset), options.max_corners, 0.01, spacing);
        if corners.len() < options.min_points {
            return None;
        }
        let mut pairs = Vec::with_capacity(corners.len());
        for corner in &corners {
            let start = [f64::from(corner[0]), f64::from(corner[1])];
            let guessed = self.motion.apply(start);
            let guess = [guessed[0] as f32, guessed[1] as f32];
            let Some(found) = flow::track_point(&self.previous, next, *corner, guess, &options.flow) else { continue };
            let Some(back) = flow::track_point(next, &self.previous, found, *corner, &options.flow) else { continue };
            if (back[0] - corner[0]).hypot(back[1] - corner[1]) > options.round_trip {
                continue;
            }
            pairs.push((start, [f64::from(found[0]), f64::from(found[1])]));
        }
        if pairs.len() < options.min_points {
            return None;
        }
        let (model, mask) = similarity::ransac(&pairs, options.rotation, options.ransac_threshold, options.ransac_iterations)?;
        let agreeing = mask.iter().filter(|inside| **inside).count();
        if agreeing < options.min_points {
            return None;
        }
        let scale = model.scale();
        if !(scale.is_finite() && (0.5..=2.0).contains(&scale)) {
            return None;
        }
        let [cx, cy] = model.apply([area.cx, area.cy]);
        let angle = if options.rotation { area.angle + model.angle() } else { area.angle };
        let moved = TrackBox { cx, cy, w: area.w * scale, h: area.h * scale, angle };
        Some((moved, model, agreeing as f64 / corners.len() as f64))
    }
}

/// How a run of steps ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// The frames ran out.
    Finished,
    /// The last step was lost.
    Lost,
    /// `cancel` was set.
    Cancelled,
}

/// Anything that follows a box from frame to frame: the classical [`Tracker`] on gray frames, or
/// another tracker on frames of its own kind.
pub trait StepTracker<F> {
    /// Follows the box into `frame`. A lost step leaves the tracker where it was.
    fn step(&mut self, frame: F) -> TrackStep;
}

impl StepTracker<GrayImage> for Tracker {
    fn step(&mut self, frame: GrayImage) -> TrackStep {
        Tracker::step(self, frame)
    }
}

/// Steps `tracker` through `frames` until they run out, a step is lost (it is the last one
/// given) or `cancel` is set (checked before each frame). `on_step` sees each step with its
/// index. A frame that cannot be read ends the run with its error, unless `cancel` is set by then.
pub fn track_frames<F, E, T: StepTracker<F> + ?Sized>(tracker: &mut T, frames: impl IntoIterator<Item = Result<F, E>>, cancel: &AtomicBool, mut on_step: impl FnMut(usize, &TrackStep)) -> Result<(Vec<TrackStep>, RunEnd), E> {
    let mut steps = Vec::new();
    let mut frames = frames.into_iter();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok((steps, RunEnd::Cancelled));
        }
        let Some(frame) = frames.next() else { return Ok((steps, RunEnd::Finished)) };
        let frame = match frame {
            Ok(frame) => frame,
            // A read cut short by the cancel is the cancel, not a failure: the steps made stay.
            Err(_) if cancel.load(Ordering::Relaxed) => return Ok((steps, RunEnd::Cancelled)),
            Err(error) => return Err(error),
        };
        let step = tracker.step(frame);
        on_step(steps.len(), &step);
        steps.push(step);
        if step.lost {
            return Ok((steps, RunEnd::Lost));
        }
    }
}
