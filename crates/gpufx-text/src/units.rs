//! Drawing a text whose units move one by one.
//!
//! Each character (a grapheme, see [`engine::unit_ranges`]) keeps the coverage of its glyphs, one
//! byte a pixel, made once per text and size (see [`UnitsPlan`]). A frame moves every character's
//! coverage into one mask, through the transform of its units, and draws that mask as the still
//! picture draws its glyphs: the background box, then the glow, shadow and outline of the whole
//! mask, then the fill. So where the layers of two characters meet they join as in the still
//! picture, and the frame just before the units rest looks like the still text (no jump when the
//! still picture takes over). A glow, shadow or outline does not turn with its character: the
//! shadow keeps its direction, as it would on a real text.
//!
//! A character that is partly see-through (fading in or out) cannot go into the shared mask: the
//! outline and glow of a faint coverage would show at full strength. It is drawn from a picture of
//! its own with its own glow, shadow and outline, made the first time it is needed and kept with
//! the plan, at its opacity.
//!
//! The frame grows past the still picture, by the same amount on opposite sides, when a moved
//! character would leave it (see [`TextImage::margin`]), so the middle stays where it was; the
//! margin goes up in steps so most frames share a size.
//!
//! Why the CPU and not one GPU quad per character: the compositor places one picture per clip and
//! has no path for many quads; a picture per frame goes through it as a still does. Only the
//! coverages are kept on the CPU, and a frame only moves them, without shaping or drawing glyphs
//! again. The plan is shared through an [`Arc`], so a frame is drawn without holding the
//! rasterizer.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use engine::{unit_ranges, AnimationPose, TextUnit};

use super::{draw_background, draw_mask, draw_under, over, premultiplied, text_font_stamp, Layout, Mask, TextContent, TextImage, TextRasterizer, MAX_DIMENSION, MAX_PIXELS};
use cosmic_text::SwashContent;

/// How many plans a rasterizer keeps, so a few clips moving at once do not take turns rebuilding.
pub const MAX_UNIT_PLANS: usize = 6;
/// The most bytes the kept plans may take, counting the pictures of fading characters as if every
/// one were made; the newest plan stays whatever its size.
pub const UNIT_PLAN_BYTES: usize = 64 * 1024 * 1024;

/// The coverage of one character.
pub(crate) struct Sprite {
    /// Where its top left lies in the text picture.
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    /// The coverage of its glyphs, 0 to 255, cut to the pixels they touch.
    coverage: Vec<u8>,
    /// Its glow, shadow and outline alone, premultiplied, with [`Prepared::room`] around the
    /// coverage on every side; made when it first fades.
    under: OnceLock<Vec<[f32; 4]>>,
    /// The byte of the shaped text the character starts at.
    start: usize,
    /// Its ink box in the text picture.
    ink: (f32, f32, f32, f32),
}

/// What a frame of a moving text is drawn from: the text, the coverage of each character, the
/// background box and the numbers of the layers, for one text, output height and scale. Made by
/// [`TextRasterizer::prepare_units`] and kept by the rasterizer; drawing a frame with
/// [`UnitsPlan::draw`] needs no rasterizer.
#[derive(Clone)]
pub struct UnitsPlan(Arc<Prepared>);

pub(crate) struct Prepared {
    text: TextContent,
    /// The numbers [`draw_under`] reads: font size, outline, shadow and glow reach.
    layers: Layout,
    /// How far the glow, shadow and outline of a character reach past its coverage, in pixels.
    room: i32,
    /// The size of the still picture.
    width: usize,
    height: usize,
    /// The background box alone, as large as the still picture, when the text has one.
    background: Option<Vec<[f32; 4]>>,
    sprites: Vec<Sprite>,
    /// The picture with no pixels: its size and what the layout found missing.
    blank: TextImage,
    bytes: usize,
}

/// Whether `pose` leaves a unit as it is.
pub(crate) fn is_rest(pose: &AnimationPose) -> bool {
    pose.dx == 0.0 && pose.dy == 0.0 && pose.scale == 1.0 && pose.scale_x == 1.0 && pose.scale_y == 1.0 && pose.rotation == 0.0 && pose.opacity >= 1.0
}

/// An affine map `(x, y) -> (a x + b y + c, d x + e y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine([f32; 6]);

impl Affine {
    const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    fn translate(x: f32, y: f32) -> Affine {
        Affine([1.0, 0.0, x, 0.0, 1.0, y])
    }

