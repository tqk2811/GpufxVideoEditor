use engine::project::{FontRef, TextAlign, TextBackground, TextContent, TextOutline, TextShadow};
use gpufx_text::{TextImage, TextRasterizer, DEFAULT_FAMILY_CANDIDATES, MAX_DIMENSION, MAX_OUTLINE_RATIO, MAX_PIXELS};
use unicode_normalization::UnicodeNormalization;

const HEIGHT: u32 = 720;

/// The system rasterizer when its default font is one of [`DEFAULT_FAMILY_CANDIDATES`], so the
/// checks below run on a known font; otherwise the test is skipped with a note.
fn rasterizer() -> Option<TextRasterizer> {
    let r = TextRasterizer::new();
    if r.default_family().is_some_and(|family| DEFAULT_FAMILY_CANDIDATES.iter().any(|known| known.eq_ignore_ascii_case(family))) {
        Some(r)
    } else {
        eprintln!("skipped: none of {DEFAULT_FAMILY_CANDIDATES:?} is installed (default is {:?})", r.default_family());
        None
    }
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
    let Some(mut r) = rasterizer() else { return };
    let small = r.rasterize(&TextContent::new("Hello", 0.05), HEIGHT, 1.0);
    let large = r.rasterize(&TextContent::new("Hello", 0.1), HEIGHT, 1.0);
    assert!(large.width > small.width * 3 / 2 && large.height > small.height * 3 / 2, "{}x{} vs {}x{}", small.width, small.height, large.width, large.height);
    assert_eq!(small.pixels.len(), (small.width * small.height * 4) as usize);
    let half = r.rasterize(&TextContent::new("Hello", 0.1), HEIGHT, 0.5);
    assert!(half.width < large.width * 2 / 3);
}

#[test]
fn alignment_moves_the_short_line() {
    let Some(mut r) = rasterizer() else { return };
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
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Outline", 0.08);
    let plain = r.rasterize(&text, HEIGHT, 1.0);
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: 0.08 });
    let outlined = r.rasterize(&text, HEIGHT, 1.0);
    assert!(covered(&outlined) > covered(&plain) * 5 / 4, "{} vs {}", covered(&outlined), covered(&plain));
}

/// Small letters have hairlines with no pixel half covered; their outline must still show.
#[test]
fn tiny_letters_still_get_an_outline() {
    let ink = |image: &TextImage| image.pixels.chunks(4).map(|p| p[3] as u64).sum::<u64>();
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("i", 8.0 / HEIGHT as f32);
    let plain = r.rasterize(&text, HEIGHT, 1.0);
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: MAX_OUTLINE_RATIO });
    let outlined = r.rasterize(&text, HEIGHT, 1.0);
    assert!(ink(&outlined) > ink(&plain) * 3, "{} vs {}", ink(&outlined), ink(&plain));
}

#[test]
fn line_feed_makes_more_lines() {
    let Some(mut r) = rasterizer() else { return };
    let one = r.rasterize(&TextContent::new("Line", 0.06), HEIGHT, 1.0);
    let three = r.rasterize(&TextContent::new("Line\nLine\nLine", 0.06), HEIGHT, 1.0);
    assert!(three.height > one.height * 2, "{} vs {}", three.height, one.height);
    let mut spaced = TextContent::new("Line\nLine\nLine", 0.06);
    spaced.line_spacing = 2.0;
    assert!(r.rasterize(&spaced, HEIGHT, 1.0).height > three.height * 3 / 2);
}

#[test]
fn vietnamese_nfc_and_nfd_draw_alike_with_no_missing_glyph() {
    let Some(mut r) = rasterizer() else { return };
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
}

