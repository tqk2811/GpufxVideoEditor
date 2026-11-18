//! Corners worth tracking: the Shi-Tomasi measure (Shi and Tomasi, "Good features to track",
//! 1994), the smaller eigenvalue of the gradient structure tensor over a small window.

use super::image::GrayImage;

/// Half the side of the window the structure tensor sums over.
const TENSOR_RADIUS: isize = 2;

/// The strongest corners of `image` for which `inside` holds, at most `max_corners`, none weaker
/// than `quality` times the strongest, and no two closer than `min_distance` pixels. Strongest
/// first. `bounds` (x0, y0, x1, y1) limits the pixels looked at.
pub fn good_features(image: &GrayImage, bounds: (isize, isize, isize, isize), inside: impl Fn(f32, f32) -> bool, max_corners: usize, quality: f32, min_distance: f32) -> Vec<[f32; 2]> {
    let margin = TENSOR_RADIUS + 1;
    let x0 = bounds.0.max(margin);
    let y0 = bounds.1.max(margin);
    let x1 = bounds.2.min(image.width as isize - 1 - margin);
    let y1 = bounds.3.min(image.height as isize - 1 - margin);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    // Gradients over the bounds and the tensor margin.
    let (gx0, gy0) = (x0 - TENSOR_RADIUS, y0 - TENSOR_RADIUS);
    let (gw, gh) = ((x1 - x0 + 1 + 2 * TENSOR_RADIUS) as usize, (y1 - y0 + 1 + 2 * TENSOR_RADIUS) as usize);
    let mut products = vec![[0.0f32; 3]; gw * gh];
    for j in 0..gh {
        for i in 0..gw {
            let (x, y) = (gx0 + i as isize, gy0 + j as isize);
            let dx = (image.at(x + 1, y) - image.at(x - 1, y)) * 0.5;
            let dy = (image.at(x, y + 1) - image.at(x, y - 1)) * 0.5;
            products[j * gw + i] = [dx * dx, dx * dy, dy * dy];
        }
    }
    let mut scored = Vec::new();
    let mut strongest = 0.0f32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !inside(x as f32, y as f32) {
                continue;
            }
            let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
            for dy in -TENSOR_RADIUS..=TENSOR_RADIUS {
                for dx in -TENSOR_RADIUS..=TENSOR_RADIUS {
                    let [pa, pb, pc] = products[(y - gy0 + dy) as usize * gw + (x - gx0 + dx) as usize];
                    a += pa;
                    b += pb;
                    c += pc;
                }
            }
            let half_trace = (a + c) * 0.5;
            let lambda = half_trace - (((a - c) * 0.5).powi(2) + b * b).sqrt();
            if lambda > 0.0 {
                strongest = strongest.max(lambda);
                scored.push((lambda, x, y));
            }
        }
    }
    let floor = strongest * quality;
    scored.retain(|(lambda, _, _)| *lambda >= floor && *lambda > 1.0e-9);
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut chosen: Vec<[f32; 2]> = Vec::new();
    let min_squared = min_distance * min_distance;
    for (_, x, y) in scored {
        let point = [x as f32, y as f32];
        if chosen.iter().all(|other| (other[0] - point[0]).powi(2) + (other[1] - point[1]).powi(2) >= min_squared) {
            chosen.push(point);
            if chosen.len() >= max_corners {
                break;
            }
        }
    }
    chosen
}
