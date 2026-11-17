//! The paints of the text layers (solid colour, gradient, texture picture) and the layers drawn
//! from the glyph coverage: shadows with their own outlines, outlines and inner shadows.
//!
//! Every layer is drawn from one squared distance field of the glyph coverage, made once per
//! picture: an outline is the coverage grown by its width, so many outlines cost one pass each over
//! the picture and no other field. Shadows that differ only by where they lie (the stack of a 3D
//! text) share one picture of the shadow, drawn and blurred once and put down at each offset.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::project::{GradientStop, TextContent, TextImageRef, TextOutline, TextPaint, TextTexture, TextureFit};

use super::{font_file_stamp, over, premultiplied, FontFileStamp, Mask};

/// The largest texture picture file read, in bytes.
pub const MAX_TEXTURE_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// The widest or tallest texture picture used; a larger one is not drawn.
pub const MAX_TEXTURE_DIMENSION: u32 = 4096;

/// A decoded texture picture, premultiplied, 0 to 1.
pub(crate) struct Picture {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 4]>,
}

impl Picture {
    /// Reads a PNG or JPEG file. `None` when it is missing, too large or not a picture.
    fn load(path: &Path) -> Option<Picture> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_TEXTURE_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?;
        let (width, height) = reader.into_dimensions().ok()?;
        if width == 0 || height == 0 || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION {
            return None;
        }
        let decoded = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?.decode().ok()?.into_rgba8();
        let pixels = decoded.pixels().map(|p| premultiplied(p.0)).collect();
        Some(Picture { width: decoded.width() as usize, height: decoded.height() as usize, pixels })
    }

    /// The colour at `(u, v)` in picture pixels, bilinear, with the picture repeated (mirrored
    /// every other copy when `mirror`) past its edges.
    fn sample(&self, u: f32, v: f32, mirror: bool) -> [f32; 4] {
        let wrap = |i: i64, n: usize| -> usize {
            let n = n as i64;
            if mirror {
                let period = 2 * n;
                let m = i.rem_euclid(period);
                (if m < n { m } else { period - 1 - m }) as usize
            } else {
                i.rem_euclid(n) as usize
            }
        };
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let at = |i: i64, j: i64| self.pixels[wrap(j, self.height) * self.width + wrap(i, self.width)];
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
        std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
    }
}

/// The texture pictures a text draws with, by the path of their file.
#[derive(Clone, Default)]
pub(crate) struct Textures(Vec<(String, Arc<Picture>)>);

impl Textures {
    fn get(&self, path: &str) -> Option<&Picture> {
        self.0.iter().find(|(known, _)| known == path).map(|(_, picture)| &**picture)
    }

    pub(crate) fn bytes(&self) -> usize {
        self.0.iter().map(|(_, picture)| picture.pixels.len() * 16).sum()
    }
}

/// Every picture the layers of `text` paint with, each once.
pub fn text_images(text: &TextContent) -> Vec<&TextImageRef> {
    let mut paints: Vec<&TextPaint> = vec![&text.fill];
    paints.extend(text.outlines.iter().map(|outline| &outline.paint));
    for shadow in &text.shadows {
        paints.push(&shadow.paint);
        paints.extend(shadow.outlines.iter().map(|outline| &outline.paint));
    }
    paints.extend(text.inner_shadows.iter().map(|shadow| &shadow.paint));
    let mut images: Vec<&TextImageRef> = Vec::new();
    for image in paints.into_iter().filter_map(TextPaint::texture_image) {
        if !images.contains(&image) {
            images.push(image);
        }
    }
    images
}

/// The stamps of the texture files `text` paints with (`None` for a picture that is not a file or
/// cannot be read), in the order of [`text_images`]. Goes into the keys of cached pictures, so a
/// changed file is drawn again.
pub fn text_texture_stamps(text: &TextContent) -> Vec<Option<FontFileStamp>> {
    text_images(text)
        .into_iter()
        .map(|image| match image {
            TextImageRef::File { path } => font_file_stamp(Path::new(path)),
            TextImageRef::Pack { .. } => None,
        })
        .collect()
}

