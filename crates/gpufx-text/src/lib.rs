//! Rasterizes the text of a text clip into a premultiplied RGBA picture on the CPU.
//!
//! Shaping, bidi and font fallback come from `cosmic-text`, glyph pictures from its swash cache.
//! Outline, background box and drop shadow are drawn here on alpha masks.

use std::sync::{Arc, Mutex, OnceLock};

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Wrap};
use engine::project::{FontRef, Rgba, TextAlign, TextContent};

/// The font built into the editor: Be Vietnam Pro Regular, under the SIL Open Font License 1.1
/// (see `fonts/OFL.txt`).
pub const DEFAULT_FONT: &[u8] = include_bytes!("../fonts/BeVietnamPro-Regular.ttf");
/// The family name of [`DEFAULT_FONT`].
pub const DEFAULT_FAMILY: &str = "Be Vietnam Pro";

/// The widest or tallest picture [`TextRasterizer::rasterize`] returns, in pixels. Larger text is
/// drawn smaller so that it fits.
pub const MAX_DIMENSION: u32 = 8192;
/// The most pixels a returned picture may hold (64 MiB of RGBA).
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;

/// A rasterized text picture.
#[derive(Debug, Clone, PartialEq)]
pub struct TextImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` pixels, row by row from the top, as premultiplied RGBA.
    pub pixels: Vec<u8>,
    /// The system font family that was asked for and not found, so the default font drew it.
    pub missing_family: Option<String>,
    /// How many glyphs no loaded font could draw (shown as the font's "missing" box).
    pub missing_glyphs: usize,
    /// Whether the text was drawn smaller than asked to stay within [`MAX_DIMENSION`] and
    /// [`MAX_PIXELS`].
    pub downscaled: bool,
}

/// Holds the fonts and the glyph cache. Building one loads fonts, so make one and keep it.
pub struct TextRasterizer {
    font_system: FontSystem,
    cache: SwashCache,
}

impl Default for TextRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

/// The rasterizer with the system fonts, built the first time it is asked for and then shared by
/// everyone, since building one scans every installed font.
pub fn shared() -> Arc<Mutex<TextRasterizer>> {
    static SHARED: OnceLock<Arc<Mutex<TextRasterizer>>> = OnceLock::new();
    SHARED.get_or_init(|| Arc::new(Mutex::new(TextRasterizer::new()))).clone()
}

impl TextRasterizer {
    /// The built-in font plus every font installed on the system.
    pub fn new() -> TextRasterizer {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self::with_db(db)
    }

    /// Only the built-in font, so the result does not depend on the machine. For tests.
    pub fn embedded_only() -> TextRasterizer {
        Self::with_db(fontdb::Database::new())
    }

    fn with_db(mut db: fontdb::Database) -> TextRasterizer {
        db.load_font_source(fontdb::Source::Binary(Arc::new(DEFAULT_FONT)));
        db.set_sans_serif_family(DEFAULT_FAMILY);
        let locale = sys_locale();
        TextRasterizer { font_system: FontSystem::new_with_locale_and_db(locale, db), cache: SwashCache::new() }
    }

    /// Whether a font family with this name is loaded, ignoring case.
    pub fn has_family(&self, family: &str) -> bool {
        self.font_system.db().faces().any(|face| face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(family)))
    }

    /// Draws `text` for an output `output_height_px` pixels tall, `scale` times as large (such as
    /// below 1 for a smaller preview). Empty text, or a size that comes to no pixels, gives a
    /// 0 by 0 picture.
    pub fn rasterize(&mut self, text: &TextContent, output_height_px: u32, scale: f32) -> TextImage {
        let font_px = text.size * output_height_px as f32 * scale;
        let first = self.rasterize_px(text, font_px);
        let (w, h) = (first.width as u64, first.height as u64);
        if w <= MAX_DIMENSION as u64 && h <= MAX_DIMENSION as u64 && w * h <= MAX_PIXELS {
            return first;
        }
        // Everything scales with the font size, so shrink it once by the ratio that fits.
        let ratio = (MAX_DIMENSION as f64 / w as f64).min(MAX_DIMENSION as f64 / h as f64).min((MAX_PIXELS as f64 / (w * h) as f64).sqrt()) * 0.98;
        let mut image = self.rasterize_px(text, font_px * ratio as f32);
        image.downscaled = true;
        image
    }

    fn rasterize_px(&mut self, text: &TextContent, font_px: f32) -> TextImage {
        let mut image = TextImage { width: 0, height: 0, pixels: Vec::new(), missing_family: None, missing_glyphs: 0, downscaled: false };
        let family = match &text.font {
            FontRef::System { family } if self.has_family(family) => family.clone(),
            FontRef::System { family } => {
                image.missing_family = Some(family.clone());
                DEFAULT_FAMILY.to_string()
            }
            FontRef::Default => DEFAULT_FAMILY.to_string(),
        };
        if text.content.is_empty() || !font_px.is_finite() || font_px < 0.5 {
            return image;
        }
        let font_px = font_px.min(MAX_DIMENSION as f32);
        let line_spacing = if text.line_spacing.is_finite() && text.line_spacing > 0.0 { text.line_spacing } else { 1.0 };
        let line_height = (font_px * 1.25 * line_spacing).max(1.0);

        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_px, line_height));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        let attrs = Attrs::new().family(Family::Name(&family));
        buffer.set_text(&text.content, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Glyph positions and the size of the text block.
        let mut glyphs = Vec::new();
        let mut text_w = 0f32;
        let mut text_h = 0f32;
        let mut lines = Vec::new();
        for run in buffer.layout_runs() {
            text_w = text_w.max(run.line_w);
            text_h = text_h.max(run.line_top + run.line_height);
            lines.push((run.line_w, run.line_y, run.glyphs.to_vec()));
        }
        for (line_w, line_y, run_glyphs) in lines {
            let shift = match text.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (text_w - line_w) / 2.0,
                TextAlign::Right => text_w - line_w,
            };
            for glyph in run_glyphs {
                if glyph.glyph_id == 0 {
                    image.missing_glyphs += 1;
                }
                glyphs.push(glyph.physical((shift, line_y), 1.0));
            }
        }

        // Room around the text for the outline, the box and the shadow.
        let outline_px = text.outline.map(|o| (o.width * font_px).clamp(0.0, font_px)).unwrap_or(0.0);
        let pad_px = text.background.map(|b| (b.padding * font_px).clamp(0.0, font_px * 4.0)).unwrap_or(0.0);
        let (shadow_dx, shadow_dy, shadow_blur) = text.shadow.map(|s| ((s.offset[0] * font_px).clamp(-font_px * 4.0, font_px * 4.0), (s.offset[1] * font_px).clamp(-font_px * 4.0, font_px * 4.0), (s.blur * font_px).clamp(0.0, font_px * 2.0))).unwrap_or((0.0, 0.0, 0.0));
        let base = outline_px.max(pad_px).ceil() + 1.0;
        let left = base + (-shadow_dx).max(0.0).ceil() + if text.shadow.is_some() { shadow_blur.ceil() * 2.0 } else { 0.0 };
        let right = base + shadow_dx.max(0.0).ceil() + if text.shadow.is_some() { shadow_blur.ceil() * 2.0 } else { 0.0 };
        let top = base + (-shadow_dy).max(0.0).ceil() + if text.shadow.is_some() { shadow_blur.ceil() * 2.0 } else { 0.0 };
        let bottom = base + shadow_dy.max(0.0).ceil() + if text.shadow.is_some() { shadow_blur.ceil() * 2.0 } else { 0.0 };
        let width = (left + text_w.ceil() + right) as usize;
        let height = (top + text_h.ceil() + bottom) as usize;
        image.width = width as u32;
        image.height = height as u32;
        if (width as u64) * (height as u64) > MAX_PIXELS * 4 {
            // Far too large: the caller shrinks it; skip drawing.
            return image;
        }

        // Glyph coverage.
        let mut fill = Mask::new(width, height);
        let (ox, oy) = (left as i32, top as i32);
        for glyph in &glyphs {
            let Some(picture) = self.cache.get_image(&mut self.font_system, glyph.cache_key).as_ref() else { continue };
            let p = picture.placement;
            let x0 = ox + glyph.x + p.left;
            let y0 = oy + glyph.y - p.top;
            let stride = match picture.content {
                SwashContent::Mask => 1,
                SwashContent::SubpixelMask | SwashContent::Color => 4,
            };
            for j in 0..p.height as i32 {
                for i in 0..p.width as i32 {
                    let index = (j as usize * p.width as usize + i as usize) * stride;
                    // Colour glyphs (emoji) are drawn in the text colour for now.
                    let a = picture.data[index + stride - 1] as f32 / 255.0;
                    fill.add(x0 + i, y0 + j, a);
                }
            }
        }

        let outline = text.outline.filter(|_| outline_px > 0.0).map(|o| (o.color, fill.dilate(outline_px)));
        let silhouette = outline.as_ref().map(|(_, m)| m).unwrap_or(&fill);

        let mut canvas = vec![[0f32; 4]; width * height];
        if let Some(bg) = text.background {
            let radius = (bg.radius * font_px).max(0.0);
            let rect = (left - pad_px, top - pad_px, left + text_w + pad_px, top + text_h + pad_px);
            let color = premultiplied(bg.color);
            for y in 0..height {
                for x in 0..width {
                    let a = rounded_rect_coverage(x as f32 + 0.5, y as f32 + 0.5, rect, radius);
                    over(&mut canvas[y * width + x], color, a);
                }
            }
        }
        if let Some(shadow) = text.shadow {
            let mut mask = silhouette.shifted(shadow_dx.round() as i32, shadow_dy.round() as i32);
            if shadow_blur > 0.0 {
                mask.blur(shadow_blur);
            }
            draw_mask(&mut canvas, &mask, premultiplied(shadow.color));
        }
        if let Some((color, mask)) = &outline {
            draw_mask(&mut canvas, mask, premultiplied(*color));
        }
        draw_mask(&mut canvas, &fill, premultiplied(text.color));

        image.pixels = canvas.iter().flat_map(|p| p.map(|c| (c * 255.0 + 0.5).clamp(0.0, 255.0) as u8)).collect();
        image
    }
}

fn sys_locale() -> String {
    std::env::var("LANG").ok().and_then(|l| l.split('.').next().map(str::to_string)).filter(|l| !l.is_empty()).unwrap_or_else(|| "en-US".to_string())
}

fn premultiplied(c: Rgba) -> [f32; 4] {
    let a = c[3] as f32 / 255.0;
    [c[0] as f32 / 255.0 * a, c[1] as f32 / 255.0 * a, c[2] as f32 / 255.0 * a, a]
}

/// Puts `color` scaled by `coverage` over `dst`, both premultiplied.
fn over(dst: &mut [f32; 4], color: [f32; 4], coverage: f32) {
    if coverage <= 0.0 {
        return;
    }
    let k = 1.0 - color[3] * coverage;
    for c in 0..4 {
        dst[c] = color[c] * coverage + dst[c] * k;
    }
}

fn draw_mask(canvas: &mut [[f32; 4]], mask: &Mask, color: [f32; 4]) {
    for (dst, &a) in canvas.iter_mut().zip(&mask.data) {
        over(dst, color, a);
    }
}

/// Coverage of a pixel centre by a rectangle with rounded corners.
fn rounded_rect_coverage(x: f32, y: f32, (x0, y0, x1, y1): (f32, f32, f32, f32), radius: f32) -> f32 {
    let r = radius.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let (dx, dy) = (x - cx, y - cy);
    let distance = if r > 0.0 && (dx != 0.0 || dy != 0.0) && (x < x0 + r || x > x1 - r) && (y < y0 + r || y > y1 - r) {
        (dx * dx + dy * dy).sqrt() - r
    } else {
        (x0 - x).max(x - x1).max(y0 - y).max(y - y1)
    };
    (0.5 - distance).clamp(0.0, 1.0)
}

/// A single-channel coverage picture, 0 to 1.
#[derive(Clone)]
struct Mask {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Mask {
    fn new(width: usize, height: usize) -> Mask {
        Mask { width, height, data: vec![0.0; width * height] }
    }

    fn add(&mut self, x: i32, y: i32, a: f32) {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            let v = &mut self.data[y as usize * self.width + x as usize];
            *v = (*v + a).min(1.0);
        }
    }

    fn shifted(&self, dx: i32, dy: i32) -> Mask {
        let mut out = Mask::new(self.width, self.height);
        for y in 0..self.height as i32 {
            let sy = y - dy;
            if sy < 0 || sy >= self.height as i32 {
                continue;
            }
            for x in 0..self.width as i32 {
                let sx = x - dx;
                if sx >= 0 && sx < self.width as i32 {
                    out.data[y as usize * self.width + x as usize] = self.data[sy as usize * self.width + sx as usize];
                }
            }
        }
        out
    }

    /// The largest coverage within a disc of `radius` pixels around each pixel.
    fn dilate(&self, radius: f32) -> Mask {
        let r = radius.ceil() as i32;
        let mut out = Mask::new(self.width, self.height);
        let mut row_max = vec![0f32; self.width];
        for y in 0..self.height as i32 {
            let out_row = &mut out.data[y as usize * self.width..(y as usize + 1) * self.width];
            for dy in -r..=r {
                let sy = y + dy;
                if sy < 0 || sy >= self.height as i32 {
                    continue;
                }
                let half = (radius * radius - (dy * dy) as f32).max(0.0).sqrt().floor() as usize;
                let src = &self.data[sy as usize * self.width..(sy as usize + 1) * self.width];
                sliding_max(src, half, &mut row_max);
                for (o, &m) in out_row.iter_mut().zip(&row_max) {
                    *o = o.max(m);
                }
            }
        }
        out
    }

    /// Approximates a Gaussian blur of about `radius` pixels with three box blurs.
    fn blur(&mut self, radius: f32) {
        let half = ((radius / 2.0).round() as usize).max(1);
        let mut tmp = vec![0f32; self.width.max(self.height)];
        for _ in 0..3 {
            for y in 0..self.height {
                let row = &mut self.data[y * self.width..(y + 1) * self.width];
                box_blur(row, half, &mut tmp);
            }
            let mut column = vec![0f32; self.height];
            for x in 0..self.width {
                for y in 0..self.height {
                    column[y] = self.data[y * self.width + x];
                }
                box_blur(&mut column, half, &mut tmp);
                for y in 0..self.height {
                    self.data[y * self.width + x] = column[y];
                }
            }
        }
    }
}

/// `out[i]` = max of `src[i - half ..= i + half]`, in linear time (van Herk / Gil-Werman).
fn sliding_max(src: &[f32], half: usize, out: &mut [f32]) {
    let n = src.len();
    if half == 0 {
        out.copy_from_slice(src);
        return;
    }
    let k = 2 * half + 1;
    let mut prefix = vec![0f32; n];
    let mut suffix = vec![0f32; n];
    for i in 0..n {
        prefix[i] = if i % k == 0 { src[i] } else { prefix[i - 1].max(src[i]) };
    }
    for i in (0..n).rev() {
        suffix[i] = if i == n - 1 || (i + 1) % k == 0 { src[i] } else { suffix[i + 1].max(src[i]) };
    }
    for i in 0..n {
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(n - 1);
        out[i] = if lo / k != hi / k {
            suffix[lo].max(prefix[hi])
        } else if lo % k == 0 {
            prefix[hi]
        } else {
            // Same block, cut short by the right edge, where the suffix restarts.
            suffix[lo]
        };
    }
}

fn box_blur(data: &mut [f32], half: usize, tmp: &mut [f32]) {
    let n = data.len();
    let window = (2 * half + 1) as f32;
    let mut sum = 0f32;
    let at = |i: isize| if i < 0 || i >= n as isize { 0.0 } else { data[i as usize] };
    for i in -(half as isize)..=(half as isize) {
        sum += at(i);
    }
    for i in 0..n {
        tmp[i] = sum / window;
        sum += at(i as isize + half as isize + 1) - at(i as isize - half as isize);
    }
    data.copy_from_slice(&tmp[..n]);
}
