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
/// The widest or tallest texture picture used; a larger one is not drawn. 2048 is sharp enough
/// for a texture filling glyphs (a text box rarely spans more than a 1080p frame) and keeps one
/// picture at 16 MiB of RGBA8.
pub const MAX_TEXTURE_DIMENSION: u32 = 2048;
/// The most bytes of decoded texture pictures kept; past it the least recently used are dropped.
pub const MAX_TEXTURE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// A decoded texture picture, straight RGBA8; premultiplied when sampled.
pub(crate) struct Picture {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 4]>,
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
        let pixels = decoded.pixels().map(|p| p.0).collect();
        Some(Picture { width: decoded.width() as usize, height: decoded.height() as usize, pixels })
    }

    /// The bytes the decoded pixels take.
    pub(crate) fn bytes(&self) -> usize {
        self.pixels.len() * std::mem::size_of::<[u8; 4]>()
    }

    /// The colour at `(u, v)` in picture pixels, bilinear, with the picture repeated (mirrored
    /// every other copy when `mirror`) past its edges.
    fn sample(&self, u: f32, v: f32, mirror: bool) -> [f32; 4] {
        self.sample_cell(0, self.width, u, v, mirror)
    }

    /// Like [`Self::sample`] within the columns `cell_x..cell_x + cell_width` of the picture, repeated past
    /// their edges: `u` counts from `cell_x`.
    fn sample_cell(&self, cell_x: usize, cell_width: usize, u: f32, v: f32, mirror: bool) -> [f32; 4] {
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
        let at = |i: i64, j: i64| premultiplied(self.pixels[wrap(j, self.height) * self.width + cell_x + wrap(i, cell_width)]);
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
        self.0.iter().map(|(_, picture)| picture.bytes()).sum()
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

/// Whether `path` names a network share or a device (`\\server\share`, `//server/share`,
/// `\\?\UNC\...`, `\\.\...`) rather than a local disk. Opening one can reach out to another
/// machine, so such a file is neither read for a texture nor accepted from an agent.
pub fn is_network_or_device_path(path: &str) -> bool {
    use std::path::{Component, Prefix};
    if let Some(Component::Prefix(prefix)) = Path::new(path).components().next() {
        return !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_));
    }
    // Off Windows there is no prefix, but both spellings still start a share there.
    path.starts_with(r"\\") || path.starts_with("//")
}

/// Whether a texture file named `path` may be read: an absolute path on a local disk. A relative
/// path is resolved against the project folder before it gets here, so one left relative is not
/// read against the working folder.
pub fn is_readable_texture_path(path: &str) -> bool {
    !is_network_or_device_path(path) && Path::new(path).is_absolute()
}

/// The stamps of the texture files `text` paints with (`None` for a picture that is not a file or
/// cannot be read), in the order of [`text_images`]. Goes into the keys of cached pictures, so a
/// changed file is drawn again.
pub fn text_texture_stamps(text: &TextContent) -> Vec<Option<FontFileStamp>> {
    text_images(text)
        .into_iter()
        .map(|image| match image {
            TextImageRef::File { path } if is_readable_texture_path(path) => font_file_stamp(Path::new(path)),
            _ => None,
        })
        .collect()
}

/// Decoded texture pictures by path, with the stamp of the file they were read from (`None` for a
/// file that is not a picture) and when each was last used.
#[derive(Default)]
pub(crate) struct TextureCache {
    files: HashMap<PathBuf, CachedTexture>,
    clock: u64,
}

struct CachedTexture {
    stamp: Option<FontFileStamp>,
    picture: Option<Arc<Picture>>,
    used: u64,
}

/// The most texture files kept known, pictures or not.
const MAX_CACHED_TEXTURES: usize = 64;

