//! Rasterizes the text of a text clip into a premultiplied RGBA picture on the CPU.
//!
//! Shaping, bidi and font fallback come from `cosmic-text`, glyph pictures from its swash cache.
//! Outline, background box and drop shadow are drawn here on alpha masks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use cosmic_text::{Attrs, Buffer, Command, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Wrap};
use engine::project::{FontRef, Rgba, TextAlign, TextContent};

/// The installed families tried, in order, for the default font: Segoe UI on Windows (it has the
/// Vietnamese letters), then Arial. No font ships with the editor.
pub const DEFAULT_FAMILY_CANDIDATES: [&str; 2] = ["Segoe UI", "Arial"];

/// When a font file was last written and how long it is; a new value means the file changed.
pub type FontFileStamp = (SystemTime, u64);

/// The stamp of a font file, or `None` when it cannot be read.
pub fn font_file_stamp(path: &Path) -> Option<FontFileStamp> {
    let meta = std::fs::metadata(path).ok().filter(|meta| meta.is_file())?;
    Some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()))
}

/// The stamp of the font file `text` uses, `None` for a font that is not a file. Goes into the
/// keys of cached pictures, so a changed file is drawn again.
pub fn text_font_stamp(text: &TextContent) -> Option<FontFileStamp> {
    match &text.font {
        FontRef::File { path } => font_file_stamp(Path::new(path)),
        _ => None,
    }
}

/// The largest font file read, in bytes; a larger one is taken as not a font. Real fonts, even
/// large CJK collections, stay well below it.
pub const MAX_FONT_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// The bytes of the font file at `path`, or `None` when it cannot be read or is larger than
/// [`MAX_FONT_FILE_BYTES`], which is checked before reading.
fn read_font_file(path: &Path) -> Option<Vec<u8>> {
    let (_, len) = font_file_stamp(path)?;
    if len > MAX_FONT_FILE_BYTES {
        eprintln!("gpufx-text: {} is larger than {MAX_FONT_FILE_BYTES} bytes, not read as a font", path.display());
        return None;
    }
    std::fs::read(path).ok()
}

/// The family of the first face of the font file at `path`, or `None` when it is missing, larger
/// than [`MAX_FONT_FILE_BYTES`] or not a font. Reads the file without loading it into any
/// rasterizer.
pub fn font_file_family(path: &Path) -> Option<String> {
    let mut db = fontdb::Database::new();
    let data = read_font_file(path)?;
    let ids: Vec<fontdb::ID> = db.load_font_source(fontdb::Source::Binary(Arc::new(data))).into_iter().collect();
    first_family(&db, &ids)
}

fn first_family(db: &fontdb::Database, ids: &[fontdb::ID]) -> Option<String> {
    ids.iter().filter_map(|id| db.face(*id)).filter_map(|face| face.families.first()).map(|(name, _)| name.trim().to_owned()).find(|name| !name.is_empty())
}

/// The widest or tallest picture [`TextRasterizer::rasterize`] returns, in pixels. Larger text is
/// drawn smaller so that it fits.
pub const MAX_DIMENSION: u32 = 8192;
/// The most pixels a returned picture may hold (64 MiB of RGBA).
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;
/// The widest an outline may be, as a part of the font size. Wider would bury the letters, and
/// the room around the text grows with it.
pub const MAX_OUTLINE_RATIO: f32 = 0.25;

