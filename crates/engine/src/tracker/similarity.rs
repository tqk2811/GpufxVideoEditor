//! Similarity transforms (move, uniform scale, turn) fitted to point pairs, with RANSAC (Fischler
//! and Bolles 1981) to leave out the pairs that moved otherwise.

/// `q = [a -b; b a] p + t`: a turn by `atan2(b, a)` (clockwise on a picture whose y goes down)
/// and a scale by `hypot(a, b)`, then a move by `t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Similarity {
    pub const IDENTITY: Self = Self { a: 1.0, b: 0.0, tx: 0.0, ty: 0.0 };

    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        [self.a * p[0] - self.b * p[1] + self.tx, self.b * p[0] + self.a * p[1] + self.ty]
    }

    pub fn scale(&self) -> f64 {
        self.a.hypot(self.b)
    }

    /// Clockwise degrees.
    pub fn angle(&self) -> f64 {
        self.b.atan2(self.a).to_degrees()
    }

    /// The transform that maps exactly `p1` to `q1` and `p2` to `q2` (without a turn when
    /// `rotation` is false: then the scale along their line). `None` when `p1` and `p2` coincide.
    pub fn from_two(p1: [f64; 2], p2: [f64; 2], q1: [f64; 2], q2: [f64; 2], rotation: bool) -> Option<Self> {
        let (px, py) = (p1[0] - p2[0], p1[1] - p2[1]);
        let (qx, qy) = (q1[0] - q2[0], q1[1] - q2[1]);
        let length = px * px + py * py;
        if length < 1.0e-9 {
            return None;
        }
        // (a + ib) = (q1 - q2) / (p1 - p2) as complex numbers.
        let (mut a, mut b) = ((qx * px + qy * py) / length, (qy * px - qx * py) / length);
        if !rotation {
            a = a.hypot(b);
            b = 0.0;
        }
        Some(Self { a, b, tx: q1[0] - (a * p1[0] - b * p1[1]), ty: q1[1] - (b * p1[0] + a * p1[1]) })
    }

    /// The least-squares transform of the pairs. `None` with fewer than two distinct points.
    pub fn fit(pairs: &[([f64; 2], [f64; 2])], rotation: bool) -> Option<Self> {
        if pairs.len() < 2 {
            return None;
        }
        let n = pairs.len() as f64;
        let (mut px, mut py, mut qx, mut qy) = (0.0, 0.0, 0.0, 0.0);
        for (p, q) in pairs {
            px += p[0];
            py += p[1];
            qx += q[0];
            qy += q[1];
        }
        let (px, py, qx, qy) = (px / n, py / n, qx / n, qy / n);
        let (mut dot, mut cross, mut length) = (0.0, 0.0, 0.0);
        for (p, q) in pairs {
            let (ux, uy) = (p[0] - px, p[1] - py);
            let (vx, vy) = (q[0] - qx, q[1] - qy);
            dot += ux * vx + uy * vy;
            cross += ux * vy - uy * vx;
            length += ux * ux + uy * uy;
        }
        if length < 1.0e-9 {
            return None;
        }
        // Without a turn the scale is the length of the fitted (a, b), as in `from_two`: `dot`
        // alone is the scale times the cosine of the turn, which shrinks the box of an object that
        // turns and flips it past a quarter turn.
        let (a, b) = if rotation { (dot / length, cross / length) } else { (dot.hypot(cross) / length, 0.0) };
        Some(Self { a, b, tx: qx - (a * px - b * py), ty: qy - (b * px + a * py) })
    }
}

/// A small fixed-seed generator, so a run of the tracker gives the same answer every time.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// The transform most pairs agree with, within `threshold` pixels, refitted on those pairs, and
/// which pairs they are. `None` with fewer than two pairs.
pub fn ransac(pairs: &[([f64; 2], [f64; 2])], rotation: bool, threshold: f64, iterations: usize) -> Option<(Similarity, Vec<bool>)> {
    if pairs.len() < 2 {
        return None;
    }
    let squared = threshold * threshold;
    let inliers_of = |model: &Similarity| -> Vec<bool> {
        pairs
            .iter()
            .map(|(p, q)| {
                let m = model.apply(*p);
                (m[0] - q[0]).powi(2) + (m[1] - q[1]).powi(2) <= squared
            })
            .collect()
    };
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut best: Option<(usize, Similarity)> = None;
    let mut consider = |model: Similarity| {
        let count = inliers_of(&model).iter().filter(|inside| **inside).count();
        if best.is_none_or(|(known, _)| count > known) {
            best = Some((count, model));
        }
    };
    if let Some(all) = Similarity::fit(pairs, rotation) {
        consider(all);
    }
    for _ in 0..iterations {
        let i = random.below(pairs.len());
        let j = random.below(pairs.len());
        if i == j {
            continue;
        }
        if let Some(model) = Similarity::from_two(pairs[i].0, pairs[j].0, pairs[i].1, pairs[j].1, rotation) {
            consider(model);
        }
    }
    let (_, mut model) = best?;
    let mut mask = inliers_of(&model);
    // Refit on the agreeing pairs, twice, as the refit can take in a few more.
    for _ in 0..2 {
        let chosen: Vec<_> = pairs.iter().zip(&mask).filter(|(_, inside)| **inside).map(|(pair, _)| *pair).collect();
        let Some(refit) = Similarity::fit(&chosen, rotation) else { break };
        model = refit;
        mask = inliers_of(&model);
    }
    Some((model, mask))
}