impl TextureCache {
    /// The pictures `text` paints with, and the first one that could not be drawn (a pack picture
    /// the caller did not resolve to its file, a missing file, or one that is not a picture).
    pub(crate) fn textures_for(&mut self, text: &TextContent) -> (Textures, Option<String>) {
        let mut textures = Textures::default();
        let mut missing = None;
        for image in text_images(text) {
            let picture = match image {
                TextImageRef::File { path } if is_readable_texture_path(path) => self.load(Path::new(path)),
                _ => None,
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
        self.clock += 1;
        let stamp = font_file_stamp(path);
        if let Some(known) = self.files.get_mut(path) {
            if known.stamp == stamp {
                known.used = self.clock;
                return known.picture.clone();
            }
        }
        let picture = stamp.and_then(|_| Picture::load(path)).map(Arc::new);
        self.files.insert(path.to_owned(), CachedTexture { stamp, picture: picture.clone(), used: self.clock });
        self.evict(path);
        picture
    }

    /// The bytes of the pictures kept.
    pub(crate) fn bytes(&self) -> usize {
        self.files.values().filter_map(|entry| entry.picture.as_ref()).map(|picture| picture.bytes()).sum()
    }

    /// Drops the least recently used entries, never `keep`, until the cache fits its limits.
    fn evict(&mut self, keep: &Path) {
        let mut total = self.bytes();
        while self.files.len() > MAX_CACHED_TEXTURES || total > MAX_TEXTURE_CACHE_BYTES {
            let Some(oldest) = self.files.iter().filter(|(path, _)| path.as_path() != keep).min_by_key(|(_, entry)| entry.used).map(|(path, _)| path.clone()) else { break };
            if let Some(gone) = self.files.remove(&oldest) {
                total -= gone.picture.map_or(0, |picture| picture.bytes());
            }
        }
    }
}

/// Where the paints are laid: the box of the whole text in the picture being drawn, the boxes of
/// its glyphs, the font size and the pictures of the textures.
#[derive(Clone, Copy)]
pub(crate) struct Painter<'a> {
    pub textures: &'a Textures,
    /// The line boxes of the text (x0, y0, x1, y1), in the pixels of the picture drawn into.
    pub rect: (f32, f32, f32, f32),
    /// The ink box of each glyph with ink (x0, y0, x1, y1), around the top left of `rect`, in
    /// the order of the text: what per-glyph gradients and [`TextureFit::Glyph`] lie over.
    pub glyphs: &'a [(f32, f32, f32, f32)],
    /// How far the glyph boxes are grown on each side, in pixels: the width of the outline being
    /// painted, so a per-glyph paint covers the outline around its glyph as CapCut lays it.
    pub grow: f32,
    pub font_px: f32,
}

impl<'a> Painter<'a> {
    /// The same paints for a picture whose pixel `(0, 0)` is pixel `(dx, dy)` of this one's.
    pub fn moved(&self, dx: f32, dy: f32) -> Painter<'a> {
        let (x0, y0, x1, y1) = self.rect;
        Painter { rect: (x0 - dx, y0 - dy, x1 - dx, y1 - dy), ..*self }
    }

    /// The same paints with the glyph boxes grown by `grow` pixels on each side.
    pub fn grown(&self, grow: f32) -> Painter<'a> {
        Painter { grow, ..*self }
    }

    /// The premultiplied colour of `paint` at the centre of pixel `(x, y)`.
    pub fn color_at(&self, paint: &TextPaint, x: usize, y: usize) -> [f32; 4] {
        self.paint_at(paint, x as f32 + 0.5, y as f32 + 0.5)
    }

    fn paint_at(&self, paint: &TextPaint, px: f32, py: f32) -> [f32; 4] {
        match paint {
            TextPaint::Solid { color } => premultiplied(*color),
            TextPaint::Gradient { stops, angle, per_glyph } => {
                let rect = if *per_glyph { self.glyph_at(px, py).1 } else { self.rect };
                gradient_color(stops, gradient_t(rect, *angle, px, py))
            }
            TextPaint::Texture(texture) => {
                let color = self.texture_color(texture, px, py);
                match texture.base.as_deref() {
                    // A texture under a texture is refused by validation; drawn as none.
                    Some(base) if !matches!(base, TextPaint::Texture(_)) => {
                        let under = self.paint_at(base, px, py);
                        std::array::from_fn(|c| color[c] + under[c] * (1.0 - color[3]))
                    }
                    _ => color,
                }
            }
        }
    }

    /// The glyph whose ink box holds `(px, py)`, else the nearest one: its number among the glyphs
    /// with ink and its box in the picture. The whole text box when there is no glyph.
    fn glyph_at(&self, px: f32, py: f32) -> (usize, (f32, f32, f32, f32)) {
        let (ox, oy, _, _) = self.rect;
        let mut best = (f32::INFINITY, 0, self.rect);
        for (number, &(x0, y0, x1, y1)) in self.glyphs.iter().enumerate() {
            let g = self.grow;
            let rect = (ox + x0 - g, oy + y0 - g, ox + x1 + g, oy + y1 + g);
            let dx = (rect.0 - px).max(px - rect.2).max(0.0);
            let dy = (rect.1 - py).max(py - rect.3).max(0.0);
            let far = dx * dx + dy * dy;
            if far < best.0 {
                best = (far, number, rect);
                if far == 0.0 {
                    break;
                }
            }
        }
        (best.1, best.2)
    }
}

