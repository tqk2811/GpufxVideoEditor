//! Still images as clip sources: decoded once to straight-alpha RGBA, with no GPU here.
//!
//! The compositor samples these pictures bilinearly in straight alpha, so a transparent texel's colour
//! shows at the edge of what is opaque next to it. PNG tools leave black or anything else there, so
//! [`read_still`] fills the colour of transparent texels from the nearest visible one.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits, RgbaImage};

use crate::animated_webp::AnimatedWebp;
use crate::thumbnail::ThumbnailImage;

/// The longest side a still keeps; larger ones are shrunk when decoded, keeping their shape.
pub const MAX_STILL_SIDE: u32 = 4096;
/// The most pixels a file may decode to before it is refused rather than shrunk.
pub const MAX_STILL_PIXELS: u64 = 128 * 1000 * 1000;
/// A hard limit for the decoder itself, from what [`MAX_STILL_PIXELS`] can need.
const DECODER_ALLOCATION: u64 = 2 * (1 << 30);
/// The side limit given to the decoder; only stops a header lying about an absurd size.
const DECODER_SIDE_LIMIT: u32 = 65_535;

/// A decoded still: `width * height * 4` bytes of straight-alpha RGBA, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StillPicture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Whether any pixel is not fully opaque.
    pub has_alpha: bool,
}

impl StillPicture {
    /// Width over height.
    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height.max(1))
    }
}

fn reader_of(path: &Path) -> Result<ImageReader<BufReader<File>>, String> {
    let mut reader = ImageReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?.with_guessed_format().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODER_SIDE_LIMIT);
    limits.max_image_height = Some(DECODER_SIDE_LIMIT);
    limits.max_alloc = Some(DECODER_ALLOCATION);
    reader.limits(limits);
    Ok(reader)
}

/// The size of the picture in `path`, read from its header alone.
pub fn still_dimensions(path: &Path) -> Result<(u32, u32), String> {
    let dimensions = reader_of(path)?.into_dimensions().map_err(|e| format!("{}: {e}", path.display()))?;
    check_pixels(path, dimensions)?;
    Ok(dimensions)
}

fn check_pixels(path: &Path, (width, height): (u32, u32)) -> Result<(), String> {
    if u64::from(width) * u64::from(height) > MAX_STILL_PIXELS {
        return Err(format!("{}: {width}x{height} is larger than the {MAX_STILL_PIXELS} pixels a still may have", path.display()));
    }
    Ok(())
}

/// Reads and decodes `path`, turned as its EXIF orientation says, and shrunk to [`MAX_STILL_SIDE`].
pub fn read_still(path: &Path) -> Result<StillPicture, String> {
    Ok(finished_picture(decoded_rgba(path)?))
}

/// The pixels of `path`, turned as its EXIF orientation says, at the size of the file.
fn decoded_rgba(path: &Path) -> Result<RgbaImage, String> {
    let shown = path.display();
    let mut decoder = reader_of(path)?.into_decoder().map_err(|e| format!("{shown}: {e}"))?;
    check_pixels(path, decoder.dimensions())?;
    let orientation = decoder.orientation().map_err(|e| format!("{shown}: {e}"))?;
    let mut picture = DynamicImage::from_decoder(decoder).map_err(|e| format!("{shown}: {e}"))?;
    picture.apply_orientation(orientation);
    Ok(picture.into_rgba8())
}

/// A still of a picture drawn in premultiplied RGBA, such as rasterized text, turned into the
/// straight colours every still has. An empty picture gives one transparent pixel.
pub(crate) fn premultiplied_still(width: u32, height: u32, mut pixels: Vec<u8>) -> StillPicture {
    if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
        return StillPicture { width: 1, height: 1, rgba: vec![0; 4], has_alpha: true };
    }
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    let picture = RgbaImage::from_raw(width, height, pixels).expect("length checked above");
    finished_picture(picture)
}

/// Bleeds before it shrinks: the filter mixes neighbouring colours whatever their alpha, so shrinking
/// first would pull the junk colour of transparent texels into the edge of what is visible.
fn finished_picture(mut picture: RgbaImage) -> StillPicture {
    let has_alpha = picture.pixels().any(|pixel| pixel[3] != 255);
    if has_alpha {
        let (width, height) = picture.dimensions();
        bleed_into_transparent(&mut picture, width, height);
    }
    let (width, height) = picture.dimensions();
    let (fitted_width, fitted_height) = fitted_size(width, height, MAX_STILL_SIDE);
    if (fitted_width, fitted_height) != (width, height) {
        picture = image::imageops::resize(&picture, fitted_width, fitted_height, FilterType::Triangle);
    }
    let (width, height) = picture.dimensions();
    StillPicture { width, height, rgba: picture.into_raw(), has_alpha }
}

/// `width` x `height` shrunk, keeping the shape, so neither side passes `limit`. At least 1 by 1.
pub(crate) fn fitted_size(width: u32, height: u32, limit: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= limit {
        return (width.max(1), height.max(1));
    }
    let scale = f64::from(limit) / f64::from(longest);
    (((f64::from(width) * scale).round() as u32).max(1), ((f64::from(height) * scale).round() as u32).max(1))
}