#[test]
fn unknown_family_falls_back_to_the_default_and_reports_it() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Fallback", 0.06);
    let default = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(default.missing_family, None);
    text.font = FontRef::System { family: "No Such Font Family 1234".into() };
    let fallback = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(fallback.missing_family.as_deref(), Some("No Such Font Family 1234"));
    assert_eq!(fallback.pixels, default.pixels);
    text.font = FontRef::System { family: r.default_family().unwrap().to_lowercase() };
    assert_eq!(r.rasterize(&text, HEIGHT, 1.0).missing_family, None);
}

#[test]
fn empty_text_gives_an_empty_picture() {
    let Some(mut r) = rasterizer() else { return };
    let image = r.rasterize(&TextContent::new("", 0.06), HEIGHT, 1.0);
    assert_eq!((image.width, image.height, image.pixels.len()), (0, 0, 0));
    let zero = r.rasterize(&TextContent::new("x", 0.0), HEIGHT, 1.0);
    assert_eq!(zero.pixels.len(), 0);
    let newlines = r.rasterize(&TextContent::new("\n\n", 0.06), HEIGHT, 1.0);
    assert_eq!(newlines.pixels.len(), (newlines.width * newlines.height * 4) as usize);
}

#[test]
fn background_and_shadow_are_drawn_premultiplied() {
    let Some(mut r) = rasterizer() else { return };
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
    let Some(mut r) = rasterizer() else { return };
    let image = r.rasterize(&TextContent::new("WWWWWWWWWWWWWWWWWWWW", 1.0), 4320, 1.0);
    assert!(image.downscaled);
    assert!(image.width <= MAX_DIMENSION && image.height <= MAX_DIMENSION);
    assert!(image.width as u64 * image.height as u64 <= MAX_PIXELS);
    assert!(covered(&image) > 0);
}

#[test]
fn measure_gives_the_size_before_shrinking_and_draws_nothing() {
    let Some(mut r) = rasterizer() else { return };
    let text = TextContent::new("WWWWWWWWWWWWWWWWWWWW", 1.0);
    let (w, h) = r.measure(&text, 4320, 1.0);
    assert!(w > MAX_DIMENSION, "the natural width {w} is not shrunk");
    let image = r.rasterize(&text, 4320, 1.0);
    assert!(image.downscaled && image.width < w && image.height < h);
    let small = TextContent::new("Hi", 0.06);
    let image = r.rasterize(&small, HEIGHT, 1.0);
    assert_eq!(r.measure(&small, HEIGHT, 1.0), (image.width, image.height));
}

/// Whether any pixel on the edge of the picture has ink, which means some was cut off.
fn ink_on_edge(image: &TextImage) -> bool {
    let (w, h) = (image.width, image.height);
    (0..w).any(|x| alpha(image, x, 0) > 0 || alpha(image, x, h - 1) > 0) || (0..h).any(|y| alpha(image, 0, y) > 0 || alpha(image, w - 1, y) > 0)
}

#[test]
fn stacked_marks_on_a_tight_line_spacing_are_not_cut() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Ể Ỗ ẤỤ", 0.1);
    let normal = r.rasterize(&text, HEIGHT, 1.0);
    text.line_spacing = 0.3;
    let tight = r.rasterize(&text, HEIGHT, 1.0);
    assert!(!ink_on_edge(&normal) && !ink_on_edge(&tight));
    let (a, b) = (covered(&normal) as f64, covered(&tight) as f64);
    assert!((a - b).abs() < a * 0.1, "the tight lines cover {b} pixels, the normal ones {a}");
}

#[test]
fn a_wide_outline_is_capped_and_quick() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("HI", 0.5);
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: MAX_OUTLINE_RATIO });
    let capped = r.rasterize(&text, HEIGHT, 1.0);
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: 1.0 });
    let started = std::time::Instant::now();
    let wide = r.rasterize(&text, HEIGHT, 1.0);
    let took = started.elapsed();
    assert_eq!((wide.width, wide.height), (capped.width, capped.height), "an outline wider than the cap draws as the cap");
    assert_eq!(wide.pixels, capped.pixels);
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    // The outline reaches about its width past the letters, and no further.
    let plain = r.rasterize(&TextContent::new("HI", 0.5), HEIGHT, 1.0);
    let font_px = 0.5 * HEIGHT as f32;
    let grown = (font_px * MAX_OUTLINE_RATIO) as u32;
    let (pl, pr) = ink_columns(&plain, 0, plain.height);
    let (wl, wr) = ink_columns(&wide, 0, wide.height);
    let (pw, ww) = (pr - pl, wr - wl);
    assert!(ww + 4 >= pw + 2 * grown && ww <= pw + 2 * grown + 4, "ink {ww} wide, letters {pw}, outline {grown}");
}