/// Where `(px, py)` lies along a gradient running `angle` degrees clockwise from left to right
/// across `rect`: 0 at the box corner it starts from, 1 at the one it ends at.
fn gradient_t(rect: (f32, f32, f32, f32), angle: f32, px: f32, py: f32) -> f32 {
    let (x0, y0, x1, y1) = rect;
    let radians = if angle.is_finite() { angle.to_radians() } else { 0.0 };
    let (dx, dy) = (radians.cos(), radians.sin());
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let half = ((x1 - x0) * dx.abs() + (y1 - y0) * dy.abs()) / 2.0;
    if half <= 0.0 {
        return 0.0;
    }
    (((px - cx) * dx + (py - cy) * dy) / (2.0 * half) + 0.5).clamp(0.0, 1.0)
}

impl<'a> Painter<'a> {
    fn texture_color(&self, texture: &TextTexture, px: f32, py: f32) -> [f32; 4] {
        let TextImageRef::File { path } = &texture.image else { return [0.0; 4] };
        let Some(picture) = self.textures.get(path) else { return [0.0; 4] };
        let scale = if texture.scale.is_finite() { texture.scale.clamp(*engine::TEXT_TEXTURE_SCALE_RANGE.start(), *engine::TEXT_TEXTURE_SCALE_RANGE.end()) } else { 1.0 };
        let opacity = if texture.opacity.is_finite() { texture.opacity.clamp(0.0, 1.0) } else { 1.0 };
        if texture.fit == TextureFit::Glyph {
            // One cell per glyph, in turn, covering the glyph's box and centred on it.
            let (number, (x0, y0, x1, y1)) = self.glyph_at(px, py);
            let cells = texture.cells.clamp(1, engine::MAX_TEXTURE_CELLS) as usize;
            let cell_width = (picture.width / cells).max(1);
            let cell = number % cells;
            let (cw, ih) = (cell_width as f32, picture.height as f32);
            let zoom = ((x1 - x0) / cw).max((y1 - y0) / ih).max(1e-3) * scale;
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let (qx, qy) = turned(texture.angle, (cx, cy), px, py);
            let mut u = (qx - cx) / zoom + cw / 2.0;
            let mut v = (qy - cy) / zoom + ih / 2.0;
            if texture.flip_x {
                u = cw - u;
            }
            if texture.flip_y {
                v = ih - v;
            }
            let x0 = (cell * cell_width).min(picture.width - cell_width);
            return picture.sample_cell(x0, cell_width, u, v, false).map(|c| c * opacity);
        }
        let (x0, y0, x1, y1) = self.rect;
        let (iw, ih) = (picture.width as f32, picture.height as f32);
        // Screen pixels per picture pixel, and the screen point picture pixel (0, 0) lands on.
        let (zoom, anchor) = match texture.fit {
            TextureFit::Stretch => {
                let zoom = ((x1 - x0) / iw).max((y1 - y0) / ih).max(1e-3) * scale;
                (zoom, ((x0 + x1) / 2.0 - iw * zoom / 2.0, (y0 + y1) / 2.0 - ih * zoom / 2.0))
            }
            TextureFit::Tile | TextureFit::Mirror | TextureFit::Glyph => ((scale * self.font_px / ih).max(1e-3), (x0, y0)),
        };
        // Turn the point back around the text centre by the picture's turn.
        let (qx, qy) = turned(texture.angle, ((x0 + x1) / 2.0, (y0 + y1) / 2.0), px, py);
        let mut u = (qx - anchor.0) / zoom;
        let mut v = (qy - anchor.1) / zoom;
        if texture.flip_x {
            u = iw - u;
        }
        if texture.flip_y {
            v = ih - v;
        }
        let color = picture.sample(u, v, texture.fit == TextureFit::Mirror);
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

/// `(px, py)` turned back around `centre` by a picture turned `angle` degrees clockwise.
fn turned(angle: f32, centre: (f32, f32), px: f32, py: f32) -> (f32, f32) {
    let radians = if angle.is_finite() { -angle.to_radians() } else { 0.0 };
    let (s, c) = radians.sin_cos();
    let (rx, ry) = (px - centre.0, py - centre.1);
    (centre.0 + rx * c - ry * s, centre.1 + rx * s + ry * c)
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
///
/// As in CapCut, an outline is a ring around the glyphs, from their edge out to its width: it never
/// covers the inside of the glyphs, which shows the paint drawn there next (the fill of the text,
/// the paint of a shadow) or, when that paint is transparent, whatever lies under the layer. So a
/// shadow at distance 0 with a transparent paint and outlines draws only rings, as CapCut packs
/// often use it. The rings are drawn so that the paint of the glyphs, put over them next with the
/// coverage `fill`, meets them without a seam along the anti-aliased edge.
pub(crate) fn draw_outlines(canvas: &mut [[f32; 4]], fill: &Mask, squared: &[f64], outlines: &[TextOutline], painter: &Painter) {
    let mut sorted: Vec<(f32, &TextPaint)> = outlines.iter().map(|outline| (outline_px(outline.width, painter.font_px), &outline.paint)).filter(|(width, _)| *width > 0.0).collect();
    sorted.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (width, paint) in sorted {
        let mut ring = fill.dilated(squared, width);
        // Coverage of the ring alone is `grown - f`; dividing by `1 - f` makes the fill drawn over
        // it with coverage `f` add up to `grown` at the edge, as two shapes side by side would.
        for (r, &f) in ring.data.iter_mut().zip(&fill.data) {
            *r = if f >= 1.0 { 0.0 } else { ((*r - f) / (1.0 - f)).clamp(0.0, 1.0) };
        }
        painter.grown(width).draw(canvas, &ring, paint);
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

/// Draws the inner shadows of `text` over `canvas`, the last first: as in CapCut, the paint covers
/// the glyphs where they overlap themselves moved by the shadow's offset (and blurred), so a rim of
/// the fill stays bare along the edge facing away from the offset, as if lit from that side.
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
            *v *= a;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 255])).save(&path).unwrap();
        path
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-text-paint-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn texture_files_on_a_network_share_or_relative_are_not_read() {
        let dir = temp_dir("paths");
        let local = write_png(&dir, "a.png", 4, 4);
        let mut text = TextContent::new("Hi", 0.1);
        let texture = |path: &str| TextPaint::Texture(TextTexture::new(TextImageRef::File { path: path.to_owned() }));
        let mut cache = TextureCache::default();
        text.fill = texture(&local.to_string_lossy());
        assert_eq!(cache.textures_for(&text).1, None);
        for path in [r"\\server\share\a.png", "//server/share/a.png", r"\\?\UNC\server\share\a.png", r"\\.\pipe\a", "textures/a.png"] {
            text.fill = texture(path);
            assert_eq!(cache.textures_for(&text).1.as_deref(), Some(path), "{path}");
            assert_eq!(text_texture_stamps(&text), vec![None], "{path}");
        }
        // A picture that is there, named relative to the working folder, is still not read.
        let relative = format!("gpufx-text-paint-relative-{}.png", std::process::id());
        write_png(&std::env::current_dir().unwrap(), &relative, 4, 4);
        text.fill = texture(&relative);
        let (missing, stamps) = (cache.textures_for(&text).1, text_texture_stamps(&text));
        std::fs::remove_file(&relative).ok();
        assert_eq!((missing.as_deref(), stamps), (Some(relative.as_str()), vec![None]));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pictures_are_kept_as_rgba8() {
        let dir = temp_dir("rgba8");
        let picture = Picture::load(&write_png(&dir, "a.png", 8, 4)).unwrap();
        assert_eq!(picture.bytes(), 8 * 4 * 4);
        let color = picture.sample(2.5, 1.5, false);
        assert!((color[0] - 200.0 / 255.0).abs() < 1e-4 && (color[3] - 1.0).abs() < 1e-6);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pictures_wider_than_the_texture_limit_are_not_drawn() {
        let dir = temp_dir("limit");
        assert!(Picture::load(&write_png(&dir, "edge.png", MAX_TEXTURE_DIMENSION, 1)).is_some());
        assert!(Picture::load(&write_png(&dir, "wide.png", 3000, 1)).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_cache_drops_the_least_recently_used_pictures_past_its_byte_limit() {
        let dir = temp_dir("lru");
        // Each picture is 16 MiB; the limit holds four.
        let paths: Vec<PathBuf> = (0..6).map(|i| write_png(&dir, &format!("{i}.png"), 2048, 2048)).collect();
        let mut cache = TextureCache::default();
        for path in &paths[..4] {
            assert!(cache.load(path).is_some());
        }
        assert!(cache.load(&paths[0]).is_some()); // 0 is now the most recent, 1 the oldest
        assert!(cache.load(&paths[4]).is_some());
        assert!(cache.bytes() <= MAX_TEXTURE_CACHE_BYTES);
        assert!(cache.files.contains_key(&paths[0]) && !cache.files.contains_key(&paths[1]));
        assert!(cache.load(&paths[5]).is_some());
        assert!(cache.bytes() <= MAX_TEXTURE_CACHE_BYTES && !cache.files.contains_key(&paths[2]));
        std::fs::remove_dir_all(dir).ok();
    }
}