/// Gives every fully transparent texel the colour of the nearest texel with any alpha, breadth first.
/// Alpha is left alone. A picture with nothing visible is left as it is.
fn bleed_into_transparent(picture: &mut RgbaImage, width: u32, height: u32) {
    let (columns, rows) = (width as usize, height as usize);
    let index = |x: usize, y: usize| y * columns + x;
    let mut filled: Vec<bool> = picture.as_raw().chunks_exact(4).map(|pixel| pixel[3] != 0).collect();
    // A pixel with no transparent neighbour can never give its colour away, so the queue only ever
    // holds the moving edge, instead of a queue entry for each pixel of a large opaque picture.
    let mut queue: VecDeque<(u32, u32)> = edge_of_visible(&filled, columns, rows).into();
    let raw: &mut [u8] = picture.as_mut();
    while let Some((x, y)) = queue.pop_front() {
        let (x, y) = (x as usize, y as usize);
        let from = index(x, y) * 4;
        let colour = [raw[from], raw[from + 1], raw[from + 2]];
        for (nx, ny) in neighbours_of(x, y) {
            if nx < columns && ny < rows && !filled[index(nx, ny)] {
                filled[index(nx, ny)] = true;
                let to = index(nx, ny) * 4;
                raw[to..to + 3].copy_from_slice(&colour);
                queue.push_back((nx as u32, ny as u32));
            }
        }
    }
}

fn neighbours_of(x: usize, y: usize) -> [(usize, usize); 4] {
    [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)]
}

/// The visible pixels with a transparent neighbour, where the colour starts to spread from.
fn edge_of_visible(visible: &[bool], columns: usize, rows: usize) -> Vec<(u32, u32)> {
    let index = |x: usize, y: usize| y * columns + x;
    let mut edge = Vec::new();
    for y in 0..rows {
        for x in 0..columns {
            if visible[index(x, y)] && neighbours_of(x, y).into_iter().any(|(nx, ny)| nx < columns && ny < rows && !visible[index(nx, ny)]) {
                edge.push((x as u32, y as u32));
            }
        }
    }
    edge
}

/// A small picture of `path`, `height` tall, straight alpha. Only for looking at, so it skips the
/// bleed and the shrink that a clip's picture needs.
pub fn still_thumbnail(path: &Path, height: u32) -> Result<ThumbnailImage, String> {
    let image = decoded_rgba(path)?;
    let aspect = f64::from(image.width()) / f64::from(image.height().max(1));
    let width = ((aspect * f64::from(height)).round() as u32).max(1);
    let small = image::imageops::resize(&image, width, height.max(1), FilterType::Triangle);
    let (width, height) = small.dimensions();
    Ok(ThumbnailImage { width, height, rgba: small.into_raw() })
}

/// The most bytes the decoded frames of one animated picture may take; a longer or larger one is
/// refused rather than kept half.
pub const MAX_ANIMATION_BYTES: u64 = 512 * 1024 * 1024;
/// How long a frame with a delay of this many milliseconds or less is shown instead, as browsers
/// do: some files say 0 and mean "as fast as the player likes", which would be a frozen picture.
/// The longest side the canvas of an animated picture may have, tighter than a still's pixels:
/// the canvas, a frame of it and the finished frame are all in memory while it is decoded.
pub const MAX_ANIMATION_SIDE: u32 = 4096;
const SHORTEST_FRAME_MS: u64 = 10;
/// What such a frame lasts.
const DEFAULT_FRAME_MS: u64 = 100;

/// The frames of an animated picture, each finished as a still is, and when each one ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimatedPicture {
    pub frames: Vec<StillPicture>,
    /// The time each frame ends, in milliseconds from the start of the loop; the last one is the
    /// length of the loop.
    pub ends_ms: Vec<u64>,
    /// How many times the loop plays before the last frame stays; 0 is forever.
    pub loops: u32,
}

impl AnimatedPicture {
    /// The frame shown `ticks` (1/90000 s) after the clip starts: the loop repeats [`Self::loops`]
    /// times, or forever, and then the last frame stays.
    pub fn frame_index_at(&self, ticks: i64) -> usize {
        let Some(&length) = self.ends_ms.last().filter(|length| **length > 0) else { return 0 };
        // Milliseconds rounded down, so a frame starts exactly at its time.
        let played = (ticks.max(0) / (engine::TICKS_PER_SECOND / 1000)) as u64;
        if self.loops != 0 && played >= length.saturating_mul(u64::from(self.loops)) {
            return self.frames.len() - 1;
        }
        let ms = played % length;
        self.ends_ms.partition_point(|end| *end <= ms).min(self.frames.len() - 1)
    }
}

