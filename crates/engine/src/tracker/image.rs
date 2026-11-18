//! Grayscale pictures and their pyramids, the input of the tracker.

/// A grayscale picture, brightness from 0 to 1, top row first.
#[derive(Debug, Clone, PartialEq)]
pub struct GrayImage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl GrayImage {
    /// A picture of `width` x `height` from `data`, which must hold that many values.
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Self {
        assert_eq!(data.len(), width * height, "a {width}x{height} picture needs {} values", width * height);
        Self { width, height, data }
    }

    /// The brightness (BT.709 weights) of an RGBA picture, `width * height * 4` bytes.
    pub fn from_rgba(width: usize, height: usize, rgba: &[u8]) -> Self {
        assert_eq!(rgba.len(), width * height * 4, "a {width}x{height} RGBA picture needs {} bytes", width * height * 4);
        let data = rgba.as_chunks::<4>().0.iter().map(|p| (0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])) / 255.0).collect();
        Self { width, height, data }
    }

    /// The value at whole pixel (`x`, `y`), the nearest edge pixel outside.
    pub fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        self.data[y * self.width + x]
    }

    /// The value at (`x`, `y`), pixel centres at whole numbers, interpolated between the four
    /// pixels around it; the edge is repeated outside.
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (fx, fy) = (x.floor(), y.floor());
        let (ax, ay) = (x - fx, y - fy);
        let (ix, iy) = (fx as isize, fy as isize);
        let top = self.at(ix, iy) * (1.0 - ax) + self.at(ix + 1, iy) * ax;
        let bottom = self.at(ix, iy + 1) * (1.0 - ax) + self.at(ix + 1, iy + 1) * ax;
        top * (1.0 - ay) + bottom * ay
    }

    /// Half the size, after a [1 2 1] blur each way so the smaller picture does not alias.
    pub fn half(&self) -> Self {
        let (width, height) = (self.width.div_ceil(2).max(1), self.height.div_ceil(2).max(1));
        let mut data = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = ((x * 2) as isize, (y * 2) as isize);
                let mut sum = 0.0;
                for (dy, wy) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                    for (dx, wx) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                        sum += self.at(sx + dx, sy + dy) * wx * wy;
                    }
                }
                data.push(sum / 16.0);
            }
        }
        Self { width, height, data }
    }
}

/// A picture and its halvings, level 0 the picture itself.
#[derive(Debug, Clone)]
pub struct Pyramid {
    pub levels: Vec<GrayImage>,
}

impl Pyramid {
    /// Up to `levels` levels, stopping before a side would fall under 16 pixels.
    pub fn new(image: GrayImage, levels: usize) -> Self {
        let mut all = vec![image];
        while all.len() < levels.max(1) {
            let last = all.last().expect("one level at least");
            if last.width < 32 || last.height < 32 {
                break;
            }
            let next = last.half();
            all.push(next);
        }
        Self { levels: all }
    }

    pub fn base(&self) -> &GrayImage {
        &self.levels[0]
    }
}