    /// `self` after `first`.
    fn after(self, first: Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [p, q, r, s, t, u] = first.0;
        Affine([a * p + b * s, a * q + b * t, a * r + b * u + c, d * p + e * s, d * q + e * t, d * r + e * u + f])
    }

    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + b * y + c, d * x + e * y + f)
    }

    fn inverse(self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * e - b * d;
        if det.abs() < 1e-9 {
            return None;
        }
        let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
        Some(Affine([ia, ib, -(ia * c + ib * f), id, ie, -(id * c + ie * f)]))
    }

    /// The whole-pixel shift this map is, when it is only one.
    fn whole_shift(self) -> Option<(i32, i32)> {
        let [a, b, c, d, e, f] = self.0;
        (a == 1.0 && b == 0.0 && d == 0.0 && e == 1.0 && c.fract() == 0.0 && f.fract() == 0.0).then_some((c as i32, f as i32))
    }

    /// The map a unit pose gives, around `center`, for a font of `font_px`.
    fn of_pose(pose: &AnimationPose, center: (f32, f32), font_px: f32) -> Affine {
        let (sx, sy) = ((pose.scale * pose.scale_x) as f32, (pose.scale * pose.scale_y) as f32);
        let (sin, cos) = (pose.rotation as f32).to_radians().sin_cos();
        let turn_scale = Affine([cos * sx, -sin * sy, 0.0, sin * sx, cos * sy, 0.0]);
        let moved = (center.0 + pose.dx as f32 * font_px, center.1 + pose.dy as f32 * font_px);
        Affine::translate(moved.0, moved.1).after(turn_scale).after(Affine::translate(-center.0, -center.1))
    }
}

/// The index of the range of `ranges` (in rising order) that holds byte `at`.
fn holding(ranges: &[Range<usize>], at: usize) -> Option<usize> {
    let index = ranges.partition_point(|range| range.end <= at);
    ranges.get(index).filter(|range| range.contains(&at)).map(|_| index)
}

fn union(a: Option<(f32, f32, f32, f32)>, b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    a.map_or(b, |a| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// The value of `picture` at `(x, y)`, between its pixels; `T::default()` outside it.
fn sample<T: Copy + Default>(picture: &[T], width: usize, height: usize, x: f32, y: f32, mix: impl Fn([T; 4], [f32; 4]) -> T) -> T {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let at = |i: i64, j: i64| -> T {
        if i < 0 || j < 0 || i >= width as i64 || j >= height as i64 {
            T::default()
        } else {
            picture[j as usize * width + i as usize]
        }
    };
    mix([at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1)], [(1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy])
}

/// A picture's place before it is moved: left, top, width, height.
type Rect = (i32, i32, usize, usize);

impl Sprite {
    fn rect(&self) -> Rect {
        (self.x, self.y, self.width, self.height)
    }

    /// The rectangle of its own glow, shadow and outline.
    fn under_rect(&self, room: i32) -> Rect {
        (self.x - room, self.y - room, self.width + 2 * room as usize, self.height + 2 * room as usize)
    }
}

/// Calls `each(x, y, sx, sy)` for every pixel of a canvas `canvas_width` by `canvas_height` that
/// a picture at `rect` covers through `map`, with the point of the picture it comes from.
fn cover(canvas_width: usize, canvas_height: usize, rect: Rect, map: Affine, mut each: impl FnMut(usize, usize, f32, f32)) {
    let Some(back) = map.inverse() else { return };
    let (x0, y0, x1, y1) = (rect.0 as f32, rect.1 as f32, (rect.0 + rect.2 as i32) as f32, (rect.1 + rect.3 as i32) as f32);
    let corners = [map.apply(x0, y0), map.apply(x1, y0), map.apply(x0, y1), map.apply(x1, y1)];
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let max_x = (corners.iter().map(|c| c.0).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(canvas_width);
    let max_y = (corners.iter().map(|c| c.1).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(canvas_height);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let (sx, sy) = back.apply(x as f32 + 0.5, y as f32 + 0.5);
            each(x, y, sx - x0 - 0.5, sy - y0 - 0.5);
        }
    }
}

/// The box a picture at `rect` covers through `map`.
fn covered_box(rect: Rect, map: Affine) -> (f32, f32, f32, f32) {
    let (x0, y0, x1, y1) = (rect.0 as f32, rect.1 as f32, (rect.0 + rect.2 as i32) as f32, (rect.1 + rect.3 as i32) as f32);
    let corners = [map.apply(x0, y0), map.apply(x1, y0), map.apply(x0, y1), map.apply(x1, y1)];
    corners.iter().fold((f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY), |b, c| (b.0.min(c.0), b.1.min(c.1), b.2.max(c.0), b.3.max(c.1)))
}

fn coverage_mix(p: [u8; 4], w: [f32; 4]) -> u8 {
    let value = (0..4).map(|i| f32::from(p[i]) * w[i]).sum::<f32>();
    (value + 0.5).clamp(0.0, 255.0) as u8
}

fn color_mix(p: [[f32; 4]; 4], w: [f32; 4]) -> [f32; 4] {
    std::array::from_fn(|c| (0..4).map(|i| p[i][c] * w[i]).sum())
}

impl Prepared {
    /// The glow, shadow and outline of `sprite` alone, made the first time.
    fn under_of<'a>(&self, sprite: &'a Sprite) -> &'a [[f32; 4]] {
        sprite.under.get_or_init(|| {
            let (_, _, width, height) = sprite.under_rect(self.room);
            let room = self.room as usize;
            let mut fill = Mask::new(width, height);
            for (row, from) in sprite.coverage.chunks_exact(sprite.width).enumerate() {
                for (i, &c) in from.iter().enumerate() {
                    fill.data[(row + room) * width + room + i] = f32::from(c) / 255.0;
                }
            }
            let mut under = vec![[0f32; 4]; width * height];
            draw_under(&self.text, &self.layers, &fill, &mut under);
            under
        })
    }
}

