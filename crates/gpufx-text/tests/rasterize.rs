use engine::project::{FontRef, TextAlign, TextBackground, TextContent, TextOutline, TextShadow};
use gpufx_text::{TextImage, TextRasterizer, MAX_DIMENSION, MAX_PIXELS};
use unicode_normalization::UnicodeNormalization;

const HEIGHT: u32 = 720;

fn rasterizer() -> TextRasterizer {
    TextRasterizer::embedded_only()
}

fn alpha(image: &TextImage, x: u32, y: u32) -> u8 {
    image.pixels[((y * image.width + x) * 4 + 3) as usize]
}

fn covered(image: &TextImage) -> usize {
    image.pixels.chunks(4).filter(|p| p[3] > 0).count()
}

/// Leftmost and rightmost column with ink in rows `y0..y1`.
fn ink_columns(image: &TextImage, y0: u32, y1: u32) -> (u32, u32) {
    let mut range = (u32::MAX, 0);
    for y in y0..y1 {
        for x in 0..image.width {
            if alpha(image, x, y) > 64 {
                range = (range.0.min(x), range.1.max(x));
            }
        }
    }
    assert!(range.0 <= range.1, "no ink in rows {y0}..{y1}");
    range
}

#[test]
fn larger_size_gives_a_larger_picture() {
    let mut r = rasterizer();
    let small = r.rasterize(&TextContent::new("Hello", 0.05), HEIGHT, 1.0);
    let large = r.rasterize(&TextContent::new("Hello", 0.1), HEIGHT, 1.0);
    assert!(large.width > small.width * 3 / 2 && large.height > small.height * 3 / 2, "{}x{} vs {}x{}", small.width, small.height, large.width, large.height);
    assert_eq!(small.pixels.len(), (small.width * small.height * 4) as usize);
    let half = r.rasterize(&TextContent::new("Hello", 0.1), HEIGHT, 0.5);
    assert!(half.width < large.width * 2 / 3);
}

#[test]
fn alignment_moves_the_short_line() {
    let mut r = rasterizer();
    let mut text = TextContent::new("I\nIIIIIIIIIIII", 0.08);
    let mut first_line = |align| {
        text.align = align;
        let image = r.rasterize(&text, HEIGHT, 1.0);
        let (l, h) = ink_columns(&image, 0, image.height / 2);
        (l, h, image.width)
    };
    let (ll, lh, w) = first_line(TextAlign::Left);
    let (cl, ch, _) = first_line(TextAlign::Center);
    let (rl, rh, _) = first_line(TextAlign::Right);
    assert!(lh < w / 4, "left: {ll}..{lh} of {w}");
    let centre = (cl + ch) / 2;
    assert!(centre.abs_diff(w / 2) <= 3, "centre: {cl}..{ch} of {w}");
    assert!(rl > w * 3 / 4, "right: {rl}..{rh} of {w}");
}

#[test]
fn outline_covers_more_pixels() {
    let mut r = rasterizer();
    let mut text = TextContent::new("Outline", 0.08);
    let plain = r.rasterize(&text, HEIGHT, 1.0);
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: 0.08 });
    let outlined = r.rasterize(&text, HEIGHT, 1.0);
    assert!(covered(&outlined) > covered(&plain) * 5 / 4, "{} vs {}", covered(&outlined), covered(&plain));
}

#[test]
fn line_feed_makes_more_lines() {
    let mut r = rasterizer();
    let one = r.rasterize(&TextContent::new("Line", 0.06), HEIGHT, 1.0);
    let three = r.rasterize(&TextContent::new("Line\nLine\nLine", 0.06), HEIGHT, 1.0);
    assert!(three.height > one.height * 2, "{} vs {}", three.height, one.height);
    let mut spaced = TextContent::new("Line\nLine\nLine", 0.06);
    spaced.line_spacing = 2.0;
    assert!(r.rasterize(&spaced, HEIGHT, 1.0).height > three.height * 3 / 2);
}