/// A rasterized text picture.
#[derive(Debug, Clone, PartialEq)]
pub struct TextImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` pixels, row by row from the top, as premultiplied RGBA.
    pub pixels: Vec<u8>,
    /// The system font family, or the path of a font file, that was asked for and not found or
    /// not readable, so the default font drew it.
    pub missing_family: Option<String>,
    /// No font is loaded at all, so nothing could be drawn and the picture is empty.
    pub no_font: bool,
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
    /// The family [`FontRef::Default`] draws with, or `None` when no font is loaded.
    default_family: Option<String>,
    /// Font files loaded for [`FontRef::File`], by path.
    files: HashMap<PathBuf, LoadedFile>,
    /// How many font files have been loaded, which numbers their [`LoadedFile::select_name`].
    file_loads: u64,
}

/// A font file as loaded: the stamp it had, its faces and its family (`None` when not a font).
struct LoadedFile {
    stamp: Option<FontFileStamp>,
    ids: Vec<fontdb::ID>,
    family: Option<String>,
    /// The family name its faces are renamed to in the font database, unique to this load, so
    /// that a system font or another file with the same family is never picked in its place.
    select_name: String,
}

/// A family to shape with, and the weight, style and stretch to ask for.
struct FaceChoice {
    name: String,
    weight: cosmic_text::Weight,
    style: cosmic_text::Style,
    stretch: cosmic_text::Stretch,
}

impl FaceChoice {
    /// A family at the normal weight, style and stretch.
    fn family(name: String) -> FaceChoice {
        FaceChoice { name, weight: cosmic_text::Weight::NORMAL, style: cosmic_text::Style::Normal, stretch: cosmic_text::Stretch::Normal }
    }
}

/// Starts the family name a loaded font file's faces get, see [`LoadedFile::select_name`].
const FILE_FAMILY_PREFIX: &str = "gpufx-file:";

fn is_file_family(name: &str) -> bool {
    name.starts_with(FILE_FAMILY_PREFIX)
}

impl Default for TextRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

/// The rasterizer with the system fonts, built the first time it is asked for and then shared by
/// everyone, since building one scans every installed font.
pub fn shared() -> Arc<Mutex<TextRasterizer>> {
    SHARED.get_or_init(|| Arc::new(Mutex::new(TextRasterizer::new()))).clone()
}

static SHARED: OnceLock<Arc<Mutex<TextRasterizer>>> = OnceLock::new();

/// The shared rasterizer if [`shared`] has finished building it, without waiting for the font scan.
pub fn shared_if_ready() -> Option<Arc<Mutex<TextRasterizer>>> {
    SHARED.get().cloned()
}

impl TextRasterizer {
    /// Every font installed on the system; the default font is one of them (see [`Self::default_family`]).
    pub fn new() -> TextRasterizer {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self::with_db(db)
    }

    /// Only the fonts in these files, without the system ones. An empty list loads no font, so
    /// every text comes out empty with [`TextImage::no_font`] set.
    pub fn with_font_files(files: &[PathBuf]) -> TextRasterizer {
        let mut db = fontdb::Database::new();
        for file in files {
            if let Err(error) = db.load_font_file(file) {
                eprintln!("gpufx-text: cannot load font {}: {error}", file.display());
            }
        }
        Self::with_db(db)
    }

    fn with_db(mut db: fontdb::Database) -> TextRasterizer {
        let default_family = choose_default_family(&db);
        match &default_family {
            Some(family) => db.set_sans_serif_family(family.clone()),
            None => eprintln!("gpufx-text: no font is installed; text cannot be drawn"),
        }
        let locale = sys_locale();
        TextRasterizer { font_system: FontSystem::new_with_locale_and_db(locale, db), cache: SwashCache::new(), default_family, files: HashMap::new(), file_loads: 0 }
    }

    /// The installed family [`FontRef::Default`] draws with: the first of
    /// [`DEFAULT_FAMILY_CANDIDATES`] that is installed, else the first family in name order.
    /// `None` when no font is loaded.
    pub fn default_family(&self) -> Option<&str> {
        self.default_family.as_deref()
    }

    /// Whether a font family with this name is loaded, ignoring case.
    pub fn has_family(&self, family: &str) -> bool {
        !is_file_family(family) && self.font_system.db().faces().any(|face| face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(family)))
    }

    /// The family the font file at `path` gives, loading it the first time and again when it
    /// changed. `None` when the file is missing or is not a font.
    pub fn file_family(&mut self, path: &Path) -> Option<String> {
        self.load_file(path).family.clone()
    }

    /// What selects the first face of the font file at `path` and nothing else: its unique
    /// family name with that face's weight, style and stretch (cosmic-text skips a face whose
    /// weight differs from the one asked for). `None` when it is missing or not a font.
    fn file_face(&mut self, path: &Path) -> Option<FaceChoice> {
        let loaded = self.load_file(path);
        loaded.family.as_ref()?;
        let (name, id) = (loaded.select_name.clone(), *loaded.ids.first()?);
        let face = self.font_system.db().face(id)?;
        Some(FaceChoice { name, weight: face.weight, style: face.style, stretch: face.stretch })
    }

    /// The font file at `path` as loaded, loading it the first time and again when it changed.
    fn load_file(&mut self, path: &Path) -> &LoadedFile {
        let stamp = font_file_stamp(path);
        if self.files.get(path).is_some_and(|loaded| loaded.stamp == stamp) {
            return &self.files[path];
        }
        if let Some(old) = self.files.remove(path) {
            let db = self.font_system.db_mut();
            for id in old.ids {
                db.remove_face(id);
            }
        }
        self.file_loads += 1;
        // A fresh name per load, so a font match cosmic-text cached for an older load of the
        // file is never reused.
        let select_name = format!("{FILE_FAMILY_PREFIX}{}", self.file_loads);
        let data = stamp.and_then(|_| read_font_file(path));
        let (ids, family) = match data {
            Some(data) => {
                let db = self.font_system.db_mut();
                let loaded: Vec<fontdb::ID> = db.load_font_source(fontdb::Source::Binary(Arc::new(data))).into_iter().collect();
                let family = first_family(db, &loaded);
                if family.is_none() {
                    eprintln!("gpufx-text: {} is not a font", path.display());
                }
                // fontdb has no way to change a face in place, so each face goes back in under
                // the unique name.
                let mut ids = Vec::with_capacity(loaded.len());
                for id in loaded {
                    let Some(mut info) = db.face(id).cloned() else { continue };
                    db.remove_face(id);
                    info.families = vec![(select_name.clone(), fontdb::Language::English_UnitedStates)];
                    info.id = fontdb::ID::dummy();
                    ids.push(db.push_face_info(info));
                }
                (ids, family)
            }
            None => (Vec::new(), None),
        };
        self.files.insert(path.to_owned(), LoadedFile { stamp, ids, family, select_name });
        &self.files[path]
    }

    /// The font family or font file `text` asks for that cannot be used, so the default font
    /// draws it; `None` for the default font or one that is there.
    pub fn missing_font(&mut self, text: &TextContent) -> Option<String> {
        match &text.font {
            FontRef::Default => None,
            FontRef::System { family } => (!self.has_family(family)).then(|| family.clone()),
            FontRef::File { path } => self.file_family(Path::new(path)).is_none().then(|| path.clone()),
        }
    }

    /// Every loaded font family once, sorted ignoring case. Empty names are left out.
    pub fn families(&self) -> Vec<String> {
        sorted_families(self.font_system.db())
    }

    /// Draws `text` for an output `output_height_px` pixels tall, `scale` times as large (such as
    /// below 1 for a smaller preview). Empty text, or a size that comes to no pixels, gives a
    /// 0 by 0 picture. The size is decided from the layout first, so a picture over the limits is
    /// never drawn at full size.
    pub fn rasterize(&mut self, text: &TextContent, output_height_px: u32, scale: f32) -> TextImage {
        let font_px = text.size * output_height_px as f32 * scale;
        let mut layout = self.layout(text, font_px);
        let mut downscaled = false;
        // Everything scales with the font size, so shrink it by the ratio that fits; the margins
        // round up, so check again and shrink a little more while they still do not fit.
        for _ in 0..8 {
            let (w, h) = (layout.width as u64, layout.height as u64);
            if w <= MAX_DIMENSION as u64 && h <= MAX_DIMENSION as u64 && w * h <= MAX_PIXELS {
                break;
            }
            let ratio = (MAX_DIMENSION as f64 / w as f64).min(MAX_DIMENSION as f64 / h as f64).min((MAX_PIXELS as f64 / (w * h) as f64).sqrt()) * 0.98;
            layout = self.layout(text, layout.font_px * ratio as f32);
            downscaled = true;
        }
        let mut image = self.draw(text, &layout);
        image.downscaled = downscaled;
        image
    }

    /// The width and height [`Self::rasterize`] would give before shrinking to the limits, so
    /// either may pass [`MAX_DIMENSION`]. Shapes the text but draws nothing.
    pub fn measure(&mut self, text: &TextContent, output_height_px: u32, scale: f32) -> (u32, u32) {
        let layout = self.layout(text, text.size * output_height_px as f32 * scale);
        (layout.width, layout.height)
    }

    /// Shapes `text` at `font_px` and places it: glyph positions and the size of the picture.
    fn layout(&mut self, text: &TextContent, font_px: f32) -> Layout {
        let mut layout = Layout::default();
        let asked = match &text.font {
            FontRef::Default => None,
            FontRef::System { family } if self.has_family(family) => Some(FaceChoice::family(family.clone())),
            FontRef::System { family } => {
                layout.missing_family = Some(family.clone());
                None
            }
            FontRef::File { path } => {
                let face = self.file_face(Path::new(path));
                if face.is_none() {
                    layout.missing_family = Some(path.clone());
                }
                face
            }
        };
        let Some(face) = asked.or_else(|| self.default_family.clone().map(FaceChoice::family)) else {
            layout.no_font = true;
            return layout;
        };
        if text.content.is_empty() || !font_px.is_finite() || font_px < 0.5 {
            return layout;
        }
        let font_px = font_px.min(MAX_DIMENSION as f32);
        layout.font_px = font_px;
        let line_spacing = if text.line_spacing.is_finite() && text.line_spacing > 0.0 { text.line_spacing } else { 1.0 };
        let line_height = (font_px * 1.25 * line_spacing).max(1.0);

        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_px, line_height));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        let attrs = Attrs::new().family(Family::Name(&face.name)).weight(face.weight).style(face.style).stretch(face.stretch);
        buffer.set_text(&text.content, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Glyph positions and the size of the line boxes.
        let mut text_w = 0f32;
        let mut text_h = 0f32;
        let mut lines = Vec::new();
        for run in buffer.layout_runs() {
            text_w = text_w.max(run.line_w);
            text_h = text_h.max(run.line_top + run.line_height);
            lines.push((run.line_w, run.line_y, run.glyphs.to_vec()));
        }
        // The ink may leave the line boxes (marks stacked above a letter on a tight line spacing,
        // a glyph that starts left of its origin), so the picture holds both.
        let (mut x0, mut y0, mut x1, mut y1) = (0f32, 0f32, text_w, text_h);
        for (line_w, line_y, run_glyphs) in lines {
            let shift = match text.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (text_w - line_w) / 2.0,
                TextAlign::Right => text_w - line_w,
            };
            for glyph in run_glyphs {
                if glyph.glyph_id == 0 {
                    layout.missing_glyphs += 1;
                }
                let physical = glyph.physical((shift, line_y), 1.0);
                if let Some((gx0, gy0, gx1, gy1)) = self.ink_bounds(physical.cache_key) {
                    // One pixel more on each side for the sub-pixel offset and anti-aliasing.
                    x0 = x0.min(physical.x as f32 + gx0 - 1.0);
                    x1 = x1.max(physical.x as f32 + gx1 + 1.0);
                    y0 = y0.min(physical.y as f32 + gy0 - 1.0);
                    y1 = y1.max(physical.y as f32 + gy1 + 1.0);
                }
                layout.glyphs.push(physical);
            }
        }

        // Room around the text for the outline, the box and the shadow.
        let outline_px = text.outline.map(|o| (o.width * font_px).clamp(0.0, font_px * MAX_OUTLINE_RATIO)).unwrap_or(0.0);
        let pad_px = text.background.map(|b| (b.padding * font_px).clamp(0.0, font_px * 4.0)).unwrap_or(0.0);
        let (shadow_dx, shadow_dy, shadow_blur) = text.shadow.map(|s| ((s.offset[0] * font_px).clamp(-font_px * 4.0, font_px * 4.0), (s.offset[1] * font_px).clamp(-font_px * 4.0, font_px * 4.0), (s.blur * font_px).clamp(0.0, font_px * 2.0))).unwrap_or((0.0, 0.0, 0.0));
        // A blur too small to move a pixel is no blur, and needs no room.
        let shadow_blur = if blur_half(shadow_blur) == 0 { 0.0 } else { shadow_blur };
        let blur_room = if text.shadow.is_some() { shadow_blur.ceil() * 2.0 } else { 0.0 };
        let (x0, y0, x1, y1) = (x0.floor(), y0.floor(), x1.ceil(), y1.ceil());
        let base = outline_px.max(pad_px).ceil() + 1.0;
        let left = base + (-shadow_dx).max(0.0).ceil() + blur_room;
        let right = base + shadow_dx.max(0.0).ceil() + blur_room;
        let top = base + (-shadow_dy).max(0.0).ceil() + blur_room;
        let bottom = base + shadow_dy.max(0.0).ceil() + blur_room;
        layout.width = (left + (x1 - x0) + right) as u32;
        layout.height = (top + (y1 - y0) + bottom) as u32;
        layout.origin = (left - x0, top - y0);
        layout.text_size = (text_w, text_h);
        layout.outline_px = outline_px;
        layout.pad_px = pad_px;
        layout.shadow = (shadow_dx, shadow_dy, shadow_blur);
        layout
    }

    /// The box of a glyph's outline around its origin, y down, in pixels. A glyph with no
    /// outline, such as a bitmap emoji, is measured by its picture.
    fn ink_bounds(&mut self, key: cosmic_text::CacheKey) -> Option<(f32, f32, f32, f32)> {
        if let Some(commands) = self.cache.get_outline_commands_uncached(&mut self.font_system, key) {
            let mut bounds: Option<(f32, f32, f32, f32)> = None;
            // Outline y points up. Control points hold the curves, so their box holds the glyph.
            let mut add = |x: f32, y: f32| {
                let y = -y;
                bounds = Some(bounds.map_or((x, y, x, y), |(a, b, c, d)| (a.min(x), b.min(y), c.max(x), d.max(y))));
            };
            for command in commands.iter() {
                match *command {
                    Command::MoveTo(p) | Command::LineTo(p) => add(p.x, p.y),
                    Command::QuadTo(a, b) => {
                        add(a.x, a.y);
                        add(b.x, b.y);
                    }
                    Command::CurveTo(a, b, c) => {
                        add(a.x, a.y);
                        add(b.x, b.y);
                        add(c.x, c.y);
                    }
                    Command::Close => {}
                }
            }
            return bounds;
        }
        let picture = self.cache.get_image_uncached(&mut self.font_system, key)?;
        let p = picture.placement;
        Some((p.left as f32, -p.top as f32, (p.left + p.width as i32) as f32, (p.height as i32 - p.top) as f32))
    }

    /// Draws a layout from [`Self::layout`]. One over the limits draws nothing.
    fn draw(&mut self, text: &TextContent, layout: &Layout) -> TextImage {
        let mut image = TextImage { width: layout.width, height: layout.height, pixels: Vec::new(), missing_family: layout.missing_family.clone(), no_font: layout.no_font, missing_glyphs: layout.missing_glyphs, downscaled: false };
        let (width, height) = (layout.width as usize, layout.height as usize);
        if width == 0 || height == 0 {
            return image;
        }
        if width > MAX_DIMENSION as usize || height > MAX_DIMENSION as usize || (width as u64) * (height as u64) > MAX_PIXELS {
            image.width = 0;
            image.height = 0;
            return image;
        }
        let font_px = layout.font_px;
        let (left, top) = layout.origin;
        let (text_w, text_h) = layout.text_size;
        let (outline_px, pad_px) = (layout.outline_px, layout.pad_px);
        let (shadow_dx, shadow_dy, shadow_blur) = layout.shadow;

        // Glyph coverage.
        let mut fill = Mask::new(width, height);
        let (ox, oy) = (left as i32, top as i32);
        for glyph in &layout.glyphs {
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

/// A shaped and placed text, ready to draw.
#[derive(Default)]
struct Layout {
    font_px: f32,
    glyphs: Vec<cosmic_text::PhysicalGlyph>,
    width: u32,
    height: u32,
    /// Where the top left of the line boxes lands in the picture.
    origin: (f32, f32),
    /// The size of the line boxes, which the background box goes around.
    text_size: (f32, f32),
    outline_px: f32,
    pad_px: f32,
    /// Shadow offset x, y and blur, in pixels.
    shadow: (f32, f32, f32),
    missing_family: Option<String>,
    no_font: bool,
    missing_glyphs: usize,
}

fn sorted_families(db: &fontdb::Database) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (name, _) in db.faces().flat_map(|face| face.families.iter()) {
        let name = name.trim();
        if !name.is_empty() && !is_file_family(name) && !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            names.push(name.to_owned());
        }
    }
    names.sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
    names
}

fn choose_default_family(db: &fontdb::Database) -> Option<String> {
    let names = sorted_families(db);
    DEFAULT_FAMILY_CANDIDATES
        .iter()
        .find_map(|wanted| names.iter().find(|name| name.eq_ignore_ascii_case(wanted)))
        .or_else(|| names.first())
        .cloned()
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

    /// The glyphs grown by `radius` pixels: each pixel is covered by how far it is from the
    /// nearest covered pixel, through an exact Euclidean distance transform, so the cost does not
    /// grow with the radius. A pixel under half covered starts that far short of the edge, so thin
    /// strokes with no pixel half covered still grow. Never less than the coverage itself.
    fn dilate(&self, radius: f32) -> Mask {
        let (w, h) = (self.width, self.height);
        let mut squared: Vec<f64> = self
            .data
            .iter()
            .map(|&a| {
                if a > 0.0 {
                    let short = (0.5 - a as f64).max(0.0);
                    short * short
                } else {
                    FAR
                }
            })
            .collect();
        let mut scratch = DistanceScratch::new(w.max(h));
        let mut column = std::mem::take(&mut scratch.line);
        column.truncate(h);
        for x in 0..w {
            for y in 0..h {
                column[y] = squared[y * w + x];
            }
            scratch.transform(&mut column);
            for y in 0..h {
                squared[y * w + x] = column[y];
            }
        }
        for row in squared.chunks_exact_mut(w) {
            scratch.transform(row);
        }
        // The edge of the glyph lies about half a pixel past the centre of its last covered pixel.
        let data = squared.iter().zip(&self.data).map(|(&d2, &a)| a.max((radius + 1.0 - d2.sqrt() as f32).clamp(0.0, 1.0))).collect();
        Mask { width: w, height: h, data }
    }

    /// Approximates a Gaussian blur of about `radius` pixels with three box blurs.
    fn blur(&mut self, radius: f32) {
        let half = blur_half(radius);
        if half == 0 {
            return;
        }
        let mut tmp = vec![0f32; self.width.max(self.height)];
        let mut column = vec![0f32; self.height];
        for _ in 0..3 {
            for y in 0..self.height {
                let row = &mut self.data[y * self.width..(y + 1) * self.width];
                box_blur(row, half, &mut tmp);
            }
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

/// The half width of each box of [`Mask::blur`]; 0 for a blur under a pixel, which is skipped.
fn blur_half(radius: f32) -> usize {
    (radius / 2.0).round() as usize
}

/// A squared distance larger than any picture has, small enough that `f64` still adds a squared
/// pixel distance to it exactly.
const FAR: f64 = 1e10;

/// Buffers for the one-dimensional squared distance transform of Felzenszwalb and Huttenlocher,
/// kept from row to row.
struct DistanceScratch {
    line: Vec<f64>,
    out: Vec<f64>,
    hulls: Vec<usize>,
    bounds: Vec<f64>,
}

impl DistanceScratch {
    fn new(n: usize) -> DistanceScratch {
        DistanceScratch { line: vec![0.0; n], out: vec![0.0; n], hulls: vec![0; n], bounds: vec![0.0; n + 1] }
    }

    /// Replaces each `f[i]` with the least `f[j] + (i - j)²`, in linear time.
    fn transform(&mut self, f: &mut [f64]) {
        let n = f.len();
        if n == 0 {
            return;
        }
        let (v, z, d) = (&mut self.hulls, &mut self.bounds, &mut self.out);
        let mut k = 0usize;
        v[0] = 0;
        z[0] = f64::NEG_INFINITY;
        z[1] = f64::INFINITY;
        for q in 1..n {
            let qf = q as f64;
            let mut s;
            loop {
                let pf = v[k] as f64;
                s = ((f[q] + qf * qf) - (f[v[k]] + pf * pf)) / (2.0 * (qf - pf));
                if s > z[k] {
                    break;
                }
                // z[0] is minus infinity, so this stops at k = 0.
                k -= 1;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
        }
        k = 0;
        for q in 0..n {
            let qf = q as f64;
            while z[k + 1] < qf {
                k += 1;
            }
            let delta = qf - v[k] as f64;
            d[q] = delta * delta + f[v[k]];
        }
        f.copy_from_slice(&d[..n]);
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