/// Decoded texture pictures by path, with the stamp of the file they were read from; `None` for a
/// file that is not a picture.
#[derive(Default)]
pub(crate) struct TextureCache {
    files: HashMap<PathBuf, (Option<FontFileStamp>, Option<Arc<Picture>>)>,
}

/// The most texture files kept decoded.
const MAX_CACHED_TEXTURES: usize = 64;

impl TextureCache {
    /// The pictures `text` paints with, and the first one that could not be drawn (a pack picture
    /// the caller did not resolve to its file, a missing file, or one that is not a picture).
    pub(crate) fn textures_for(&mut self, text: &TextContent) -> (Textures, Option<String>) {
        let mut textures = Textures::default();
        let mut missing = None;
        for image in text_images(text) {
            let picture = match image {
                TextImageRef::File { path } => self.load(Path::new(path)),
                TextImageRef::Pack { .. } => None,
            };
            match (picture, image) {
                (Some(picture), TextImageRef::File { path }) => textures.0.push((path.clone(), picture)),
                _ => {
                    missing.get_or_insert_with(|| image.label());
                }
            }
        }
        (textures, missing)
    }

    fn load(&mut self, path: &Path) -> Option<Arc<Picture>> {
        let stamp = font_file_stamp(path);
        if let Some((known, picture)) = self.files.get(path) {
            if *known == stamp {
                return picture.clone();
            }
        }
        if self.files.len() >= MAX_CACHED_TEXTURES {
            self.files.clear();
        }
        let picture = stamp.and_then(|_| Picture::load(path)).map(Arc::new);
        self.files.insert(path.to_owned(), (stamp, picture.clone()));
        picture
    }
}

/// Where the paints are laid: the box of the whole text in the picture being drawn, the font size
/// and the pictures of the textures.
#[derive(Clone, Copy)]
pub(crate) struct Painter<'a> {
    pub textures: &'a Textures,
    /// The line boxes of the text (x0, y0, x1, y1), in the pixels of the picture drawn into.
    pub rect: (f32, f32, f32, f32),
    pub font_px: f32,
}