/// What a frame that says it lasts `delay_ms` is shown for: a frame of [`SHORTEST_FRAME_MS`] or less
/// lasts [`DEFAULT_FRAME_MS`], as browsers show it.
fn frame_ms(delay_ms: u32) -> u64 {
    let delay = u64::from(delay_ms);
    if delay <= SHORTEST_FRAME_MS { DEFAULT_FRAME_MS } else { delay }
}

/// How long one loop of an animated picture lasts and how many times it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationTiming {
    /// The length of one loop in milliseconds, as [`AnimatedPicture::ends_ms`] ends.
    pub loop_ms: u64,
    /// How many times the loop plays; 0 is forever.
    pub loops: u32,
}

/// Which animated format the extension of `path` names, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnimatedFormat {
    Webp,
    Gif,
}

fn animated_format(path: &Path) -> Option<AnimatedFormat> {
    let extension = path.extension()?;
    if extension.eq_ignore_ascii_case("webp") {
        Some(AnimatedFormat::Webp)
    } else if extension.eq_ignore_ascii_case("gif") {
        Some(AnimatedFormat::Gif)
    } else {
        None
    }
}

/// The timing of `path` when it is an animated WebP or GIF, or `None` for any other picture. Reads
/// the file's chunks or blocks but decodes no frame, so it is cheap next to [`read_animation`].
pub fn animation_timing(path: &Path) -> Result<Option<AnimationTiming>, String> {
    let Some(format) = animated_format(path) else { return Ok(None) };
    let shown = path.display();
    if format == AnimatedFormat::Gif {
        let file = File::open(path).map_err(|e| format!("{shown}: {e}"))?;
        let Some((_, _, delays, loops)) = crate::animated_gif::timing(BufReader::new(file)).map_err(|e| format!("{shown}: {e}"))? else { return Ok(None) };
        return Ok(Some(AnimationTiming { loop_ms: delays.into_iter().map(frame_ms).sum(), loops }));
    }
    let data = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
    let Some(animation) = AnimatedWebp::parse(&data).map_err(|e| format!("{shown}: {e}"))? else { return Ok(None) };
    let loop_ms = animation.durations_ms().map(frame_ms).sum();
    Ok(Some(AnimationTiming { loop_ms, loops: animation.loops }))
}

/// The frames of `path` when it is an animated WebP or GIF, or `None` for any other picture, which
/// is a still. Each frame is the whole canvas with the frames before it laid under it, as libwebp
/// and browsers lay them (see [`crate::animated_webp`] and [`crate::animated_gif`]).
pub fn read_animation(path: &Path) -> Result<Option<AnimatedPicture>, String> {
    let shown = path.display();
    let Some(format) = animated_format(path) else { return Ok(None) };
    if format == AnimatedFormat::Gif {
        return read_gif_animation(path);
    }
    let data = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
    let Some(animation) = AnimatedWebp::parse(&data).map_err(|e| format!("{shown}: {e}"))? else { return Ok(None) };
    check_pixels(path, (animation.width, animation.height))?;
    if animation.width.max(animation.height) > MAX_ANIMATION_SIDE {
        return Err(format!("{shown}: {}x{} is larger than the {MAX_ANIMATION_SIDE} pixels a side an animated picture may have", animation.width, animation.height));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODER_SIDE_LIMIT);
    limits.max_image_height = Some(DECODER_SIDE_LIMIT);
    limits.max_alloc = Some(DECODER_ALLOCATION);
    let mut picture = AnimatedPicture { frames: Vec::new(), ends_ms: Vec::new(), loops: animation.loops };
    let (mut bytes, mut end) = (0u64, 0u64);
    for frame in animation.frames(limits) {
        let frame = frame.map_err(|e| format!("{shown}: {e}"))?;
        end += frame_ms(frame.duration_ms);
        let canvas = RgbaImage::from_raw(animation.width, animation.height, frame.rgba).ok_or_else(|| format!("{shown}: a frame of the wrong size"))?;
        let still = finished_picture(canvas);
        bytes += still.rgba.len() as u64;
        if bytes > MAX_ANIMATION_BYTES {
            return Err(format!("{shown}: its frames take more than the {MAX_ANIMATION_BYTES} bytes an animated picture may have"));
        }
        picture.frames.push(still);
        picture.ends_ms.push(end);
    }
    // Whether a picture has alpha picks how it is drawn, so every frame says the same.
    let has_alpha = picture.frames.iter().any(|frame| frame.has_alpha);
    for frame in &mut picture.frames {
        frame.has_alpha = has_alpha;
    }
    Ok(Some(picture))
}