#[test]
fn vietnamese_nfc_and_nfd_draw_alike_with_no_missing_glyph() {
    let mut r = rasterizer();
    let source = "Tiếng Việt: ăâđêôơư ẮẰẲẴẶ ỹỵ";
    let nfc: String = source.nfc().collect();
    let nfd: String = source.nfd().collect();
    assert_ne!(nfc, nfd);
    let a = r.rasterize(&TextContent::new(nfc, 0.06), HEIGHT, 1.0);
    let b = r.rasterize(&TextContent::new(nfd, 0.06), HEIGHT, 1.0);
    assert_eq!(a.missing_glyphs, 0);
    assert_eq!(b.missing_glyphs, 0);
    assert_eq!((a.width, a.height), (b.width, b.height));
    let diff: u64 = a.pixels.iter().zip(&b.pixels).map(|(x, y)| x.abs_diff(*y) as u64).sum();
    let mean = diff as f64 / a.pixels.len() as f64;
    assert!(mean < 2.0, "mean difference {mean}");
    // The check can fail: the built-in font has no Han glyphs.
    assert!(r.rasterize(&TextContent::new("中文", 0.06), HEIGHT, 1.0).missing_glyphs > 0);
}

#[test]
fn unknown_family_falls_back_to_the_default_and_reports_it() {
    let mut r = rasterizer();
    let mut text = TextContent::new("Fallback", 0.06);
    let default = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(default.missing_family, None);
    text.font = FontRef::System { family: "No Such Font Family 1234".into() };
    let fallback = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(fallback.missing_family.as_deref(), Some("No Such Font Family 1234"));
    assert_eq!(fallback.pixels, default.pixels);
    text.font = FontRef::System { family: "be vietnam pro".into() };
    assert_eq!(r.rasterize(&text, HEIGHT, 1.0).missing_family, None);
}

#[test]
fn empty_text_gives_an_empty_picture() {
    let mut r = rasterizer();
    let image = r.rasterize(&TextContent::new("", 0.06), HEIGHT, 1.0);
    assert_eq!((image.width, image.height, image.pixels.len()), (0, 0, 0));
    let zero = r.rasterize(&TextContent::new("x", 0.0), HEIGHT, 1.0);
    assert_eq!(zero.pixels.len(), 0);
    let newlines = r.rasterize(&TextContent::new("\n\n", 0.06), HEIGHT, 1.0);
    assert_eq!(newlines.pixels.len(), (newlines.width * newlines.height * 4) as usize);
}

#[test]
fn background_and_shadow_are_drawn_premultiplied() {
    let mut r = rasterizer();
    let mut text = TextContent::new("Box", 0.06);
    text.background = Some(TextBackground { color: [0, 0, 255, 128], padding: 0.3, radius: 0.2 });
    text.shadow = Some(TextShadow { color: [0, 0, 0, 200], offset: [0.1, 0.1], blur: 0.1 });
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert!(image.pixels.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    // The blue box covers far more than the glyphs do.
    let plain = r.rasterize(&TextContent::new("Box", 0.06), HEIGHT, 1.0);
    assert!(image.width > plain.width && image.height > plain.height);
    let blue = image.pixels.chunks(4).filter(|p| p[2] > 100).count();
    assert!(blue > covered(&plain) * 2, "{blue} vs {}", covered(&plain));
}

#[test]
fn huge_text_is_shrunk_to_the_limits() {
    let mut r = rasterizer();
    let image = r.rasterize(&TextContent::new("WWWWWWWWWWWWWWWWWWWW", 1.0), 4320, 1.0);
    assert!(image.downscaled);
    assert!(image.width <= MAX_DIMENSION && image.height <= MAX_DIMENSION);
    assert!(image.width as u64 * image.height as u64 <= MAX_PIXELS);
    assert!(covered(&image) > 0);
}