impl UnitsPlan {
    /// Draws a frame of the text with its units moved by `poses` (see
    /// [`TextRasterizer::rasterize_units`]). The picture is the still picture's size grown by
    /// [`TextImage::margin`] on each side.
    pub fn draw(&self, poses: &[(TextUnit, Vec<AnimationPose>)]) -> TextImage {
        let plan = &*self.0;
        let mut image = plan.blank.clone();
        let (width, height) = (plan.width, plan.height);
        if width == 0 || height == 0 {
            return image;
        }
        let sprites = &plan.sprites;
        let shaped = plan.text.shaped_content();
        let font_px = plan.layers.font_px;
        // The map and opacity of each character, through the poses of every kind of unit.
        let mut placed: Vec<(Affine, f32)> = vec![(Affine::IDENTITY, 1.0); sprites.len()];
        for (kind, unit_poses) in poses {
            let ranges = unit_ranges(&shaped, *kind);
            let owner: Vec<Option<usize>> = sprites.iter().map(|sprite| holding(&ranges, sprite.start)).collect();
            let mut centers: Vec<Option<(f32, f32, f32, f32)>> = vec![None; ranges.len()];
            for (sprite, unit) in sprites.iter().zip(&owner) {
                if let Some(unit) = unit {
                    centers[*unit] = Some(union(centers[*unit], sprite.ink));
                }
            }
            for (index, unit) in owner.iter().enumerate() {
                let (Some(unit), Some(pose)) = (unit, unit.and_then(|unit| unit_poses.get(unit))) else { continue };
                let Some(ink) = centers[*unit] else { continue };
                let center = ((ink.0 + ink.2) / 2.0, (ink.1 + ink.3) / 2.0);
                let (map, opacity) = placed[index];
                placed[index] = (Affine::of_pose(pose, center, font_px).after(map), opacity * pose.opacity.clamp(0.0, 1.0) as f32);
            }
        }
        // Grow the picture by the same amount on opposite sides so every moved character fits,
        // with its glow, shadow and outline, which do not turn with it.
        let (mut reach_x, mut reach_y) = (0f32, 0f32);
        let room = plan.room as f32;
        for (sprite, (map, opacity)) in sprites.iter().zip(&placed) {
            if *opacity > 0.0 {
                let b = covered_box(sprite.rect(), *map);
                reach_x = reach_x.max(room - b.0).max(b.2 + room - width as f32);
                reach_y = reach_y.max(room - b.1).max(b.3 + room - height as f32);
            }
        }
        let (margin_x, margin_y) = margins(width, height, font_px, reach_x, reach_y);
        let (canvas_width, canvas_height) = (width + 2 * margin_x, height + 2 * margin_y);
        let shift = Affine::translate(margin_x as f32, margin_y as f32);

        let mut canvas = vec![[0f32; 4]; canvas_width * canvas_height];
        if let Some(background) = &plan.background {
            for (row, from) in background.chunks_exact(width).enumerate() {
                let at = (row + margin_y) * canvas_width + margin_x;
                canvas[at..at + width].copy_from_slice(from);
            }
        }
        // The characters that show whole share one mask, drawn as the still picture's glyphs.
        let mut mask = Mask::new(canvas_width, canvas_height);
        let mut any_whole = false;
        for (sprite, (map, opacity)) in sprites.iter().zip(&placed) {
            if *opacity < 1.0 {
                continue;
            }
            any_whole = true;
            let map = shift.after(*map);
            match map.whole_shift() {
                // A character at rest, or moved by whole pixels, is copied as it is.
                Some((dx, dy)) => {
                    for j in 0..sprite.height {
                        for i in 0..sprite.width {
                            let c = sprite.coverage[j * sprite.width + i];
                            if c > 0 {
                                mask.add(sprite.x + dx + i as i32, sprite.y + dy + j as i32, f32::from(c) / 255.0);
                            }
                        }
                    }
                }
                None => cover(canvas_width, canvas_height, sprite.rect(), map, |x, y, sx, sy| {
                    let c = sample(&sprite.coverage, sprite.width, sprite.height, sx, sy, coverage_mix);
                    if c > 0 {
                        mask.add(x as i32, y as i32, f32::from(c) / 255.0);
                    }
                }),
            }
        }
        if any_whole {
            draw_under(&plan.text, &plan.layers, &mask, &mut canvas);
        }
        // The fading characters, each with its own layers, at its opacity.
        let fading: Vec<(&Sprite, Affine, f32)> = sprites.iter().zip(&placed).filter(|(_, (_, opacity))| *opacity > 0.0 && *opacity < 1.0).map(|(sprite, (map, opacity))| (sprite, shift.after(*map), *opacity)).collect();
        for (sprite, map, opacity) in &fading {
            let under = plan.under_of(sprite);
            let rect = sprite.under_rect(plan.room);
            cover(canvas_width, canvas_height, rect, *map, |x, y, sx, sy| {
                let color = sample(under, rect.2, rect.3, sx, sy, color_mix);
                if color[3] > 0.0 {
                    over(&mut canvas[y * canvas_width + x], color, *opacity);
                }
            });
        }
        let fill_color = premultiplied(plan.text.color);
        if any_whole {
            draw_mask(&mut canvas, &mask, fill_color);
        }
        for (sprite, map, opacity) in &fading {
            cover(canvas_width, canvas_height, sprite.rect(), *map, |x, y, sx, sy| {
                let c = sample(&sprite.coverage, sprite.width, sprite.height, sx, sy, coverage_mix);
                if c > 0 {
                    over(&mut canvas[y * canvas_width + x], fill_color, f32::from(c) / 255.0 * opacity);
                }
            });
        }
        image.width = canvas_width as u32;
        image.height = canvas_height as u32;
        image.margin = (margin_x as u32, margin_y as u32);
        image.pixels = canvas.iter().flat_map(|p| p.map(|c| (c * 255.0 + 0.5).clamp(0.0, 255.0) as u8)).collect();
        image
    }