impl<'a> Painter<'a> {
    /// The same paints for a picture whose pixel `(0, 0)` is pixel `(dx, dy)` of this one's.
    pub fn moved(&self, dx: f32, dy: f32) -> Painter<'a> {
        let (x0, y0, x1, y1) = self.rect;
        Painter { rect: (x0 - dx, y0 - dy, x1 - dx, y1 - dy), ..*self }
    }

    /// The premultiplied colour of `paint` at the centre of pixel `(x, y)`.
    pub fn color_at(&self, paint: &TextPaint, x: usize, y: usize) -> [f32; 4] {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        match paint {
            TextPaint::Solid { color } => premultiplied(*color),
            TextPaint::Gradient { stops, angle } => gradient_color(stops, self.gradient_t(*angle, px, py)),
            TextPaint::Texture(texture) => self.texture_color(texture, px, py),
        }
    }

    /// Where `(px, py)` lies along a gradient running `angle` degrees clockwise from left to right
    /// across the text box: 0 at the box corner it starts from, 1 at the one it ends at.
    fn gradient_t(&self, angle: f32, px: f32, py: f32) -> f32 {
        let (x0, y0, x1, y1) = self.rect;
        let radians = if angle.is_finite() { angle.to_radians() } else { 0.0 };
        let (dx, dy) = (radians.cos(), radians.sin());
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let half = ((x1 - x0) * dx.abs() + (y1 - y0) * dy.abs()) / 2.0;
        if half <= 0.0 {
            return 0.0;
        }
        (((px - cx) * dx + (py - cy) * dy) / (2.0 * half) + 0.5).clamp(0.0, 1.0)
    }

    fn texture_color(&self, texture: &TextTexture, px: f32, py: f32) -> [f32; 4] {
        let TextImageRef::File { path } = &texture.image else { return [0.0; 4] };
        let Some(picture) = self.textures.get(path) else { return [0.0; 4] };
        let (x0, y0, x1, y1) = self.rect;
        let (iw, ih) = (picture.width as f32, picture.height as f32);
        let scale = if texture.scale.is_finite() { texture.scale.clamp(*engine::TEXT_TEXTURE_SCALE_RANGE.start(), *engine::TEXT_TEXTURE_SCALE_RANGE.end()) } else { 1.0 };
        // Screen pixels per picture pixel, and the screen point picture pixel (0, 0) lands on.
        let (zoom, anchor) = match texture.fit {
            TextureFit::Stretch => {
                let zoom = ((x1 - x0) / iw).max((y1 - y0) / ih).max(1e-3) * scale;
                (zoom, ((x0 + x1) / 2.0 - iw * zoom / 2.0, (y0 + y1) / 2.0 - ih * zoom / 2.0))
            }
            TextureFit::Tile | TextureFit::Mirror => ((scale * self.font_px / ih).max(1e-3), (x0, y0)),
        };
        // Turn the point back around the text centre by the picture's turn.
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let radians = if texture.angle.is_finite() { -texture.angle.to_radians() } else { 0.0 };
        let (s, c) = radians.sin_cos();
        let (rx, ry) = (px - cx, py - cy);
        let (qx, qy) = (cx + rx * c - ry * s, cy + rx * s + ry * c);
        let color = picture.sample((qx - anchor.0) / zoom, (qy - anchor.1) / zoom, texture.fit == TextureFit::Mirror);
        let opacity = if texture.opacity.is_finite() { texture.opacity.clamp(0.0, 1.0) } else { 1.0 };
        color.map(|v| v * opacity)
    }

    /// Paints the coverage `mask` with `paint` over `canvas` of the same size.
    pub fn draw(&self, canvas: &mut [[f32; 4]], mask: &Mask, paint: &TextPaint) {
        if let TextPaint::Solid { color } = paint {
            let color = premultiplied(*color);
            for (dst, &a) in canvas.iter_mut().zip(&mask.data) {
                over(dst, color, a);
            }
            return;
        }
        let width = mask.width;
        for (index, (dst, &a)) in canvas.iter_mut().zip(&mask.data).enumerate() {
            if a > 0.0 {
                over(dst, self.color_at(paint, index % width, index / width), a);
            }
        }
    }
}

/// The colour of a gradient at `t`, 0 to 1, premultiplied. Stops are mixed as straight colours.
fn gradient_color(stops: &[GradientStop], t: f32) -> [f32; 4] {
    let Some(first) = stops.first() else { return [0.0; 4] };
    if t <= first.position || stops.len() == 1 {
        return premultiplied(first.color);
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t <= b.position {
            let span = b.position - a.position;
            let k = if span > 0.0 { ((t - a.position) / span).clamp(0.0, 1.0) } else { 1.0 };
            let mixed: [u8; 4] = std::array::from_fn(|c| (f32::from(a.color[c]) + (f32::from(b.color[c]) - f32::from(a.color[c])) * k).round() as u8);
            return premultiplied(mixed);
        }
    }
    premultiplied(stops[stops.len() - 1].color)
}

/// A shadow in pixels: offset, blur and the widest of its outlines.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ShadowPx {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub outline_px: f32,
}

/// The width in pixels an outline `width` font sizes wide draws at.
pub(crate) fn outline_px(width: f32, font_px: f32) -> f32 {
    if width.is_finite() { (width * font_px).clamp(0.0, font_px * engine::MAX_TEXT_OUTLINE_WIDTH) } else { 0.0 }
}

/// Draws `outlines` of the coverage `fill` (squared distances `squared`) over `canvas`, widest
/// first.
pub(crate) fn draw_outlines(canvas: &mut [[f32; 4]], fill: &Mask, squared: &[f64], outlines: &[TextOutline], painter: &Painter) {
    let mut sorted: Vec<(f32, &TextPaint)> = outlines.iter().map(|outline| (outline_px(outline.width, painter.font_px), &outline.paint)).filter(|(width, _)| *width > 0.0).collect();
    sorted.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (width, paint) in sorted {
        painter.draw(canvas, &fill.dilated(squared, width), paint);
    }
}

