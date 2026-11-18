//! The look of the tracked box as a small patch, compared by normalised cross-correlation (NCC):
//! how sure the tracker is, and where the object went when the flow lost it.

use super::image::Pyramid;
use super::TrackBox;

/// The side of a patch, in samples.
pub const PATCH: usize = 24;

/// The box of `pyramid` sampled into a [`PATCH`] x [`PATCH`] patch in its own frame (turned with
/// it), from the level whose pixels are about the size of a sample, so a large box does not alias.
pub fn sample_patch(pyramid: &Pyramid, area: &TrackBox) -> Vec<f32> {
    let longest = area.w.max(area.h).max(1.0);
    let wanted = (longest / PATCH as f64).max(1.0).log2().floor() as usize;
    let level = wanted.min(pyramid.levels.len() - 1);
    let image = &pyramid.levels[level];
    let factor = (1u32 << level) as f64;
    let (sine, cosine) = area.angle.to_radians().sin_cos();
    let mut patch = Vec::with_capacity(PATCH * PATCH);
    for j in 0..PATCH {
        let v = ((j as f64 + 0.5) / PATCH as f64 - 0.5) * area.h;
        for i in 0..PATCH {
            let u = ((i as f64 + 0.5) / PATCH as f64 - 0.5) * area.w;
            let x = area.cx + cosine * u - sine * v;
            let y = area.cy + sine * u + cosine * v;
            // Pixel x of a level is pixel x * 2^level of level 0 (see [`super::GrayImage::half`]).
            patch.push(image.sample((x / factor) as f32, (y / factor) as f32));
        }
    }
    patch
}

/// The normalised cross-correlation of two patches of the same size, -1 to 1; 0 when either is flat.
pub fn ncc(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len()) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mean_a = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let mean_b = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x) - mean_a, f64::from(*y) - mean_b);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    let norm = (aa * bb).sqrt();
    if norm < 1.0e-12 { 0.0 } else { (ab / norm).clamp(-1.0, 1.0) }
}

/// The box like `around` (its angle kept) at one of `scales` of its size and within `reach` box
/// sizes of its middle whose patch best matches `template`, and that score. A coarse grid first,
/// then finer steps around the best place.
pub fn search(pyramid: &Pyramid, template: &[f32], around: &TrackBox, scales: &[f64], reach: f64) -> (TrackBox, f64) {
    let mut best = (*around, ncc(template, &sample_patch(pyramid, around)));
    let base = pyramid.base();
    let longest = around.w.max(around.h).max(4.0);
    for &scale in scales {
        let size = TrackBox { w: around.w * scale, h: around.h * scale, ..*around };
        let span = reach * longest;
        let step = (longest * scale / 8.0).max(1.0);
        let count = (span / step).ceil() as i64;
        for j in -count..=count {
            for i in -count..=count {
                let candidate = TrackBox { cx: around.cx + i as f64 * step, cy: around.cy + j as f64 * step, ..size };
                if candidate.cx < 0.0 || candidate.cy < 0.0 || candidate.cx > base.width as f64 || candidate.cy > base.height as f64 {
                    continue;
                }
                let score = ncc(template, &sample_patch(pyramid, &candidate));
                if score > best.1 {
                    best = (candidate, score);
                }
            }
        }
    }
    // Finer: halve the step around the best place down to a quarter pixel.
    let mut step = (best.0.w.max(best.0.h) / 8.0).max(1.0);
    while step >= 0.25 {
        step *= 0.5;
        let centre = best.0;
        for j in -2..=2 {
            for i in -2..=2 {
                if i == 0 && j == 0 {
                    continue;
                }
                let candidate = TrackBox { cx: centre.cx + i as f64 * step, cy: centre.cy + j as f64 * step, ..centre };
                let score = ncc(template, &sample_patch(pyramid, &candidate));
                if score > best.1 {
                    best = (candidate, score);
                }
            }
        }
    }
    best
}
