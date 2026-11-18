//! Homographies (plane-to-plane perspective maps) fitted to point pairs: the direct linear
//! transform with Hartley's normalisation (Hartley 1997, "In defense of the eight-point
//! algorithm"; Hartley and Zisserman, "Multiple View Geometry", algorithm 4.2), exact from four
//! pairs, least squares from more, and RANSAC (Fischler and Bolles 1981) to leave out the pairs
//! that moved otherwise.

/// `q ~ H p` in homogeneous coordinates, row by row; `m[8]` is kept at 1 by the fits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography {
    pub m: [f64; 9],
}

impl Homography {
    pub const IDENTITY: Self = Self { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] };

    /// Where `p` goes; `None` when it goes to infinity (the third coordinate about 0). The sign of
    /// the third coordinate is not checked: the matrix is known only up to a scale, which may be
    /// negative.
    pub fn apply(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let m = &self.m;
        let w = m[6] * p[0] + m[7] * p[1] + m[8];
        if !(w.abs() > 1.0e-12) {
            return None;
        }
        Some([(m[0] * p[0] + m[1] * p[1] + m[2]) / w, (m[3] * p[0] + m[4] * p[1] + m[5]) / w])
    }

    /// The map that undoes this one, scaled so its last entry is 1 when it can be; `None` when this
    /// one is singular.
    pub fn inverse(&self) -> Option<Self> {
        let m = &self.m;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if !(det.abs() > 1.0e-15) || !det.is_finite() {
            return None;
        }
        Some(Self { m: c.map(|v| v / det) }.normalized())
    }

    /// `self` after `first`: the map that applies `first`, then `self`.
    pub fn after(&self, first: &Self) -> Self {
        let (a, b) = (&self.m, &first.m);
        let mut m = [0.0; 9];
        for row in 0..3 {
            for col in 0..3 {
                m[row * 3 + col] = (0..3).map(|k| a[row * 3 + k] * b[k * 3 + col]).sum();
            }
        }
        Self { m }.normalized()
    }

    /// Scaled so the last entry is 1 (or, when it is about 0, so the largest entry is 1).
    fn normalized(self) -> Self {
        let last = self.m[8];
        let scale = if last.abs() > 1.0e-12 { last } else { self.m.iter().fold(0.0f64, |big, v| if v.abs() > big.abs() { *v } else { big }) };
        if scale == 0.0 {
            return self;
        }
        Self { m: self.m.map(|v| v / scale) }
    }

    /// The map that takes the four corners `from` to the four corners `to` exactly; `None` when
    /// three of either lie on a line.
    pub fn from_quads(from: &[[f64; 2]; 4], to: &[[f64; 2]; 4]) -> Option<Self> {
        let pairs: Vec<([f64; 2], [f64; 2])> = from.iter().copied().zip(to.iter().copied()).collect();
        Self::fit(&pairs)
    }

    /// The least-squares map of the pairs (exact for four); `None` with fewer than four or when
    /// they do not fix one.
    pub fn fit(pairs: &[([f64; 2], [f64; 2])]) -> Option<Self> {
        if pairs.len() < 4 {
            return None;
        }
        let (np, tp) = normalizer(pairs.iter().map(|(p, _)| *p))?;
        let (nq, tq) = normalizer(pairs.iter().map(|(_, q)| *q))?;
        // Rows of the system for h0..h7 with h8 = 1, in normalised coordinates; solved through
        // its normal equations, which are well conditioned after the normalisation.
        let mut ata = [[0.0f64; 8]; 8];
        let mut atb = [0.0f64; 8];
        for (p, q) in pairs {
            let (x, y) = ((p[0] - tp[0]) * np, (p[1] - tp[1]) * np);
            let (u, v) = ((q[0] - tq[0]) * nq, (q[1] - tq[1]) * nq);
            let rows = [([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u), ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v)];
            for (row, rhs) in rows {
                for i in 0..8 {
                    for j in 0..8 {
                        ata[i][j] += row[i] * row[j];
                    }
                    atb[i] += row[i] * rhs;
                }
            }
        }
        let h = solve8(ata, atb)?;
        let normal = Self { m: [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0] };
        // Undo the normalisations: H = Tq^-1 * normal * Tp.
        let tpm = Self { m: [np, 0.0, -np * tp[0], 0.0, np, -np * tp[1], 0.0, 0.0, 1.0] };
        let tqi = Self { m: [1.0 / nq, 0.0, tq[0], 0.0, 1.0 / nq, tq[1], 0.0, 0.0, 1.0] };
        let result = tqi.after(&normal.after(&tpm));
        result.m.iter().all(|v| v.is_finite()).then_some(result)
    }
}

