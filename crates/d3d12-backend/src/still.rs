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

/// Bleeds before it shrinks: the filter mixes neighbouring colours whatever their alpha, so shrinking
/// first would pull the junk colour of transparent texels into the edge of what is visible.
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
