//! Geometry of the free-drawn shape of a censor region: a closed polygon whose points are in parts
//! of the region's box (0 to 1 across its width and height). It is always closed (the last point
//! joins the first) and fills by the even-odd rule, so a path that crosses itself leaves holes
//! where it covers a part twice.

/// The fewest points of a polygon shape.
pub const MIN_POLYGON_POINTS: usize = 3;
/// The most points of a polygon shape (the size of the buffer the shader reads).
pub const MAX_POLYGON_POINTS: usize = 256;

/// Why `points` cannot be the shape of a censor region, if they cannot.
pub fn polygon_problem(points: &[[f64; 2]]) -> Option<String> {
    if !(MIN_POLYGON_POINTS..=MAX_POLYGON_POINTS).contains(&points.len()) {
        return Some(format!("a polygon of {} points, not from {MIN_POLYGON_POINTS} to {MAX_POLYGON_POINTS}", points.len()));
    }
    if let Some(point) = points.iter().find(|p| !p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))) {
        return Some(format!("polygon point {point:?} not inside the box (0 to 1)"));
    }
    if spread(points) <= MIN_POLYGON_SPREAD {
        return Some("a polygon whose points lie on one line covers nothing".to_owned());
    }
    None
}

/// How far, in parts of the box, a polygon's points must stand off the line through two of them:
/// closer, they lie on one line (or on one point) and the shape covers nothing.
pub const MIN_POLYGON_SPREAD: f64 = 1e-6;

/// How far the point of `points` farthest off the line through the first point and the point
/// farthest from it stands; 0 when all are on one line or one point. The shoelace area would not
/// do: a bowtie's two halves cancel in it, though the even-odd rule fills both.
fn spread(points: &[[f64; 2]]) -> f64 {
    let Some(&first) = points.first() else { return 0.0 };
    let length = |p: &[f64; 2]| (p[0] - first[0]).hypot(p[1] - first[1]);
    let Some(far) = points.iter().max_by(|a, b| length(a).total_cmp(&length(b))) else { return 0.0 };
    let span = length(far);
    if !(span > MIN_POLYGON_SPREAD) {
        return 0.0;
    }
    let (dx, dy) = ((far[0] - first[0]) / span, (far[1] - first[1]) / span);
    points.iter().map(|p| ((p[0] - first[0]) * dy - (p[1] - first[1]) * dx).abs()).fold(0.0, f64::max)
}

/// About the largest distance from a point inside the closed polygon `points` to its edge, with the
/// box `size` units wide and high (so the distance is in those units): the best of a grid of
/// samples refined around the best one, so a little under the true one. A soft edge wider than it
/// never reaches full strength.
pub fn inner_radius(points: &[[f64; 2]], size: (f64, f64)) -> f64 {
    const SAMPLES: usize = 24;
    const REFINE: usize = 8;
    let scaled: Vec<[f64; 2]> = points.iter().map(|p| [p[0] * size.0, p[1] * size.1]).collect();
    let mut best = (0.0_f64, [size.0 / 2.0, size.1 / 2.0]);
    // A grid over the box, then finer grids around the best sample so far.
    let (mut middle, mut cell, mut count) = ([size.0 / 2.0, size.1 / 2.0], [size.0 / SAMPLES as f64, size.1 / SAMPLES as f64], SAMPLES);
    for _ in 0..4 {
        for row in 0..count {
            for column in 0..count {
                let offset = |i: usize, step: f64| (i as f64 + 0.5 - count as f64 / 2.0) * step;
                let p = [middle[0] + offset(column, cell[0]), middle[1] + offset(row, cell[1])];
                let distance = signed_distance(&scaled, p);
                if distance > best.0 {
                    best = (distance, p);
                }
            }
        }
        middle = best.1;
        cell = [cell[0] * 2.0 / REFINE as f64, cell[1] * 2.0 / REFINE as f64];
        count = REFINE;
    }
    best.0
}

/// Whether `p` is inside the closed polygon `points`, by the even-odd rule.
pub fn contains(points: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    let n = points.len();
    for i in 0..n {
        let a = points[i];
        let b = points[(i + n - 1) % n];
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
    }
    inside
}

/// The distance from `p` to the segment from `a` to `b`.
pub fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length).clamp(0.0, 1.0) } else { 0.0 };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

/// The distance from `p` to the nearest edge of the closed polygon `points`, with its sign:
/// positive inside, negative outside. The soft edge of a polygon region is measured by it.
pub fn signed_distance(points: &[[f64; 2]], p: [f64; 2]) -> f64 {
    let n = points.len();
    let nearest = (0..n).map(|i| segment_distance(p, points[i], points[(i + 1) % n])).fold(f64::INFINITY, f64::min);
    if contains(points, p) { nearest } else { -nearest }
}

/// The index of the edge (from point `i` to point `i + 1`, the last one closing the shape)
/// nearest to `p`, with its distance.
pub fn nearest_edge(points: &[[f64; 2]], p: [f64; 2]) -> Option<(usize, f64)> {
    let n = points.len();
    (0..n).map(|i| (i, segment_distance(p, points[i], points[(i + 1) % n]))).min_by(|a, b| a.1.total_cmp(&b.1))
}