#[test]
fn a_blur_under_a_pixel_is_no_blur() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Box", 0.06);
    text.shadow = Some(TextShadow { color: [0, 0, 0, 200], offset: [0.1, 0.1], blur: 0.0 });
    let sharp = r.rasterize(&text, HEIGHT, 1.0);
    text.shadow = Some(TextShadow { color: [0, 0, 0, 200], offset: [0.1, 0.1], blur: 0.3 / (0.06 * HEIGHT as f32) });
    let tiny = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!((tiny.width, tiny.height), (sharp.width, sharp.height));
    assert_eq!(tiny.pixels, sharp.pixels);
}

#[test]
fn tabs_and_joiners_are_not_missing_glyphs() {
    let Some(mut r) = rasterizer() else { return };
    for content in ["a\tb", "a\u{200D}b", "a\u{200C}b", "a\u{FE0F}b", "a\u{200B}b"] {
        let image = r.rasterize(&TextContent::new(content, 0.06), HEIGHT, 1.0);
        assert_eq!(image.missing_glyphs, 0, "{content:?}");
    }
}

#[test]
fn without_any_font_text_is_empty_and_says_so() {
    let mut r = TextRasterizer::with_font_files(&[]);
    assert_eq!(r.default_family(), None);
    assert!(r.families().is_empty());
    let image = r.rasterize(&TextContent::new("Hello", 0.06), HEIGHT, 1.0);
    assert!(image.no_font);
    assert_eq!((image.width, image.height, image.pixels.len()), (0, 0, 0));
    let mut text = TextContent::new("Hello", 0.06);
    text.font = FontRef::System { family: "Arial".into() };
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert!(image.no_font);
    assert_eq!(image.missing_family.as_deref(), Some("Arial"));
}

#[test]
fn a_font_file_that_is_not_a_candidate_becomes_the_default() {
    let Some(path) = std::env::var_os("WINDIR").map(|dir| std::path::Path::new(&dir).join("Fonts").join("times.ttf")).filter(|path| path.is_file()) else {
        eprintln!("skipped: no times.ttf");
        return;
    };
    let mut r = TextRasterizer::with_font_files(&[path]);
    assert_eq!(r.default_family(), Some("Times New Roman"));
    let image = r.rasterize(&TextContent::new("Hello", 0.06), HEIGHT, 1.0);
    assert!(!image.no_font && image.width > 0);
}

#[test]
fn system_rasterizer_lists_every_family_once_sorted_with_the_default_among_them() {
    let Some(system) = rasterizer() else { return };
    let default = system.default_family().unwrap().to_owned();
    let first_installed = DEFAULT_FAMILY_CANDIDATES.iter().find(|family| system.has_family(family)).unwrap();
    assert!(default.eq_ignore_ascii_case(first_installed), "the first installed candidate wins");
    let families = system.families();
    assert!(families.contains(&default));
    assert!(families.iter().all(|name| !name.trim().is_empty()));
    let lower: Vec<String> = families.iter().map(|name| name.to_lowercase()).collect();
    let mut sorted = lower.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(lower, sorted, "sorted ignoring case, no duplicates");
    let mut text = engine::project::TextContent::new("Hello", 0.1);
    text.font = FontRef::System { family: families.last().unwrap().clone() };
    let _ = TextRasterizer::new().rasterize(&text, 360, 1.0);
}