/// [`read_animation`] for a GIF: its blocks are read once for the size and the frame count, so a
/// GIF of one frame is a still and a canvas too large is refused before any pixel is decoded.
fn read_gif_animation(path: &Path) -> Result<Option<AnimatedPicture>, String> {
    let shown = path.display();
    let data = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
    let Some((width, height, _, loops)) = crate::animated_gif::timing(data.as_slice()).map_err(|e| format!("{shown}: {e}"))? else { return Ok(None) };
    check_pixels(path, (width, height))?;
    if width.max(height) > MAX_ANIMATION_SIDE {
        return Err(format!("{shown}: {width}x{height} is larger than the {MAX_ANIMATION_SIDE} pixels a side an animated picture may have"));
    }
    let mut picture = AnimatedPicture { frames: Vec::new(), ends_ms: Vec::new(), loops };
    let (mut bytes, mut end) = (0u64, 0u64);
    crate::animated_gif::frames(data.as_slice(), DECODER_ALLOCATION, |frame| {
        end += frame_ms(frame.duration_ms);
        let canvas = RgbaImage::from_raw(width, height, frame.rgba).ok_or_else(|| "a frame of the wrong size".to_owned())?;
        let still = finished_picture(canvas);
        bytes += still.rgba.len() as u64;
        if bytes > MAX_ANIMATION_BYTES {
            return Err(format!("its frames take more than the {MAX_ANIMATION_BYTES} bytes an animated picture may have"));
        }
        picture.frames.push(still);
        picture.ends_ms.push(end);
        Ok(())
    })
    .map_err(|e| format!("{shown}: {e}"))?;
    // Whether a picture has alpha picks how it is drawn, so every frame says the same.
    let has_alpha = picture.frames.iter().any(|frame| frame.has_alpha);
    for frame in &mut picture.frames {
        frame.has_alpha = has_alpha;
    }
    Ok(Some(picture))
}

/// Bounds on what [`animation_thumbnail`] keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailLimits {
    /// The height of each frame; the width keeps the canvas's shape.
    pub height: u32,
    /// The most frames kept; the loop ends with the last one kept.
    pub max_frames: usize,
    /// The most bytes the kept frames may take together; the loop ends before the frame past it.
    pub max_bytes: u64,
}

/// Small frames of `path` for looking at, when it is an animated WebP or GIF, or `None` for any
/// other picture. Each frame is shrunk as soon as it is decoded, so only one canvas is ever full
/// size, and the loop is cut short at `limits` rather than refused. `cancelled` is asked between
/// frames; when it says true, the work stops with `Ok(None)`.
pub fn animation_thumbnail(path: &Path, limits: ThumbnailLimits, cancelled: &dyn Fn() -> bool) -> Result<Option<AnimatedPicture>, String> {
    let shown = path.display();
    let Some(format) = animated_format(path) else { return Ok(None) };
    let data = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
    let mut thumbnail;
    if format == AnimatedFormat::Gif {
        let Some((width, height, _, loops)) = crate::animated_gif::timing(data.as_slice()).map_err(|e| format!("{shown}: {e}"))? else { return Ok(None) };
        check_animation_size(path, width, height)?;
        thumbnail = Thumbnail::new(width, height, loops, limits);
        let mut stopped = false;
        let mut was_cancelled = false;
        let result = crate::animated_gif::frames(data.as_slice(), DECODER_ALLOCATION, |frame| {
            if cancelled() {
                was_cancelled = true;
            } else if thumbnail.add(frame.rgba, frame.duration_ms)? {
                return Ok(());
            }
            // The decoder has no way to stop but an error; this one is not shown.
            stopped = true;
            Err(String::new())
        });
        if was_cancelled {
            return Ok(None);
        }
        if !stopped {
            result.map_err(|e| format!("{shown}: {e}"))?;
        }
    } else {
        let Some(animation) = AnimatedWebp::parse(&data).map_err(|e| format!("{shown}: {e}"))? else { return Ok(None) };
        check_animation_size(path, animation.width, animation.height)?;
        thumbnail = Thumbnail::new(animation.width, animation.height, animation.loops, limits);
        let mut decoder_limits = Limits::default();
        decoder_limits.max_image_width = Some(DECODER_SIDE_LIMIT);
        decoder_limits.max_image_height = Some(DECODER_SIDE_LIMIT);
        decoder_limits.max_alloc = Some(DECODER_ALLOCATION);
        for frame in animation.frames(decoder_limits) {
            if cancelled() {
                return Ok(None);
            }
            let frame = frame.map_err(|e| format!("{shown}: {e}"))?;
            if !thumbnail.add(frame.rgba, frame.duration_ms).map_err(|e| format!("{shown}: {e}"))? {
                break;
            }
        }
    }
    let picture = thumbnail.picture;
    if picture.frames.is_empty() {
        return Err(format!("{shown}: no frame"));
    }
    Ok(Some(picture))
}

/// Refuses an animated canvas with too many pixels or too long a side.
fn check_animation_size(path: &Path, width: u32, height: u32) -> Result<(), String> {
    check_pixels(path, (width, height))?;
    if width.max(height) > MAX_ANIMATION_SIDE {
        return Err(format!("{}: {width}x{height} is larger than the {MAX_ANIMATION_SIDE} pixels a side an animated picture may have", path.display()));
    }
    Ok(())
}

/// The frames [`animation_thumbnail`] has kept so far, shrunk to the size `limits` asks for.
struct Thumbnail {
    canvas: (u32, u32),
    size: (u32, u32),
    limits: ThumbnailLimits,
    bytes: u64,
    end: u64,
    picture: AnimatedPicture,
}