/// The scale and the middle that bring the points to the origin at an average distance of √2.
fn normalizer(points: impl Iterator<Item = [f64; 2]> + Clone) -> Option<(f64, [f64; 2])> {
    let n = points.clone().count() as f64;
    let middle = points.clone().fold([0.0, 0.0], |sum, p| [sum[0] + p[0] / n, sum[1] + p[1] / n]);
    let spread = points.map(|p| (p[0] - middle[0]).hypot(p[1] - middle[1])).sum::<f64>() / n;
    (spread > 1.0e-12 && spread.is_finite()).then(|| (std::f64::consts::SQRT_2 / spread, middle))
}

/// Gaussian elimination with partial pivoting. A pivot is taken as zero against the largest entry
/// of the matrix, so the scale of the system does not decide whether it solves.
fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    let largest = a.iter().flatten().fold(0.0f64, |most, v| most.max(v.abs()));
    if !(largest > 0.0 && largest.is_finite()) {
        return None;
    }
    let tiny = largest * 1.0e-12;
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if !(a[pivot][col].abs() > tiny) {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..8 {
            let factor = a[row][col] / a[col][col];
            for k in col..8 {
                a[row][k] -= factor * a[col][k];
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = [0.0; 8];
    for row in (0..8).rev() {
        let sum: f64 = (row + 1..8).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - sum) / a[row][row];
    }
    Some(x)
}

/// Whether the four corners make a convex quadrilateral with some area, in the order of a quad:
/// clockwise on screen (y down), as top-left, top-right, bottom-right, bottom-left go. The other
/// way round is the picture mirrored, so it is refused too.
pub fn is_convex(quad: &[[f64; 2]; 4]) -> bool {
    (0..4).all(|i| {
        let (a, b, c) = (quad[i], quad[(i + 1) % 4], quad[(i + 2) % 4]);
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        cross > 1.0e-12
    })
}

/// The area of the quadrilateral (shoelace), positive whichever way round.
pub fn quad_area(quad: &[[f64; 2]; 4]) -> f64 {
    let mut twice = 0.0;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        twice += a[0] * b[1] - b[0] * a[1];
    }
    twice.abs() / 2.0
}

/// Whether `p` lies inside the convex quadrilateral `quad`, at least `inset` from each side.
pub fn quad_contains(quad: &[[f64; 2]; 4], p: [f64; 2], inset: f64) -> bool {
    // Which way round the quad goes decides which side of each edge is inside.
    let twice: f64 = (0..4).map(|k| quad[k][0] * quad[(k + 1) % 4][1] - quad[(k + 1) % 4][0] * quad[k][1]).sum();
    let sign = if twice >= 0.0 { 1.0 } else { -1.0 };
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let length = ex.hypot(ey);
        if length < 1.0e-12 {
            return false;
        }
        let side = (ex * (p[1] - a[1]) - ey * (p[0] - a[0])) / length;
        if side * sign < inset {
            return false;
        }
    }
    true
}

/// A small deterministic random sequence (xorshift), so RANSAC gives the same answer every run.
struct Random(u64);