    /// The bytes the plan holds, counting every fading character's picture as made.
    pub fn bytes(&self) -> usize {
        self.0.bytes
    }
}

/// The margins of a frame whose characters reach `reach_x` and `reach_y` pixels past a still
/// picture `width` by `height`: rounded up to steps of a quarter of the font size, at most two font
/// sizes, and kept within [`MAX_DIMENSION`] and [`MAX_PIXELS`]. A character farther out is cut.
fn margins(width: usize, height: usize, font_px: f32, reach_x: f32, reach_y: f32) -> (usize, usize) {
    let step = (font_px / 4.0).ceil().max(4.0);
    let most = (font_px * 2.0).ceil();
    let round = |reach: f32| if reach <= 0.0 { 0.0 } else { ((reach / step).ceil() * step).min(most) };
    let (mut x, mut y) = (round(reach_x) as usize, round(reach_y) as usize);
    let fits = |x: usize, y: usize| {
        let (w, h) = ((width + 2 * x) as u64, (height + 2 * y) as u64);
        w <= u64::from(MAX_DIMENSION) && h <= u64::from(MAX_DIMENSION) && w * h <= MAX_PIXELS
    };
    while !fits(x, y) && (x > 0 || y > 0) {
        if x >= y {
            x = x.saturating_sub(step as usize);
        } else {
            y = y.saturating_sub(step as usize);
        }
    }
    (x, y)
}