impl Thumbnail {
    fn new(width: u32, height: u32, loops: u32, limits: ThumbnailLimits) -> Self {
        let small_height = limits.height.clamp(1, height.max(1));
        let aspect = f64::from(width) / f64::from(height.max(1));
        let small_width = ((aspect * f64::from(small_height)).round() as u32).max(1);
        Self { canvas: (width, height), size: (small_width, small_height), limits, bytes: 0, end: 0, picture: AnimatedPicture { frames: Vec::new(), ends_ms: Vec::new(), loops } }
    }

    /// Shrinks and keeps one full-canvas frame; false when the frame was not kept because a limit
    /// is reached, so the loop ends.
    fn add(&mut self, rgba: Vec<u8>, duration_ms: u32) -> Result<bool, String> {
        if self.picture.frames.len() >= self.limits.max_frames.max(1) {
            return Ok(false);
        }
        let (width, height) = self.size;
        let canvas = RgbaImage::from_raw(self.canvas.0, self.canvas.1, rgba).ok_or_else(|| "a frame of the wrong size".to_owned())?;
        let small = if (width, height) == canvas.dimensions() { canvas } else { image::imageops::resize(&canvas, width, height, FilterType::Triangle) };
        let rgba = small.into_raw();
        if !self.picture.frames.is_empty() && self.bytes + rgba.len() as u64 > self.limits.max_bytes {
            return Ok(false);
        }
        self.bytes += rgba.len() as u64;
        self.end += frame_ms(duration_ms);
        let has_alpha = rgba.chunks_exact(4).any(|pixel| pixel[3] != 255);
        self.picture.frames.push(StillPicture { width, height, rgba, has_alpha });
        self.picture.ends_ms.push(self.end);
        Ok(true)
    }
}