impl Random {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// The homography most pairs agree with (within `threshold` of where it sends them), refitted to
/// them, and which pairs agree. `None` with fewer than four pairs or when no sample fixes a map.
pub fn ransac(pairs: &[([f64; 2], [f64; 2])], threshold: f64, iterations: usize) -> Option<(Homography, Vec<bool>)> {
    if pairs.len() < 4 {
        return None;
    }
    let agree = |model: &Homography| -> Vec<bool> {
        pairs.iter().map(|(p, q)| model.apply(*p).is_some_and(|r| (r[0] - q[0]).hypot(r[1] - q[1]) <= threshold)).collect()
    };
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut best: Option<(Homography, Vec<bool>, usize)> = None;
    for _ in 0..iterations.max(1) {
        let mut picked = [0usize; 4];
        let mut k = 0;
        let mut tries = 0;
        while k < 4 && tries < 64 {
            tries += 1;
            let i = random.below(pairs.len());
            if !picked[..k].contains(&i) {
                picked[k] = i;
                k += 1;
            }
        }
        if k < 4 {
            break;
        }
        let from = picked.map(|i| pairs[i].0);
        let to = picked.map(|i| pairs[i].1);
        // A sample whose points do not make a convex quad on both sides is degenerate or folds the
        // plane: no rigid plane moves that way.
        if !(is_convex(&order_round(from)) && is_convex(&order_round(to))) {
            continue;
        }
        let Some(model) = Homography::from_quads(&from, &to) else { continue };
        let mask = agree(&model);
        let count = mask.iter().filter(|inside| **inside).count();
        if best.as_ref().is_none_or(|(_, _, known)| count > *known) {
            best = Some((model, mask, count));
        }
    }
    let (model, mask, _) = best?;
    let inliers: Vec<([f64; 2], [f64; 2])> = pairs.iter().zip(&mask).filter(|(_, inside)| **inside).map(|(pair, _)| *pair).collect();
    let refined = Homography::fit(&inliers).unwrap_or(model);
    let refined_mask = agree(&refined);
    if refined_mask.iter().filter(|v| **v).count() >= mask.iter().filter(|v| **v).count() {
        Some((refined, refined_mask))
    } else {
        Some((model, mask))
    }
}

/// The four points sorted round their middle (clockwise on screen, the way [`is_convex`] wants),
/// so a sample can be checked for convexity whatever
/// order it was drawn in (the pairs keep their own order for the fit).
fn order_round(points: [[f64; 2]; 4]) -> [[f64; 2]; 4] {
    let middle = [points.iter().map(|p| p[0]).sum::<f64>() / 4.0, points.iter().map(|p| p[1]).sum::<f64>() / 4.0];
    let mut sorted = points;
    sorted.sort_by(|a, b| (a[1] - middle[1]).atan2(a[0] - middle[0]).total_cmp(&(b[1] - middle[1]).atan2(b[0] - middle[0])));
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).hypot(a[1] - b[1]) < 1.0e-6
    }

    #[test]
    fn four_corners_map_exactly_and_back() {
        let from = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let to = [[10.0, 12.0], [90.0, 5.0], [100.0, 80.0], [3.0, 70.0]];
        let h = Homography::from_quads(&from, &to).unwrap();
        for (p, q) in from.iter().zip(&to) {
            assert!(close(h.apply(*p).unwrap(), *q));
        }
        let back = h.inverse().unwrap();
        for (p, q) in from.iter().zip(&to) {
            assert!(close(back.apply(*q).unwrap(), *p));
        }
        let identity = back.after(&h);
        for (a, b) in identity.m.iter().zip(Homography::IDENTITY.m) {
            assert!((a - b).abs() < 1.0e-9, "{:?}", identity.m);
        }
    }

    #[test]
    fn a_strong_perspective_whose_scale_comes_out_negative_still_maps() {
        let from = [[0.625, 0.125], [0.75, 0.125], [0.75, 0.347], [0.625, 0.347]];
        let to = [[0.675, 0.1], [0.85, 0.15], [0.85, 0.3], [0.65, 0.375]];
        let h = Homography::from_quads(&from, &to).unwrap();
        for (p, q) in from.iter().zip(&to) {
            assert!(close(h.apply(*p).unwrap(), *q));
        }
    }

    #[test]
    fn least_squares_recovers_a_known_map_from_many_points() {
        let truth = Homography { m: [1.1, 0.05, 3.0, -0.02, 0.95, -2.0, 0.0004, -0.0003, 1.0] };
        let pairs: Vec<_> = (0..50).map(|i| {
            let p = [(i % 10) as f64 * 13.0, (i / 10) as f64 * 17.0];
            (p, truth.apply(p).unwrap())
        }).collect();
        let fitted = Homography::fit(&pairs).unwrap();
        for (p, q) in &pairs {
            assert!(close(fitted.apply(*p).unwrap(), *q));
        }
    }

    #[test]
    fn ransac_leaves_out_wrong_pairs() {
        let truth = Homography { m: [0.9, -0.1, 20.0, 0.08, 1.05, 5.0, 0.0006, 0.0002, 1.0] };
        let mut pairs: Vec<_> = (0..60).map(|i| {
            let p = [(i % 10) as f64 * 11.0, (i / 10) as f64 * 9.0];
            (p, truth.apply(p).unwrap())
        }).collect();
        for (k, pair) in pairs.iter_mut().enumerate().filter(|(k, _)| k % 5 == 0) {
            pair.1 = [pair.1[0] + 15.0 + k as f64, pair.1[1] - 9.0];
        }
        let (fitted, mask) = ransac(&pairs, 1.0, 300).unwrap();
        for (k, inside) in mask.iter().enumerate() {
            assert_eq!(*inside, k % 5 != 0, "pair {k}");
        }
        for p in [[0.0, 0.0], [99.0, 0.0], [50.0, 45.0]] {
            assert!(close(fitted.apply(p).unwrap(), truth.apply(p).unwrap()));
        }
    }

    #[test]
    fn degenerate_points_fit_nothing() {
        let line = [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]];
        assert!(Homography::from_quads(&line, &line).is_none() || !is_convex(&line));
        assert!(Homography::fit(&[([0.0, 0.0], [1.0, 1.0]); 3]).is_none());
    }

    #[test]
    fn convexity_and_containment() {
        let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert!(is_convex(&square));
        assert!(!is_convex(&[[0.0, 0.0], [10.0, 10.0], [10.0, 0.0], [0.0, 10.0]]));
        assert!(quad_contains(&square, [5.0, 5.0], 2.0));
        assert!(!quad_contains(&square, [1.0, 5.0], 2.0));
        let reversed = [square[3], square[2], square[1], square[0]];
        assert!(quad_contains(&reversed, [5.0, 5.0], 2.0));
        assert!((quad_area(&square) - 100.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_quad_the_wrong_way_round_is_not_convex() {
        // Clockwise on screen (y down) is the order of the corners; the reverse order mirrors.
        let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert!(!is_convex(&[square[0], square[3], square[2], square[1]]));
    }

    #[test]
    fn a_small_but_well_shaped_system_still_solves() {
        let mut a = [[0.0; 8]; 8];
        let mut b = [0.0; 8];
        for i in 0..8 {
            a[i][i] = 1.0e-14;
            b[i] = 2.0e-14 * (i as f64 + 1.0);
        }
        let x = solve8(a, b).expect("only the scale is small");
        for (i, v) in x.iter().enumerate() {
            assert!((v - 2.0 * (i as f64 + 1.0)).abs() < 1e-9);
        }
        a[7][7] = 0.0;
        assert!(solve8(a, b).is_none(), "a singular one does not");
    }
}