impl TextRasterizer {
    /// What frames of `text` whose units move are drawn from, for an output `output_height_px`
    /// tall at `scale` (see [`Self::rasterize`]). Kept for the next call with the same text, size and
    /// font file; the last [`MAX_UNIT_PLANS`] are kept, within [`UNIT_PLAN_BYTES`].
    pub fn prepare_units(&mut self, text: &TextContent, output_height_px: u32, scale: f32) -> UnitsPlan {
        let mut hasher = DefaultHasher::new();
        format!("{text:?}").hash(&mut hasher);
        output_height_px.hash(&mut hasher);
        scale.to_bits().hash(&mut hasher);
        text_font_stamp(text).hash(&mut hasher);
        let key = hasher.finish();
        if let Some(index) = self.unit_plans.iter().position(|(known, _)| *known == key) {
            let entry = self.unit_plans.remove(index);
            let plan = entry.1.clone();
            self.unit_plans.push(entry);
            return plan;
        }
        let (layout, downscaled) = self.fitted_layout(text, output_height_px, scale);
        let plan = UnitsPlan(Arc::new(self.prepared(text, &layout, downscaled)));
        self.unit_plan_builds += 1;
        self.unit_plans.push((key, plan.clone()));
        let mut total: usize = self.unit_plans.iter().map(|(_, plan)| plan.bytes()).sum();
        while self.unit_plans.len() > 1 && (self.unit_plans.len() > MAX_UNIT_PLANS || total > UNIT_PLAN_BYTES) {
            let (_, gone) = self.unit_plans.remove(0);
            total -= gone.bytes();
        }
        plan
    }

    /// How many plans [`Self::prepare_units`] has made rather than found kept.
    pub fn unit_plan_builds(&self) -> u64 {
        self.unit_plan_builds
    }