/// The bytes of `rgba`, tight rows, in rows of `aligned_row` bytes as a copy to a texture needs.
pub(crate) fn aligned_rows(rgba: &[u8], row_bytes: usize, aligned_row: usize) -> Vec<u8> {
    let mut aligned = vec![0u8; rgba.len() / row_bytes.max(1) * aligned_row];
    for (target, source) in aligned.chunks_exact_mut(aligned_row).zip(rgba.chunks_exact(row_bytes)) {
        target[..row_bytes].copy_from_slice(source);
    }
    aligned
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::webp::WebPEncoder, ExtendedColorType, ImageEncoder, Rgba};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-still-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn checker() -> RgbaImage {
        // Left column: opaque red. Middle: half-transparent green. Right: transparent, colour junk.
        RgbaImage::from_fn(3, 2, |x, _| match x {
            0 => Rgba([200, 0, 0, 255]),
            1 => Rgba([0, 200, 0, 128]),
            _ => Rgba([1, 2, 3, 0]),
        })
    }

    #[test]
    fn a_png_with_alpha_decodes_to_straight_rgba_and_says_it_has_alpha() {
        let dir = scratch("png");
        let path = dir.join("a.png");
        checker().save(&path).unwrap();
        let picture = read_still(&path).unwrap();
        assert_eq!((picture.width, picture.height), (3, 2));
        assert!(picture.has_alpha);
        assert_eq!(&picture.rgba[..4], &[200, 0, 0, 255]);
        assert_eq!(&picture.rgba[4..8], &[0, 200, 0, 128], "straight, not premultiplied");
        assert_eq!(picture.rgba[8 + 3], 0, "still transparent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_colour_of_a_transparent_texel_comes_from_its_nearest_visible_one() {
        let mut picture = RgbaImage::from_fn(5, 1, |x, _| if x == 0 { Rgba([200, 10, 20, 255]) } else { Rgba([0, 0, 0, 0]) });
        bleed_into_transparent(&mut picture, 5, 1);
        for x in 1..5 {
            assert_eq!(picture.get_pixel(x, 0).0, [200, 10, 20, 0], "x={x}, even a texel several away");
        }
    }

    #[test]
    fn only_the_edge_of_what_is_visible_starts_the_bleed() {
        // A 100 x 100 opaque block in a 110 x 110 picture: its 396 border pixels, not its 10 000 pixels.
        let visible: Vec<bool> = (0..110 * 110).map(|i: usize| (5..105).contains(&(i % 110)) && (5..105).contains(&(i / 110))).collect();
        assert_eq!(edge_of_visible(&visible, 110, 110).len(), 396);
    }

    #[test]
    fn a_bleed_from_the_edge_still_reaches_everything_from_the_nearest_colour() {
        let mut picture = RgbaImage::from_fn(9, 1, |x, _| match x {
            0..=2 => Rgba([200, 0, 0, 255]),
            7 | 8 => Rgba([0, 0, 200, 255]),
            _ => Rgba([0, 0, 0, 0]),
        });
        bleed_into_transparent(&mut picture, 9, 1);
        assert_eq!(picture.get_pixel(3, 0).0, [200, 0, 0, 0]);
        assert_eq!(picture.get_pixel(6, 0).0, [0, 0, 200, 0]);
        assert_eq!(picture.get_pixel(1, 0).0, [200, 0, 0, 255], "what was visible stays");
    }

    #[test]
    fn a_thumbnail_does_not_bleed_or_go_through_the_decode_for_clips() {
        let dir = scratch("thumbnail");
        let path = dir.join("a.png");
        RgbaImage::from_fn(2, 1, |x, _| if x == 0 { Rgba([200, 0, 0, 255]) } else { Rgba([0, 0, 240, 0]) }).save(&path).unwrap();
        let thumbnail = still_thumbnail(&path, 1).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (2, 1));
        assert_eq!(thumbnail.rgba[4..8], [0, 0, 240, 0], "the junk colour would be red after a bleed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_with_nothing_visible_is_left_alone() {
        let mut picture = RgbaImage::from_pixel(2, 2, Rgba([9, 8, 7, 0]));
        bleed_into_transparent(&mut picture, 2, 2);
        assert_eq!(picture.get_pixel(1, 1).0, [9, 8, 7, 0]);
    }

    #[test]
    fn jpeg_and_webp_decode_and_a_jpeg_has_no_alpha() {
        let dir = scratch("formats");
        let path = dir.join("a.jpg");
        image::DynamicImage::ImageRgba8(checker()).to_rgb8().save(&path).unwrap();
        let picture = read_still(&path).unwrap();
        assert_eq!((picture.width, picture.height, picture.has_alpha), (3, 2, false));

        let webp = dir.join("a.webp");
        let bytes = checker();
        WebPEncoder::new_lossless(std::fs::File::create(&webp).unwrap()).write_image(bytes.as_raw(), 3, 2, ExtendedColorType::Rgba8).unwrap();
        let picture = read_still(&webp).unwrap();
        assert!(picture.has_alpha);
        assert_eq!(&picture.rgba[4..8], &[0, 200, 0, 128], "lossless webp keeps the straight alpha");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_picture_is_shrunk_keeping_its_shape_and_a_huge_one_is_refused() {
        assert_eq!(fitted_size(100, 50, 4096), (100, 50));
        assert_eq!(fitted_size(8192, 4096, 4096), (4096, 2048));
        assert_eq!(fitted_size(40_000, 1, 4096), (4096, 1), "never below one row");
        let dir = scratch("large");
        let path = dir.join("wide.png");
        RgbaImage::from_pixel(5000, 100, Rgba([1, 2, 3, 255])).save(&path).unwrap();
        let picture = read_still(&path).unwrap();
        assert_eq!((picture.width, picture.height), (4096, 82));
        let refused = dir.join("huge.png");
        std::fs::write(&refused, png_header(20_000, 20_000)).unwrap();
        assert!(read_still(&refused).unwrap_err().contains("20000x20000"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn animated_fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/animated.webp")
    }

    #[test]
    fn an_animation_thumbnail_is_small_keeps_the_timing_and_stops_at_its_limits() {
        for fixture in [animated_fixture(), gif_fixture()] {
            let full = read_animation(&fixture).unwrap().expect("animated");
            let limits = ThumbnailLimits { height: 2, max_frames: 64, max_bytes: u64::MAX };
            let small = animation_thumbnail(&fixture, limits, &|| false).unwrap().expect("animated");
            assert_eq!(small.ends_ms, full.ends_ms, "{}", fixture.display());
            assert_eq!(small.loops, full.loops);
            assert!(small.frames.iter().all(|frame| frame.height == 2 && frame.rgba.len() == (frame.width * 2 * 4) as usize));
            let one = animation_thumbnail(&fixture, ThumbnailLimits { max_frames: 1, ..limits }, &|| false).unwrap().expect("animated");
            assert_eq!(one.ends_ms, full.ends_ms[..1]);
            let tight = animation_thumbnail(&fixture, ThumbnailLimits { max_bytes: 1, ..limits }, &|| false).unwrap().expect("animated");
            assert_eq!(tight.frames.len(), 1, "the first frame is always kept");
            assert_eq!(animation_thumbnail(&fixture, limits, &|| true).unwrap(), None);
        }
    }

    #[test]
    fn an_animated_webp_decodes_every_frame_with_its_end() {
        let picture = read_animation(&animated_fixture()).unwrap().expect("animated");
        assert_eq!(picture.ends_ms, [100, 300, 600]);
        assert_eq!(picture.frames.len(), 3);
        for (frame, colour) in picture.frames.iter().zip([[255, 0, 0], [0, 255, 0], [0, 0, 255]]) {
            assert_eq!((frame.width, frame.height), (4, 4));
            assert!(frame.has_alpha, "the first frame has a hole, so every frame says it has alpha");
            // The encoder that made the fixture may move a channel by one, even lossless.
            let close = frame.rgba[4..7].iter().zip(colour).all(|(got, want)| got.abs_diff(want) <= 2);
            assert!(close && frame.rgba[7] == 255, "{:?} is not {colour:?}", &frame.rgba[4..8]);
        }
        assert_eq!(picture.frames[0].rgba[3], 0, "the hole of the first frame stays transparent");
        assert_eq!(picture.frames[0].rgba[..3], [255, 0, 0], "and takes the colour next to it");
    }

    #[test]
    fn the_timing_of_an_animated_webp_matches_its_decoded_frames_without_decoding_them() {
        let timing = animation_timing(&animated_fixture()).unwrap().expect("animated");
        let picture = read_animation(&animated_fixture()).unwrap().expect("animated");
        assert_eq!(timing, AnimationTiming { loop_ms: *picture.ends_ms.last().unwrap(), loops: picture.loops });
        assert_eq!(timing.loop_ms, 600);
        let dir = scratch("timing");
        let still = dir.join("a.png");
        checker().save(&still).unwrap();
        assert_eq!(animation_timing(&still), Ok(None), "a png is not animated");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frames_over_part_of_the_canvas_blend_and_dispose_as_libwebp_does() {
        // What Pillow (libwebp) shows for the fixture; see make_animated_webp_subframes.py. A frame
        // disposed to the background leaves its square transparent, not the white the file names.
        let points = [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (7, 7), (6, 1)];
        let (red, purple, green, clear, yellow) = ([255, 0, 0, 255], [126, 0, 127, 255], [0, 255, 0, 255], [0, 0, 0, 0], [255, 255, 0, 128]);
        let expected = [
            [red; 8],
            [red, red, purple, purple, purple, purple, red, red],
            [red, red, clear, clear, clear, green, green, red],
            [yellow, yellow, clear, clear, clear, green, green, red],
        ];
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/animated_subframes.webp");
        let picture = read_animation(&path).unwrap().expect("animated");
        assert_eq!(picture.frames.len(), 4);
        for (index, (frame, expected)) in picture.frames.iter().zip(expected).enumerate() {
            for ((x, y), want) in points.into_iter().zip(expected) {
                let at = (y * frame.width as usize + x) * 4;
                let got = &frame.rgba[at..at + 4];
                // A transparent pixel takes the colour next to it, so only its alpha counts.
                let channels = if want[3] == 0 { 3..4 } else { 0..4 };
                let close = channels.clone().all(|c| got[c].abs_diff(want[c]) <= 2);
                assert!(close, "frame {index} at ({x}, {y}): {got:?} is not {want:?}");
            }
        }
    }

    fn gif_fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/animated_subframes.gif")
    }

    #[test]
    fn gif_frames_over_part_of_the_canvas_dispose_as_browsers_do() {
        // See make_animated_gif.py: frame 1 is restored to what was before it, frame 2 is cleared to
        // transparent (not the background colour of the file) and its transparent pixel shows red.
        let points = [(0, 0), (2, 2), (4, 4), (5, 5), (7, 7), (6, 1)];
        let (red, green, blue, yellow, clear) = ([255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255], [0, 0, 0, 0]);
        let expected = [
            [red; 6],
            [red, green, green, green, red, red],
            [red, red, red, blue, blue, red],
            [yellow, red, clear, clear, clear, red],
        ];
        let picture = read_animation(&gif_fixture()).unwrap().expect("animated");
        assert_eq!(picture.frames.len(), 4);
        assert_eq!(picture.ends_ms, [100, 200, 400, 500], "delays of 0 and 1/100 s last 100 ms");
        assert_eq!(picture.loops, 3, "a NETSCAPE loop count of 2 repeats twice after the first play");
        for (index, (frame, expected)) in picture.frames.iter().zip(expected).enumerate() {
            assert_eq!((frame.width, frame.height), (8, 8));
            assert!(frame.has_alpha, "frame 3 has a hole, so every frame says it has alpha");
            for ((x, y), want) in points.into_iter().zip(expected) {
                let at = (y * frame.width as usize + x) * 4;
                let got = &frame.rgba[at..at + 4];
                let channels = if want[3] == 0 { 3..4 } else { 0..4 };
                assert!(channels.clone().all(|c| got[c] == want[c]), "frame {index} at ({x}, {y}): {got:?} is not {want:?}");
            }
        }
    }

    #[test]
    fn the_timing_of_an_animated_gif_matches_its_decoded_frames() {
        let timing = animation_timing(&gif_fixture()).unwrap().expect("animated");
        assert_eq!(timing, AnimationTiming { loop_ms: 500, loops: 3 });
    }

    #[test]
    fn a_gif_of_one_frame_is_a_still_and_its_first_frame_is_its_still() {
        let dir = scratch("gif-still");
        let path = dir.join("one.gif");
        let bytes = std::fs::read(gif_fixture()).unwrap();
        // Everything before the second frame's control block, then the trailer.
        let first_end = bytes.windows(3).enumerate().filter(|(_, w)| *w == [0x21, 0xF9, 0x04]).nth(1).unwrap().0;
        let mut one = bytes[..first_end].to_vec();
        one.push(0x3B);
        std::fs::write(&path, one).unwrap();
        assert_eq!(read_animation(&path).unwrap(), None);
        assert_eq!(animation_timing(&path), Ok(None));
        let still = read_still(&gif_fixture()).unwrap();
        assert_eq!((still.width, still.height), (8, 8));
        assert_eq!(&still.rgba[..4], &[255, 0, 0, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fixture with the loop count of its ANIM chunk set to `loops`, in a new file.
    fn fixture_looping(dir: &Path, loops: u16) -> std::path::PathBuf {
        let mut bytes = std::fs::read(animated_fixture()).unwrap();
        let at = bytes.windows(4).position(|window| window == b"ANIM").expect("an ANIM chunk") + 8 + 4;
        bytes[at..at + 2].copy_from_slice(&loops.to_le_bytes());
        let path = dir.join(format!("loops-{loops}.webp"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn an_animation_that_loops_a_number_of_times_then_holds_its_last_frame() {
        let dir = scratch("loop-count");
        let ms = |ms: i64| ms * 90;
        let twice = read_animation(&fixture_looping(&dir, 2)).unwrap().expect("animated");
        assert_eq!(twice.frame_index_at(ms(700)), 1, "the second loop plays");
        assert_eq!(twice.frame_index_at(ms(1_200)), 2, "after two loops of 600 ms the last frame stays");
        assert_eq!(twice.frame_index_at(ms(60_000)), 2);
        let forever = read_animation(&fixture_looping(&dir, 0)).unwrap().expect("animated");
        assert_eq!(forever.frame_index_at(ms(1_200)), 0, "0 loops forever");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_still_webp_with_junk_after_it_or_a_long_riff_size_is_a_still() {
        let dir = scratch("junk");
        let path = dir.join("junk.webp");
        let mut bytes = Vec::new();
        let image = checker();
        WebPEncoder::new_lossless(&mut bytes).write_image(image.as_raw(), 3, 2, ExtendedColorType::Rgba8).unwrap();
        let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        bytes[4..8].copy_from_slice(&(riff + 1000).to_le_bytes());
        bytes.extend_from_slice(b"JUNK__");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_animation(&path).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_animation_with_a_canvas_past_the_animation_limit_is_refused() {
        let dir = scratch("huge-animation");
        let path = dir.join("huge.webp");
        let side = MAX_ANIMATION_SIDE + 2;
        let mut vp8x = vec![0b10u8, 0, 0, 0];
        vp8x.extend_from_slice(&(side - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(side - 1).to_le_bytes()[..3]);
        let mut body = b"WEBPVP8X".to_vec();
        body.extend_from_slice(&10u32.to_le_bytes());
        body.extend_from_slice(&vp8x);
        body.extend_from_slice(b"ANMF");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&[0; 16]);
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        std::fs::write(&path, &bytes).unwrap();
        let error = read_animation(&path).unwrap_err();
        assert!(error.contains(&format!("{side}x{side}")), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_animated_frame_is_picked_by_time_and_the_loop_repeats() {
        let picture = read_animation(&animated_fixture()).unwrap().expect("animated");
        let ms = |ms: i64| ms * 90;
        for (time, index) in [(0, 0), (99, 0), (100, 1), (299, 1), (300, 2), (599, 2), (600, 0), (700, 1), (6_000 + 350, 2)] {
            assert_eq!(picture.frame_index_at(ms(time)), index, "at {time} ms");
        }
        assert_eq!(picture.frame_index_at(-5), 0, "before the clip is the first frame");
    }

    #[test]
    fn a_still_webp_and_a_png_are_not_animations() {
        let dir = scratch("not-animated");
        let webp = dir.join("a.webp");
        let bytes = checker();
        WebPEncoder::new_lossless(std::fs::File::create(&webp).unwrap()).write_image(bytes.as_raw(), 3, 2, ExtendedColorType::Rgba8).unwrap();
        assert_eq!(read_animation(&webp).unwrap(), None);
        let png = dir.join("a.png");
        checker().save(&png).unwrap();
        assert_eq!(read_animation(&png).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_frame_of_an_animated_webp_is_its_still() {
        let picture = read_still(&animated_fixture()).unwrap();
        assert_eq!(picture.rgba[4..8], [255, 0, 0, 255]);
        assert_eq!(still_dimensions(&animated_fixture()).unwrap(), (4, 4));
    }

    /// The start of a PNG with this size in its header: enough for a header read.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&width.to_be_bytes());
        chunk.extend_from_slice(&height.to_be_bytes());
        chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
        let crc = crc32(&chunk);
        bytes.extend_from_slice(&chunk);
        bytes.extend_from_slice(&crc.to_be_bytes());
        // A decoder reads the header up to the first data chunk.
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"IDAT");
        bytes.extend_from_slice(&crc32(b"IDAT").to_be_bytes());
        bytes
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        !crc
    }

    #[test]
    fn a_file_that_is_not_an_image_is_refused_and_named() {
        let dir = scratch("broken");
        let path = dir.join("nope.png");
        std::fs::write(&path, b"not an image at all").unwrap();
        assert!(read_still(&path).unwrap_err().contains("nope.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