/// `path` with fewer points by the Douglas-Peucker method: a point stays only when it is more than
/// `tolerance` from the line between the points kept around it. The ends always stay.
pub fn simplify(path: &[[f64; 2]], tolerance: f64) -> Vec<[f64; 2]> {
    if path.len() < 3 {
        return path.to_vec();
    }
    let mut keep = vec![false; path.len()];
    keep[0] = true;
    keep[path.len() - 1] = true;
    let mut stack = vec![(0, path.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let farthest = (first + 1..last).map(|i| (i, segment_distance(path[i], path[first], path[last]))).max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, distance)) = farthest {
            if distance > tolerance {
                keep[i] = true;
                stack.push((first, i));
                stack.push((i, last));
            }
        }
    }
    path.iter().zip(keep).filter_map(|(p, kept)| kept.then_some(*p)).collect()
}

/// A free-drawn path made a polygon of at most [`MAX_POLYGON_POINTS`] points: simplified with
/// `tolerance` (raised until it fits), its closing point dropped when it lands on the first.
/// `None` when fewer than [`MIN_POLYGON_POINTS`] points are left.
pub fn closed_polygon(path: &[[f64; 2]], tolerance: f64) -> Option<Vec<[f64; 2]>> {
    let mut tolerance = tolerance.max(0.0);
    let mut points = simplify(path, tolerance);
    while points.len() > MAX_POLYGON_POINTS {
        tolerance = if tolerance > 0.0 { tolerance * 1.5 } else { 1e-6 };
        points = simplify(path, tolerance);
    }
    while points.len() > 1 && same_point(points[0], points[points.len() - 1], tolerance) {
        points.pop();
    }
    (points.len() >= MIN_POLYGON_POINTS).then_some(points)
}

fn same_point(a: [f64; 2], b: [f64; 2], tolerance: f64) -> bool {
    (a[0] - b[0]).hypot(a[1] - b[1]) <= tolerance.max(1e-12)
}

/// The bounding box of `points` as `(left, top, right, bottom)`.
pub fn bounds(points: &[[f64; 2]]) -> Option<(f64, f64, f64, f64)> {
    let first = *points.first()?;
    Some(points.iter().fold((first[0], first[1], first[0], first[1]), |(l, t, r, b), p| (l.min(p[0]), t.min(p[1]), r.max(p[0]), b.max(p[1]))))
}