    fn prepared(&mut self, text: &TextContent, layout: &Layout, downscaled: bool) -> Prepared {
        let blank = TextImage { width: layout.width, height: layout.height, pixels: Vec::new(), missing_family: layout.missing_family.clone(), no_font: layout.no_font, missing_glyphs: layout.missing_glyphs, downscaled, margin: (0, 0) };
        let layers = Layout { font_px: layout.font_px, outline_px: layout.outline_px, shadow: layout.shadow, glow_px: layout.glow_px, ..Layout::default() };
        let mut prepared = Prepared { text: text.clone(), layers, room: 0, width: 0, height: 0, background: None, sprites: Vec::new(), blank, bytes: 0 };
        let (width, height) = (layout.width as usize, layout.height as usize);
        if width == 0 || height == 0 {
            return prepared;
        }
        if width > MAX_DIMENSION as usize || height > MAX_DIMENSION as usize || (width as u64) * (height as u64) > MAX_PIXELS {
            prepared.blank.width = 0;
            prepared.blank.height = 0;
            return prepared;
        }
        (prepared.width, prepared.height) = (width, height);
        if text.background.is_some() {
            let mut background = vec![[0f32; 4]; width * height];
            draw_background(text, layout, &mut background);
            prepared.bytes += background.len() * 16;
            prepared.background = Some(background);
        }
        let shaped = text.shaped_content();
        let graphemes = unit_ranges(&shaped, TextUnit::Char);
        let (left, top) = layout.origin;
        // The glyphs and the ink of each character.
        let mut members: Vec<(Vec<usize>, Option<(f32, f32, f32, f32)>)> = vec![(Vec::new(), None); graphemes.len()];
        for (number, start) in layout.glyph_starts.iter().enumerate() {
            let (Some(unit), Some(Some(ink))) = (holding(&graphemes, *start), layout.glyph_ink.get(number)) else { continue };
            let entry = &mut members[unit];
            entry.0.push(number);
            entry.1 = Some(union(entry.1, (left + ink.0, top + ink.1, left + ink.2, top + ink.3)));
        }
        // Room around the ink for the glow, the outline and the shadow of a fading character.
        let (shadow_dx, shadow_dy, shadow_blur) = layout.shadow;
        let shadow_reach = if text.shadow.is_some() { shadow_dx.abs().max(shadow_dy.abs()) + shadow_blur * 2.0 } else { 0.0 };
        let room = (layout.outline_px + layout.glow_px.max(shadow_reach)).ceil() as i32 + 3;
        prepared.room = room;
        let (ox, oy) = (left as i32, top as i32);
        for ((glyphs, ink), range) in members.into_iter().zip(&graphemes) {
            let Some(ink) = ink else { continue };
            let (x, y) = (ink.0.floor() as i32 - room, ink.1.floor() as i32 - room);
            let (sprite_width, sprite_height) = ((ink.2.ceil() as i32 + room - x).max(1) as usize, (ink.3.ceil() as i32 + room - y).max(1) as usize);
            let mut fill = Mask::new(sprite_width, sprite_height);
            for number in glyphs {
                let glyph = &layout.glyphs[number];
                let Some(picture) = self.cache.get_image(&mut self.font_system, glyph.cache_key).as_ref() else { continue };
                let p = picture.placement;
                let gx = ox + glyph.x + p.left - x;
                let gy = oy + glyph.y - p.top - y;
                let stride = match picture.content {
                    SwashContent::Mask => 1,
                    SwashContent::SubpixelMask | SwashContent::Color => 4,
                };
                for j in 0..p.height as i32 {
                    for i in 0..p.width as i32 {
                        let index = (j as usize * p.width as usize + i as usize) * stride;
                        fill.add(gx + i, gy + j, picture.data[index + stride - 1] as f32 / 255.0);
                    }
                }
            }
            // Cut to the pixels the glyphs touch, so a frame goes over no empty room.
            let covered = |a: &f32| (a * 255.0 + 0.5) as u8 > 0;
            let rows: Vec<usize> = (0..sprite_height).filter(|&j| fill.data[j * sprite_width..(j + 1) * sprite_width].iter().any(covered)).collect();
            let columns: Vec<usize> = (0..sprite_width).filter(|&i| (0..sprite_height).any(|j| covered(&fill.data[j * sprite_width + i]))).collect();
            let (Some(&top_row), Some(&bottom_row), Some(&left_column), Some(&right_column)) = (rows.first(), rows.last(), columns.first(), columns.last()) else { continue };
            let (cut_width, cut_height) = (right_column - left_column + 1, bottom_row - top_row + 1);
            let mut coverage = Vec::with_capacity(cut_width * cut_height);
            for j in top_row..=bottom_row {
                coverage.extend(fill.data[j * sprite_width + left_column..=j * sprite_width + right_column].iter().map(|a| (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8));
            }
            let under_pixels = (cut_width + 2 * room as usize) * (cut_height + 2 * room as usize);
            prepared.bytes += coverage.len() + under_pixels * 16;
            prepared.sprites.push(Sprite { x: x + left_column as i32, y: y + top_row as i32, width: cut_width, height: cut_height, coverage, under: OnceLock::new(), start: range.start, ink });
        }
        prepared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pose_maps_around_its_centre() {
        let pose = AnimationPose { rotation: 90.0, ..AnimationPose::IDENTITY };
        let map = Affine::of_pose(&pose, (10.0, 10.0), 20.0);
        let (x, y) = map.apply(20.0, 10.0);
        assert!((x - 10.0).abs() < 1e-4 && (y - 20.0).abs() < 1e-4, "a quarter turn clockwise on a picture with y down: {x} {y}");
        let moved = Affine::of_pose(&AnimationPose { dx: 0.5, dy: -1.0, ..AnimationPose::IDENTITY }, (0.0, 0.0), 20.0);
        assert_eq!(moved.apply(1.0, 1.0), (11.0, -19.0), "dx and dy are in font sizes");
        let back = map.inverse().unwrap().after(map);
        assert!(back.0.iter().zip(Affine::IDENTITY.0).all(|(a, b)| (a - b).abs() < 1e-5));
        assert!(Affine::of_pose(&AnimationPose { scale_y: 0.0, ..AnimationPose::IDENTITY }, (0.0, 0.0), 1.0).inverse().is_none(), "a flat unit draws nothing");
        assert_eq!(Affine::translate(3.0, -2.0).whole_shift(), Some((3, -2)));
        assert_eq!(Affine::translate(0.5, 0.0).whole_shift(), None);
    }

    #[test]
    fn margins_go_up_in_steps_and_stay_within_the_limits() {
        assert_eq!(margins(100, 50, 40.0, 0.0, -3.0), (0, 0));
        assert_eq!(margins(100, 50, 40.0, 1.0, 11.0), (10, 20), "steps of a quarter font size");
        assert_eq!(margins(100, 50, 40.0, 500.0, 0.0), (80, 0), "at most two font sizes");
        let (x, _) = margins(MAX_DIMENSION as usize - 10, 50, 40.0, 30.0, 0.0);
        assert_eq!(x, 0, "never past the largest picture");
    }
}
