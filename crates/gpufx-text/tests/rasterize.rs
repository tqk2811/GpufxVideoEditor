use engine::project::{FontRef, TextAlign, TextBackground, TextContent, TextOutline, TextShadow};
use gpufx_text::{TextImage, TextRasterizer, DEFAULT_FAMILY_CANDIDATES, MAX_DIMENSION, MAX_OUTLINE_RATIO, MAX_PIXELS};
use unicode_normalization::UnicodeNormalization;

const HEIGHT: u32 = 720;

/// Notes that a test is skipped for want of a font, or fails when `GPUFX_REQUIRE_FONTS=1`, so a
/// machine that must have the fonts cannot pass by skipping.
fn skip(reason: String) {
    if std::env::var("GPUFX_REQUIRE_FONTS").is_ok_and(|value| value == "1") {
        panic!("GPUFX_REQUIRE_FONTS=1 but {reason}");
    }
    eprintln!("skipped: {reason}");
}

/// The system rasterizer when its default font is one of [`DEFAULT_FAMILY_CANDIDATES`], so the
/// checks below run on a known font; otherwise the test is skipped with a note.
fn rasterizer() -> Option<TextRasterizer> {
    let r = TextRasterizer::new();
    if r.default_family().is_some_and(|family| DEFAULT_FAMILY_CANDIDATES.iter().any(|known| known.eq_ignore_ascii_case(family))) {
        Some(r)
    } else {
        skip(format!("none of {DEFAULT_FAMILY_CANDIDATES:?} is installed (default is {:?})", r.default_family()));
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

/// A font file of Windows that is not the default font, or `None` (the test is then skipped).
fn windows_font(name: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(&std::env::var_os("WINDIR")?).join("Fonts").join(name);
    if path.is_file() {
        Some(path)
    } else {
        skip(format!("no {name}"));
        None
    }
}

#[test]
fn a_font_file_draws_with_its_own_family() {
    let Some(mut r) = rasterizer() else { return };
    let Some(path) = windows_font("times.ttf") else { return };
    assert_eq!(gpufx_text::font_file_family(&path).as_deref(), Some("Times New Roman"));
    assert_eq!(r.file_family(&path).as_deref(), Some("Times New Roman"));
    let mut text = TextContent::new("Serif text", 0.06);
    let default = r.rasterize(&text, HEIGHT, 1.0);
    text.font = FontRef::File { path: path.to_string_lossy().into_owned() };
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(image.missing_family, None);
    assert!(!image.no_font && image.width > 0);
    assert_ne!(image.pixels, default.pixels, "drawn in the file's font, not the default");
    assert_eq!(r.missing_font(&text), None);
    assert!(gpufx_text::text_font_stamp(&text).is_some());
}

#[test]
fn a_missing_or_broken_font_file_falls_back_to_the_default_and_names_the_file() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Fallback", 0.06);
    let default = r.rasterize(&text, HEIGHT, 1.0);
    let dir = std::env::temp_dir().join(format!("gpufx-text-font-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let broken = dir.join("broken.ttf");
    std::fs::write(&broken, b"not a font").unwrap();
    for path in [dir.join("absent.ttf"), broken.clone()] {
        let name = path.to_string_lossy().into_owned();
        text.font = FontRef::File { path: name.clone() };
        let image = r.rasterize(&text, HEIGHT, 1.0);
        assert_eq!(image.missing_family.as_deref(), Some(name.as_str()));
        assert_eq!(image.pixels, default.pixels);
        assert_eq!(r.missing_font(&text).as_deref(), Some(name.as_str()));
        assert_eq!(gpufx_text::font_file_family(&path), None);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_font_file_that_changes_is_read_again() {
    let Some(mut r) = rasterizer() else { return };
    let (Some(times), Some(arial)) = (windows_font("times.ttf"), windows_font("arial.ttf")) else { return };
    let dir = std::env::temp_dir().join(format!("gpufx-text-font-change-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("font.ttf");
    std::fs::copy(&times, &file).unwrap();
    assert_eq!(r.file_family(&file).as_deref(), Some("Times New Roman"));
    std::fs::copy(&arial, &file).unwrap();
    assert_eq!(r.file_family(&file).as_deref(), Some("Arial"), "a new length is a new stamp");
    let _ = std::fs::remove_dir_all(&dir);
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
fn glyphs_no_loaded_font_has_are_counted_as_missing() {
    // Only arial.ttf is loaded, so nothing falls back to a font with Han letters.
    let Some(arial) = windows_font("arial.ttf") else { return };
    let mut r = TextRasterizer::with_font_files(&[arial]);
    assert_eq!(r.rasterize(&TextContent::new("中文", 0.06), HEIGHT, 1.0).missing_glyphs, 2);
    assert_eq!(r.rasterize(&TextContent::new("Latin", 0.06), HEIGHT, 1.0).missing_glyphs, 0);
}

#[test]
fn a_font_file_that_is_not_a_candidate_becomes_the_default() {
    let Some(path) = windows_font("times.ttf") else { return };
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

#[test]
fn a_font_file_draws_its_own_face_even_when_another_face_has_the_same_family() {
    // arialbd.ttf says its family is "Arial", like the installed regular Arial and arial.ttf.
    let (Some(regular), Some(bold)) = (windows_font("arial.ttf"), windows_font("arialbd.ttf")) else { return };
    assert_eq!(gpufx_text::font_file_family(&bold).as_deref(), Some("Arial"));
    let reference = |file: &std::path::Path| TextRasterizer::with_font_files(&[file.to_owned()]).rasterize(&TextContent::new("Same family", 0.06), HEIGHT, 1.0).pixels;
    let (regular_pixels, bold_pixels) = (reference(&regular), reference(&bold));
    assert!(regular_pixels != bold_pixels);
    let clip = |file: &std::path::Path| {
        let mut text = TextContent::new("Same family", 0.06);
        text.font = FontRef::File { path: file.to_string_lossy().into_owned() };
        text
    };
    // Next to the installed fonts, and next to another file of the same family loaded first.
    let mut system = TextRasterizer::new();
    assert!(system.rasterize(&clip(&bold), HEIGHT, 1.0).pixels == bold_pixels, "beside the system Arial");
    let mut files = TextRasterizer::with_font_files(&[]);
    assert!(files.rasterize(&clip(&regular), HEIGHT, 1.0).pixels == regular_pixels);
    assert!(files.rasterize(&clip(&bold), HEIGHT, 1.0).pixels == bold_pixels, "beside arial.ttf");
    assert!(files.rasterize(&clip(&regular), HEIGHT, 1.0).pixels == regular_pixels);
    // The renamed faces stay out of the family list.
    assert!(files.families().is_empty());
    assert!(!system.families().iter().any(|name| name.starts_with("gpufx-file:")));
}

/// A copy of `source` in a new folder with every `from` in its name table, in UTF-16 and in
/// ASCII, replaced by `to` of the same length, so the copy claims another family.
fn renamed_font_copy(source: &std::path::Path, from: &str, to: &str, folder: &str) -> std::path::PathBuf {
    assert_eq!(from.len(), to.len());
    let mut data = std::fs::read(source).unwrap();
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    for (old, new) in [(utf16(from), utf16(to)), (from.as_bytes().to_vec(), to.as_bytes().to_vec())] {
        let mut i = 0;
        while i + old.len() <= data.len() {
            if data[i..i + old.len()] == old[..] {
                data[i..i + old.len()].copy_from_slice(&new);
                i += old.len();
            } else {
                i += 1;
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("{folder}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("renamed.ttf");
    std::fs::write(&path, data).unwrap();
    path
}

#[test]
fn a_font_file_with_the_family_and_weight_of_an_installed_font_draws_its_own_glyphs() {
    let Some(mut r) = rasterizer() else { return };
    let (Some(georgia), Some(_)) = (windows_font("georgia.ttf"), windows_font("verdana.ttf")) else { return };
    let file = renamed_font_copy(&georgia, "Georgia", "Verdana", "gpufx-text-font-clash");
    assert_eq!(gpufx_text::font_file_family(&file).as_deref(), Some("Verdana"), "the copy claims the installed family");
    let mut text = TextContent::new("Same family", 0.06);
    let georgia_pixels = TextRasterizer::with_font_files(&[georgia]).rasterize(&text, HEIGHT, 1.0).pixels;
    text.font = FontRef::System { family: "Verdana".into() };
    let verdana_pixels = r.rasterize(&text, HEIGHT, 1.0).pixels;
    assert!(georgia_pixels != verdana_pixels);
    text.font = FontRef::File { path: file.to_string_lossy().into_owned() };
    assert!(r.rasterize(&text, HEIGHT, 1.0).pixels == georgia_pixels, "drawn with the file, not the installed Verdana");
    text.font = FontRef::System { family: "Verdana".into() };
    assert!(r.rasterize(&text, HEIGHT, 1.0).pixels == verdana_pixels, "the installed Verdana is not replaced by the file");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn bold_text_has_more_ink_than_regular() {
    let Some(mut r) = rasterizer() else { return };
    let regular = r.rasterize(&TextContent::new("Hello world", 0.1), HEIGHT, 1.0);
    let mut text = TextContent::new("Hello world", 0.1);
    text.weight = 700;
    let bold = r.rasterize(&text, HEIGHT, 1.0);
    assert!(covered(&bold) > covered(&regular) * 11 / 10, "bold {} vs regular {}", covered(&bold), covered(&regular));
    text.weight = 650;
    assert_eq!(r.rasterize(&text, HEIGHT, 1.0), bold, "a weight the font lacks takes the nearest face by the CSS rules");
}

#[test]
fn italic_text_differs_from_upright() {
    let Some(mut r) = rasterizer() else { return };
    let upright = r.rasterize(&TextContent::new("Hello", 0.1), HEIGHT, 1.0);
    let mut text = TextContent::new("Hello", 0.1);
    text.italic = true;
    let italic = r.rasterize(&text, HEIGHT, 1.0);
    assert_ne!(italic.pixels, upright.pixels);
    assert_eq!(italic.missing_family, None);
}

#[test]
fn uppercase_draws_like_capital_letters() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Hello", 0.1);
    text.transform = engine::TextTransform::Uppercase;
    assert_eq!(r.rasterize(&text, HEIGHT, 1.0), r.rasterize(&TextContent::new("HELLO", 0.1), HEIGHT, 1.0));
    text.transform = engine::TextTransform::Lowercase;
    assert_eq!(r.rasterize(&text, HEIGHT, 1.0), r.rasterize(&TextContent::new("hello", 0.1), HEIGHT, 1.0));
}

#[test]
fn letter_spacing_widens_the_text() {
    let Some(mut r) = rasterizer() else { return };
    let plain = r.measure(&TextContent::new("Hello", 0.1), HEIGHT, 1.0);
    let mut text = TextContent::new("Hello", 0.1);
    text.letter_spacing = 0.2;
    let wide = r.measure(&text, HEIGHT, 1.0);
    // Five letters, 72 px font: about 0.2 * 72 more after each.
    assert!(wide.0 >= plain.0 + 50, "{wide:?} vs {plain:?}");
    text.letter_spacing = -0.05;
    assert!(r.measure(&text, HEIGHT, 1.0).0 < plain.0);
    text.letter_spacing = f32::NAN;
    assert_eq!(r.measure(&text, HEIGHT, 1.0), plain, "a spacing that is not a number is none");
}

fn glow(radius: f32, intensity: f32) -> engine::TextGlow {
    engine::TextGlow { color: [0, 255, 255, 255], outer_color: None, radius, intensity, falloff: 1.0 }
}

#[test]
fn a_glow_without_intensity_or_reach_is_no_glow() {
    let Some(mut r) = rasterizer() else { return };
    let plain = r.rasterize(&TextContent::new("Glow", 0.1), HEIGHT, 1.0);
    for g in [glow(0.5, 0.0), glow(0.0, 1.0), glow(0.001, 1.0), glow(f32::NAN, 1.0)] {
        let mut text = TextContent::new("Glow", 0.1);
        text.glow = Some(g);
        assert_eq!(r.rasterize(&text, HEIGHT, 1.0), plain, "{g:?}");
    }
}

#[test]
fn a_glow_fades_with_the_distance_from_the_ink_and_grows_the_picture_by_its_reach() {
    let Some(mut r) = rasterizer() else { return };
    let plain = r.rasterize(&TextContent::new("I", 0.2), HEIGHT, 1.0);
    let mut text = TextContent::new("I", 0.2);
    text.glow = Some(glow(0.5, 1.0));
    let image = r.rasterize(&text, HEIGHT, 1.0);
    // 144 px font, half of it: 72 px each side, plus the pixel of room every picture has.
    let grown = image.width - plain.width;
    assert!((140..=150).contains(&grown), "grown by {grown}");
    // Walk left from the stem along the middle row: the glow is cyan and fades out.
    let y = image.height / 2;
    let stem = (0..image.width).find(|&x| alpha(&image, x, y) == 255 && image.pixels[((y * image.width + x) * 4) as usize] == 255).expect("white stem");
    let samples: Vec<u8> = [4, 20, 40, 60].iter().map(|&d| alpha(&image, stem - d, y)).collect();
    assert!(samples.windows(2).all(|pair| pair[0] > pair[1]), "decays: {samples:?}");
    assert!(samples[0] > 180 && samples[3] < 60, "{samples:?}");
    assert_eq!(alpha(&image, 0, y), 0, "nothing past the reach");
    let p = ((y * image.width + stem - 20) * 4) as usize;
    assert_eq!(image.pixels[p], 0, "cyan glow has no red");
    assert!(image.pixels[p + 1] > 0 && image.pixels[p + 1] == image.pixels[p + 2], "premultiplied cyan: {:?}", &image.pixels[p..p + 4]);
}

#[test]
fn a_glow_blends_to_its_outer_colour_and_stays_under_the_shadow_room() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("I", 0.2);
    text.glow = Some(engine::TextGlow { color: [255, 0, 0, 255], outer_color: Some([0, 0, 255, 255]), radius: 0.5, intensity: 4.0, falloff: 0.5 });
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let y = image.height / 2;
    let stem = (0..image.width).find(|&x| alpha(&image, x, y) == 255 && image.pixels[((y * image.width + x) * 4 + 1) as usize] == 255).expect("white stem");
    let at = |x: u32| &image.pixels[((y * image.width + x) * 4) as usize..((y * image.width + x) * 4 + 3) as usize];
    let near = at(stem - 4).to_vec();
    let far = at(stem - 60).to_vec();
    assert!(near[0] > near[2] && far[2] > far[0], "red near, blue far: {near:?} {far:?}");
    // A shadow that reaches farther than the glow sets that side; the glow does not add to it.
    let mut both = text.clone();
    both.shadow = Some(TextShadow { color: [0, 0, 0, 255], offset: [1.0, 0.0], blur: 0.0 });
    let mut shadow_only = TextContent::new("I", 0.2);
    shadow_only.shadow = both.shadow;
    let (both, shadow_only) = (r.rasterize(&both, HEIGHT, 1.0), r.rasterize(&shadow_only, HEIGHT, 1.0));
    let glow_side = (image.width - r.rasterize(&TextContent::new("I", 0.2), HEIGHT, 1.0).width) / 2;
    assert_eq!(both.width, shadow_only.width + glow_side, "right side from the shadow, left side from the glow");
}

#[test]
fn a_glow_too_wide_for_the_picture_is_cut_back_and_never_hides_the_text() {
    let Some(mut r) = rasterizer() else { return };
    // About 2500 px tall letters: the picture fits with no glow, not with a glow of two font sizes.
    let mut text = TextContent::new("W", 2500.0 / 4320.0);
    let (w, h) = r.measure(&text, 4320, 1.0);
    assert!(w <= MAX_DIMENSION && h <= MAX_DIMENSION && w as u64 * h as u64 <= MAX_PIXELS, "{w}x{h} must fit with no glow");
    let plain = r.rasterize(&text, 4320, 1.0);
    text.glow = Some(glow(2.0, 1.0));
    let (gw, gh) = r.measure(&text, 4320, 1.0);
    assert!(gw <= MAX_DIMENSION && gh <= MAX_DIMENSION && gw as u64 * gh as u64 <= MAX_PIXELS, "{gw}x{gh} is cut back to the limits");
    let image = r.rasterize(&text, 4320, 1.0);
    assert!(!image.downscaled, "the glow gives way, not the text size");
    assert!(image.width > 0 && covered(&image) > 0, "the glyphs are still drawn");
    assert!(image.width >= plain.width);
}

#[test]
fn italic_of_a_font_with_no_italic_face_is_not_cut_on_the_right() {
    let Some(mut r) = rasterizer() else { return };
    let Some(path) = windows_font("impact.ttf") else { return };
    let mut text = TextContent::new("IIIIIIIIII", 0.1);
    text.font = FontRef::File { path: path.to_string_lossy().into_owned() };
    text.italic = true;
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert!(image.width > 0 && covered(&image) > 0);
    assert!(!ink_on_edge(&image), "the slanted top of the last letter fits in the picture");
    text.italic = false;
    let upright = r.rasterize(&text, HEIGHT, 1.0);
    assert_ne!(image.pixels, upright.pixels, "the font file is slanted");
}

#[test]
fn letter_spacing_leaves_the_end_of_a_line_out_of_the_alignment() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("I\nIIIIIIIIIIII", 0.08);
    text.letter_spacing = 0.5;
    let mut last_line = |align| {
        text.align = align;
        let image = r.rasterize(&text, HEIGHT, 1.0);
        let (l, h) = ink_columns(&image, image.height / 2, image.height);
        (l, h, image.width)
    };
    let (_, _, w) = last_line(TextAlign::Left);
    let (cl, ch, _) = last_line(TextAlign::Center);
    let (_, rh, _) = last_line(TextAlign::Right);
    // The 12 letters are the widest line, so they fill the picture; a trailing gap of half a font
    // size (about 29 px) would show as an offset.
    assert!(((cl + ch) / 2).abs_diff(w / 2) <= 5, "centre: {cl}..{ch} of {w}");
    assert!(w - rh <= 14, "right: ends at {rh} of {w}");
}