/// `points` in the parts of their own bounding box (0 to 1 across it), with that box. A box
/// thinner than `min_size` on a side is widened to it around its middle.
pub fn fit_in_box(points: &[[f64; 2]], min_size: f64) -> Option<(Vec<[f64; 2]>, (f64, f64, f64, f64))> {
    let (mut left, mut top, mut right, mut bottom) = bounds(points)?;
    if right - left < min_size {
        let middle = (left + right) / 2.0;
        (left, right) = (middle - min_size / 2.0, middle + min_size / 2.0);
    }
    if bottom - top < min_size {
        let middle = (top + bottom) / 2.0;
        (top, bottom) = (middle - min_size / 2.0, middle + min_size / 2.0);
    }
    let (width, height) = ((right - left).max(1e-12), (bottom - top).max(1e-12));
    let local = points.iter().map(|p| [((p[0] - left) / width).clamp(0.0, 1.0), ((p[1] - top) / height).clamp(0.0, 1.0)]).collect();
    Some((local, (left, top, right, bottom)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: [[f64; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

    #[test]
    fn a_point_is_inside_a_square_and_outside_past_its_edge() {
        assert!(contains(&SQUARE, [0.5, 0.5]));
        assert!(!contains(&SQUARE, [1.5, 0.5]));
        assert!(!contains(&SQUARE, [0.5, -0.1]));
    }

    #[test]
    fn the_last_point_joins_the_first_so_an_open_list_still_fills() {
        // A triangle given by three points: the edge from the last back to the first counts.
        let triangle = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        assert!(contains(&triangle, [0.2, 0.2]));
        assert!(!contains(&triangle, [0.8, 0.8]));
        // Distance to the closing edge (x = 0) is measured as well.
        assert!((signed_distance(&triangle, [0.05, 0.5]) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn a_self_crossing_path_fills_by_the_even_odd_rule() {
        // A pentagram: its middle pentagon is covered twice, so it is outside.
        let star: Vec<[f64; 2]> = (0..5).map(|i| {
            let a = std::f64::consts::TAU * (i as f64 * 2.0) / 5.0 - std::f64::consts::FRAC_PI_2;
            [0.5 + 0.5 * a.cos(), 0.5 + 0.5 * a.sin()]
        }).collect();
        assert!(!contains(&star, [0.5, 0.5]));
        assert!(contains(&star, [0.5, 0.05]));
    }

    #[test]
    fn signed_distance_is_positive_inside_and_negative_outside() {
        assert!((signed_distance(&SQUARE, [0.5, 0.25]) - 0.25).abs() < 1e-9);
        assert!((signed_distance(&SQUARE, [1.5, 0.5]) + 0.5).abs() < 1e-9);
    }

    #[test]
    fn simplify_drops_points_on_a_straight_line_and_keeps_corners() {
        let path: Vec<[f64; 2]> = (0..=10).map(|i| [i as f64 / 10.0, 0.0]).chain((1..=10).map(|i| [1.0, i as f64 / 10.0])).collect();
        assert_eq!(simplify(&path, 0.01), vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
    }

    #[test]
    fn simplify_keeps_a_bump_larger_than_the_tolerance() {
        let path = [[0.0, 0.0], [0.5, 0.2], [1.0, 0.0]];
        assert_eq!(simplify(&path, 0.1).len(), 3);
        assert_eq!(simplify(&path, 0.3).len(), 2);
    }

    #[test]
    fn a_drawn_loop_closes_and_drops_its_repeated_end() {
        let mut path: Vec<[f64; 2]> = (0..200).map(|i| {
            let a = std::f64::consts::TAU * i as f64 / 200.0;
            [0.5 + 0.4 * a.cos(), 0.5 + 0.4 * a.sin()]
        }).collect();
        path.push(path[0]);
        let polygon = closed_polygon(&path, 0.005).unwrap();
        assert!(polygon.len() >= MIN_POLYGON_POINTS && polygon.len() < 200);
        assert_ne!(polygon.first(), polygon.last());
        assert!(contains(&polygon, [0.5, 0.5]));
    }

    #[test]
    fn a_long_path_is_simplified_until_it_fits_the_buffer() {
        // A circle of 2000 sharp teeth keeps every point at a small tolerance, so the tolerance
        // has to rise.
        let path: Vec<[f64; 2]> = (0..2000).map(|i| {
            let (a, r) = (std::f64::consts::TAU * i as f64 / 2000.0, if i % 2 == 0 { 0.4 } else { 0.41 });
            [0.5 + r * a.cos(), 0.5 + r * a.sin()]
        }).collect();
        let polygon = closed_polygon(&path, 0.0001).unwrap();
        assert!(polygon.len() <= MAX_POLYGON_POINTS);
        assert!(polygon_problem(&polygon).is_none());
    }

    #[test]
    fn a_line_is_no_polygon() {
        assert_eq!(closed_polygon(&[[0.0, 0.0], [0.5, 0.0], [1.0, 0.0]], 0.01), None);
    }

    #[test]
    fn polygon_problem_checks_the_count_and_the_range() {
        assert!(polygon_problem(&SQUARE).is_none());
        assert!(polygon_problem(&SQUARE[..2]).is_some());
        assert!(polygon_problem(&vec![[0.5, 0.5]; MAX_POLYGON_POINTS + 1]).is_some());
        assert!(polygon_problem(&[[0.0, 0.0], [1.2, 0.0], [0.0, 1.0]]).is_some());
        assert!(polygon_problem(&[[0.0, 0.0], [f64::NAN, 0.0], [0.0, 1.0]]).is_some());
    }

    #[test]
    fn a_polygon_on_one_line_or_one_point_is_refused_but_a_bowtie_is_kept() {
        assert!(polygon_problem(&[[0.1, 0.1], [0.5, 0.5], [0.9, 0.9]]).is_some(), "collinear");
        assert!(polygon_problem(&[[0.3, 0.3]; 4]).is_some(), "all on one point");
        assert!(polygon_problem(&[[0.0, 0.0], [0.0, 0.0], [1.0, 0.0], [1.0, 0.0]]).is_some(), "duplicates on a line");
        assert!(polygon_problem(&[[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]]).is_none(), "a bowtie has no shoelace area yet fills");
    }

    #[test]
    fn the_inner_radius_of_a_square_and_a_thin_strip() {
        let square = inner_radius(&SQUARE, (100.0, 100.0));
        assert!(square > 49.0 && square <= 50.0, "{square}");
        let strip = inner_radius(&[[0.0, 0.45], [1.0, 0.45], [1.0, 0.55], [0.0, 0.55]], (100.0, 100.0));
        assert!(strip > 4.8 && strip <= 5.0, "{strip}");
    }

    #[test]
    fn fit_in_box_maps_the_points_onto_their_bounds() {
        let (local, rect) = fit_in_box(&[[0.2, 0.4], [0.6, 0.4], [0.4, 0.8]], 0.01).unwrap();
        assert_eq!(rect, (0.2, 0.4, 0.6, 0.8));
        for (got, want) in local.iter().zip([[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]]) {
            assert!((got[0] - want[0]).abs() < 1e-9 && (got[1] - want[1]).abs() < 1e-9, "{local:?}");
        }
    }

    #[test]
    fn nearest_edge_counts_the_closing_edge() {
        assert_eq!(nearest_edge(&SQUARE, [-0.1, 0.5]).map(|(i, _)| i), Some(3));
    }
}
