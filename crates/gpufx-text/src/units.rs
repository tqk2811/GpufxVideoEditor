//! Drawing a text whose units move one by one.
//!
//! Each character (a grapheme, see [`engine::unit_ranges`]) is drawn once into a small picture of
//! its own, with its glow, shadow and outline, and kept until another text is drawn. A frame puts
//! the background box down, then every character's under layers (glow, shadow, outline) and then
//! every character's fill, each moved by the transform of its units, so an outline never covers
//! the fill of the next character, as in the still picture.
//!
//! Why the CPU and not one GPU quad per character: the compositor places one picture per clip and
//! has no path for many quads; a picture per frame is what karaoke steps already go through (the
//! still cache keeps each drawn frame while it is shown and lets it go after), so the GPU holds one
//! picture of the text's size, never one per character. Only the character pictures are kept on
//! the CPU, and a frame only moves them, without shaping or drawing glyphs again.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use engine::{unit_ranges, AnimationPose, TextUnit};

use super::{draw_background, draw_mask, draw_under, over, premultiplied, Layout, Mask, TextContent, TextImage, TextRasterizer, MAX_DIMENSION, MAX_PIXELS};
use cosmic_text::SwashContent;

/// The picture of one character, with its glow, shadow and outline.
pub(crate) struct Sprite {
    /// Where its top left lies in the text picture.
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    /// The glow, shadow and outline, premultiplied.
    under: Vec<[f32; 4]>,
    /// The fill, premultiplied.
    fill: Vec<[f32; 4]>,
    /// The byte of the shaped text the character starts at.
    start: usize,
    /// Its ink box in the text picture.
    ink: (f32, f32, f32, f32),
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

/// The premultiplied colour of `picture` at `(x, y)`, between its pixels; clear outside it.
fn sample(picture: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 4] {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let at = |i: i64, j: i64| -> [f32; 4] {
        if i < 0 || j < 0 || i >= width as i64 || j >= height as i64 {
            [0.0; 4]
        } else {
            picture[j as usize * width + i as usize]
        }
    };
    let (p00, p10, p01, p11) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    std::array::from_fn(|c| (p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy)
}

/// Puts `picture`, whose top left is at `(x, y)` of the canvas before `map`, over `canvas` through
/// `map`, at `opacity`.
fn put(canvas: &mut [[f32; 4]], canvas_width: usize, canvas_height: usize, sprite: &Sprite, picture: &[[f32; 4]], map: Affine, opacity: f32) {
    let Some(back) = map.inverse() else { return };
    let (x0, y0, x1, y1) = (sprite.x as f32, sprite.y as f32, (sprite.x + sprite.width as i32) as f32, (sprite.y + sprite.height as i32) as f32);
    let corners = [map.apply(x0, y0), map.apply(x1, y0), map.apply(x0, y1), map.apply(x1, y1)];
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let max_x = (corners.iter().map(|c| c.0).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(canvas_width);
    let max_y = (corners.iter().map(|c| c.1).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(canvas_height);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let (sx, sy) = back.apply(x as f32 + 0.5, y as f32 + 0.5);
            let color = sample(picture, sprite.width, sprite.height, sx - x0 - 0.5, sy - y0 - 0.5);
            if color[3] > 0.0 {
                over(&mut canvas[y * canvas_width + x], color, opacity);
            }
        }
    }
}

impl TextRasterizer {
    /// The pictures of the characters of `text` laid out as `layout`, drawn once and kept for the
    /// next frames of the same text.
    fn sprites(&mut self, text: &TextContent, layout: &Layout) -> Arc<Vec<Sprite>> {
        let mut hasher = DefaultHasher::new();
        format!("{text:?}").hash(&mut hasher);
        layout.font_px.to_bits().hash(&mut hasher);
        (layout.width, layout.height).hash(&mut hasher);
        let key = hasher.finish();
        if let Some((known, sprites)) = &self.unit_sprites {
            if *known == key {
                return sprites.clone();
            }
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
        // Room around the ink for the glow, the outline and the shadow.
        let (shadow_dx, shadow_dy, shadow_blur) = layout.shadow;
        let shadow_reach = if text.shadow.is_some() { shadow_dx.abs().max(shadow_dy.abs()) + shadow_blur * 2.0 } else { 0.0 };
        let room = (layout.outline_px + layout.glow_px.max(shadow_reach)).ceil() as i32 + 3;
        let (ox, oy) = (left as i32, top as i32);
        let fill_color = premultiplied(text.color);
        let mut sprites = Vec::new();
        for ((glyphs, ink), range) in members.into_iter().zip(&graphemes) {
            let Some(ink) = ink else { continue };
            let (x, y) = (ink.0.floor() as i32 - room, ink.1.floor() as i32 - room);
            let (width, height) = ((ink.2.ceil() as i32 + room - x).max(1) as usize, (ink.3.ceil() as i32 + room - y).max(1) as usize);
            let mut fill = Mask::new(width, height);
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
            let mut under = vec![[0f32; 4]; width * height];
            draw_under(text, layout, &fill, &mut under);
            let mut filled = vec![[0f32; 4]; width * height];
            draw_mask(&mut filled, &fill, fill_color);
            sprites.push(Sprite { x, y, width, height, under, fill: filled, start: range.start, ink });
        }
        let sprites = Arc::new(sprites);
        self.unit_sprites = Some((key, sprites.clone()));
        sprites
    }

    /// Draws `layout` with its units moved by `poses` (see [`TextRasterizer::rasterize_units`]).
    pub(crate) fn draw_units(&mut self, text: &TextContent, layout: &Layout, poses: &[(TextUnit, Vec<AnimationPose>)]) -> TextImage {
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
        let sprites = self.sprites(text, layout);
        let shaped = text.shaped_content();
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
                placed[index] = (Affine::of_pose(pose, center, layout.font_px).after(map), opacity * pose.opacity.clamp(0.0, 1.0) as f32);
            }
        }
        let mut canvas = vec![[0f32; 4]; width * height];
        draw_background(text, layout, &mut canvas);
        for (sprite, (map, opacity)) in sprites.iter().zip(&placed) {
            if *opacity > 0.0 {
                put(&mut canvas, width, height, sprite, &sprite.under, *map, *opacity);
            }
        }
        for (sprite, (map, opacity)) in sprites.iter().zip(&placed) {
            if *opacity > 0.0 {
                put(&mut canvas, width, height, sprite, &sprite.fill, *map, *opacity);
            }
        }
        image.pixels = canvas.iter().flat_map(|p| p.map(|c| (c * 255.0 + 0.5).clamp(0.0, 255.0) as u8)).collect();
        image
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
    }
}
