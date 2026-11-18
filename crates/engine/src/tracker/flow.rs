//! Pyramidal Lucas-Kanade optical flow of single points (Lucas and Kanade 1981; the coarse to
//! fine scheme of Bouguet, "Pyramidal implementation of the Lucas Kanade feature tracker", 2000).

use super::image::Pyramid;

/// How a point is followed from one picture to the next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowOptions {
    /// Half the side of the window compared around the point, in pixels of each level.
    pub radius: isize,
    /// The most Gauss-Newton steps on each level.
    pub iterations: usize,
    /// A step shorter than this (pixels) ends the steps of a level.
    pub epsilon: f32,
    /// The smaller eigenvalue of the window's gradient matrix, per pixel of the window, below which
    /// the point has too little texture to follow.
    pub min_eigen: f32,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self { radius: 7, iterations: 30, epsilon: 0.01, min_eigen: 1.0e-5 }
    }
}

/// Where `point` of the first pyramid's picture is in the second's, starting the search at `guess`
/// (pixels of level 0). `None` when the point has too little texture or the search goes off the
/// picture.
pub fn track_point(from: &Pyramid, to: &Pyramid, point: [f32; 2], guess: [f32; 2], options: &FlowOptions) -> Option<[f32; 2]> {
    let levels = from.levels.len().min(to.levels.len());
    let top = levels - 1;
    let scale = (1u32 << top) as f32;
    // The displacement on the current level, starting from the guess.
    let mut g = [(guess[0] - point[0]) / scale, (guess[1] - point[1]) / scale];
    let side = (2 * options.radius + 1) as usize;
    let mut template = vec![0.0f32; side * side];
    let mut gradients = vec![[0.0f32; 2]; side * side];
    for level in (0..levels).rev() {
        let (image, next) = (&from.levels[level], &to.levels[level]);
        let factor = (1u32 << level) as f32;
        let p = [point[0] / factor, point[1] / factor];
        // The window of the first picture and its gradients, and their matrix.
        let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
        let mut k = 0;
        for dy in -options.radius..=options.radius {
            for dx in -options.radius..=options.radius {
                let (x, y) = (p[0] + dx as f32, p[1] + dy as f32);
                template[k] = image.sample(x, y);
                let ix = (image.sample(x + 1.0, y) - image.sample(x - 1.0, y)) * 0.5;
                let iy = (image.sample(x, y + 1.0) - image.sample(x, y - 1.0)) * 0.5;
                gradients[k] = [ix, iy];
                gxx += ix * ix;
                gxy += ix * iy;
                gyy += iy * iy;
                k += 1;
            }
        }
        let determinant = gxx * gyy - gxy * gxy;
        let min_eigen = ((gxx + gyy) - ((gxx - gyy).powi(2) + 4.0 * gxy * gxy).sqrt()) * 0.5 / (side * side) as f32;
        let weak = min_eigen < options.min_eigen || determinant.abs() < 1.0e-12;
        // Only the full picture decides whether the point can be followed: a fine texture fades
        // out of the smaller levels, which then just pass the guess on.
        if weak && level == 0 {
            return None;
        }
        let mut v = [0.0f32; 2];
        for _ in 0..if weak { 0 } else { options.iterations } {
            let (mut bx, mut by) = (0.0f32, 0.0f32);
            let mut k = 0;
            for dy in -options.radius..=options.radius {
                for dx in -options.radius..=options.radius {
                    let (x, y) = (p[0] + g[0] + v[0] + dx as f32, p[1] + g[1] + v[1] + dy as f32);
                    let difference = template[k] - next.sample(x, y);
                    bx += difference * gradients[k][0];
                    by += difference * gradients[k][1];
                    k += 1;
                }
            }
            let step = [(gyy * bx - gxy * by) / determinant, (gxx * by - gxy * bx) / determinant];
            v[0] += step[0];
            v[1] += step[1];
            if !(v[0].is_finite() && v[1].is_finite()) {
                return None;
            }
            if step[0].abs() < options.epsilon && step[1].abs() < options.epsilon {
                break;
            }
        }
        g = [g[0] + v[0], g[1] + v[1]];
        if level > 0 {
            g = [g[0] * 2.0, g[1] * 2.0];
        }
    }
    let found = [point[0] + g[0], point[1] + g[1]];
    let base = to.base();
    let inside = found[0] >= 0.0 && found[1] >= 0.0 && found[0] <= (base.width - 1) as f32 && found[1] <= (base.height - 1) as f32;
    inside.then_some(found)
}