/// Draws the shadows of `text` over `canvas`, the last first, each the coverage `fill` grown by
/// its outlines, painted, blurred and moved. `shadows` holds the pixels of each.
pub(crate) fn draw_shadows(canvas: &mut [[f32; 4]], text: &TextContent, shadows: &[ShadowPx], fill: &Mask, squared: Option<&[f64]>, painter: &Painter) {
    let (width, height) = (fill.width, fill.height);
    // The picture of the shadow last drawn, unmoved, with the index of the shadow it was drawn for.
    let mut kept: Option<(usize, Vec<[f32; 4]>)> = None;
    for (index, (shadow, px)) in text.shadows.iter().zip(shadows).enumerate().rev() {
        let (dx, dy) = (px.dx.round() as i32, px.dy.round() as i32);
        let solid = shadow.paint.solid_color().filter(|_| shadow.outlines.iter().all(|outline| outline_px(outline.width, painter.font_px) <= 0.0));
        if let Some(color) = solid {
            // A plain shadow needs only its coverage.
            let mut mask = fill.shifted(dx, dy);
            mask.blur(px.blur);
            painter.draw(canvas, &mask, &TextPaint::solid(color));
            continue;
        }
        let same = kept.as_ref().is_some_and(|(at, _)| {
            let other = &text.shadows[*at];
            other.paint == shadow.paint && other.outlines == shadow.outlines && shadows[*at].blur == px.blur
        });
        if !same {
            let mut layer = vec![[0f32; 4]; width * height];
            if let Some(squared) = squared {
                draw_outlines(&mut layer, fill, squared, &shadow.outlines, painter);
            }
            painter.draw(&mut layer, fill, &shadow.paint);
            blur_colors(&mut layer, width, height, px.blur);
            kept = Some((index, layer));
        }
        let Some((_, layer)) = &kept else { continue };
        for y in 0..height as i32 {
            let sy = y - dy;
            if sy < 0 || sy >= height as i32 {
                continue;
            }
            for x in 0..width as i32 {
                let sx = x - dx;
                if sx >= 0 && sx < width as i32 {
                    let color = layer[sy as usize * width + sx as usize];
                    if color[3] > 0.0 {
                        over(&mut canvas[y as usize * width + x as usize], color, 1.0);
                    }
                }
            }
        }
    }
}

/// Draws the inner shadows of `text` over `canvas`, the last first: the outside of the coverage
/// `fill`, moved and blurred, kept where the glyphs are.
pub(crate) fn draw_inner_shadows(canvas: &mut [[f32; 4]], text: &TextContent, fill: &Mask, painter: &Painter) {
    let font_px = painter.font_px;
    for shadow in text.inner_shadows.iter().rev() {
        let [ox, oy] = shadow.offset();
        let reach = font_px * engine::MAX_TEXT_SHADOW_DISTANCE;
        let (dx, dy) = if ox.is_finite() && oy.is_finite() { ((ox * font_px).clamp(-reach, reach).round() as i32, (oy * font_px).clamp(-reach, reach).round() as i32) } else { (0, 0) };
        let blur = if shadow.blur.is_finite() { (shadow.blur * font_px).clamp(0.0, font_px * engine::MAX_TEXT_SHADOW_BLUR) } else { 0.0 };
        // The glyphs moved and blurred; past the picture's edge there are none.
        let mut mask = fill.shifted(dx, dy);
        mask.blur(blur);
        for (v, &a) in mask.data.iter_mut().zip(&fill.data) {
            *v = (1.0 - *v) * a;
        }
        painter.draw(canvas, &mask, &shadow.paint);
    }
}

/// Blurs a premultiplied colour picture as [`Mask::blur`] blurs a mask.
fn blur_colors(layer: &mut [[f32; 4]], width: usize, height: usize, radius: f32) {
    if super::blur_half(radius) == 0 {
        return;
    }
    let mut channel = Mask::new(width, height);
    for c in 0..4 {
        for (v, p) in channel.data.iter_mut().zip(layer.iter()) {
            *v = p[c];
        }
        channel.blur(radius);
        for (v, p) in channel.data.iter().zip(layer.iter_mut()) {
            p[c] = *v;
        }
    }
}
