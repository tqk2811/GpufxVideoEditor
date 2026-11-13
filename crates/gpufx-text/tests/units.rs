use engine::project::{TextContent, TextOutline, TextShadow};
use engine::{AnimationPose, TextUnit};
use gpufx_text::{TextImage, TextRasterizer, DEFAULT_FAMILY_CANDIDATES};

const HEIGHT: u32 = 720;

fn rasterizer() -> Option<TextRasterizer> {
    let r = TextRasterizer::new();
    if r.default_family().is_some_and(|family| DEFAULT_FAMILY_CANDIDATES.iter().any(|known| known.eq_ignore_ascii_case(family))) {
        return Some(r);
    }
    let reason = format!("none of {DEFAULT_FAMILY_CANDIDATES:?} is installed (default is {:?})", r.default_family());
    if std::env::var("GPUFX_REQUIRE_FONTS").is_ok_and(|value| value == "1") {
        panic!("GPUFX_REQUIRE_FONTS=1 but {reason}");
    }
    eprintln!("skipped: {reason}");
    None
}

fn shown(opacities: &[f64]) -> Vec<(TextUnit, Vec<AnimationPose>)> {
    vec![(TextUnit::Char, opacities.iter().map(|&opacity| AnimationPose { opacity, ..AnimationPose::IDENTITY }).collect())]
}

/// The first and last column and row with ink.
fn ink_box(image: &TextImage) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..image.height {
        for x in 0..image.width {
            if image.pixels[((y * image.width + x) * 4 + 3) as usize] > 64 {
                found = Some(found.map_or((x, y, x, y), |(a, b, c, d)| (a.min(x), b.min(y), c.max(x), d.max(y))));
            }
        }
    }
    found
}

#[test]
fn the_first_characters_show_before_the_last() {
    let Some(mut r) = rasterizer() else { return };
    let text = TextContent::new("HHHH", 0.1);
    let still = r.rasterize(&text, HEIGHT, 1.0);
    let half = r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[1.0, 1.0, 0.0, 0.0]));
    assert_eq!((half.width, half.height), (still.width, still.height), "the picture keeps the size of the still text");
    let (all, part) = (ink_box(&still).unwrap(), ink_box(&half).unwrap());
    assert_eq!(part.0, all.0, "the first character is there");
    assert!(part.2 < (all.0 + all.2) / 2 + 2, "the last two are not yet: {part:?} of {all:?}");
    let none = r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[0.0; 4]));
    assert_eq!(ink_box(&none), None, "nothing shows before the first");
}

#[test]
fn poses_at_rest_give_the_still_picture_exactly() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("Hi there", 0.1);
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: 0.08 });
    let still = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[1.0; 7])), still);
    assert_eq!(r.rasterize_units(&text, HEIGHT, 1.0, &[]), still);
}

#[test]
fn each_character_keeps_its_outline_and_shadow() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("H H", 0.1);
    text.outline = Some(TextOutline { color: [0, 0, 255, 255], width: 0.06 });
    text.shadow = Some(TextShadow { color: [0, 0, 0, 200], offset: [0.04, 0.04], blur: 0.03 });
    let still = r.rasterize(&text, HEIGHT, 1.0);
    // A move too small to see sends the text through the characters' own pictures.
    let nudged = vec![(TextUnit::Char, vec![AnimationPose { dx: 1e-7, ..AnimationPose::IDENTITY }; 2])];
    let drawn = r.rasterize_units(&text, HEIGHT, 1.0, &nudged);
    let worst = still.pixels.iter().zip(&drawn.pixels).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 3, "the characters drawn one by one look like the still text: {worst}");
    // The second character alone, moved down a font size: its outline goes with it.
    let font_px = 0.1 * HEIGHT as f64;
    let moved = vec![(TextUnit::Char, vec![AnimationPose { opacity: 0.0, ..AnimationPose::IDENTITY }, AnimationPose { dy: 0.2, ..AnimationPose::IDENTITY }])];
    let image = r.rasterize_units(&text, HEIGHT, 1.0, &moved);
    let blue_rows: Vec<u32> = (0..image.height).filter(|&y| (0..image.width).any(|x| { let p = &image.pixels[((y * image.width + x) * 4) as usize..][..4]; p[2] > 200 && p[0] < 60 && p[3] > 200 })).collect();
    let still_rows: Vec<u32> = (0..still.height).filter(|&y| (0..still.width).any(|x| { let p = &still.pixels[((y * still.width + x) * 4) as usize..][..4]; p[2] > 200 && p[0] < 60 && p[3] > 200 })).collect();
    let shift = f64::from(blue_rows[0]) - f64::from(still_rows[0]);
    assert!((shift - 0.2 * font_px).abs() <= 2.0, "the outline moved with its character: {shift}");
}

fn lines(opacities: &[f64]) -> Vec<(TextUnit, Vec<AnimationPose>)> {
    vec![(TextUnit::Line, opacities.iter().map(|&opacity| AnimationPose { opacity, ..AnimationPose::IDENTITY }).collect())]
}

#[test]
fn a_lone_carriage_return_breaks_the_line_for_units_too() {
    let Some(mut r) = rasterizer() else { return };
    let text = TextContent::new("AB\rCD", 0.1);
    let still = ink_box(&r.rasterize(&text, HEIGHT, 1.0)).unwrap();
    let middle = (still.1 + still.3) / 2;
    let only_c = ink_box(&r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[0.0, 0.0, 1.0, 0.0]))).expect("the C shows");
    assert!(only_c.1 > middle, "the C is on the second line: {only_c:?} of {still:?}");
    assert!(only_c.2 < (still.0 + still.2) / 2 + 2, "and is its first letter: {only_c:?} of {still:?}");
    assert_eq!(engine::unit_ranges(&text.shaped_content(), TextUnit::Line).len(), 2, "two lines, as drawn");
    let second = ink_box(&r.rasterize_units(&text, HEIGHT, 1.0, &lines(&[0.0, 1.0]))).expect("the second line shows");
    assert!(second.1 > middle, "the second line alone: {second:?} of {still:?}");
    let crlf = TextContent::new("AB\r\nCD\n\rEF", 0.1);
    assert_eq!(engine::unit_ranges(&crlf.shaped_content(), TextUnit::Line).len(), 3, "a CR LF and an LF CR each end one line");
    let third = ink_box(&r.rasterize_units(&crlf, HEIGHT, 1.0, &lines(&[0.0, 0.0, 1.0]))).expect("the third line shows");
    let all = ink_box(&r.rasterize(&crlf, HEIGHT, 1.0)).unwrap();
    assert!(third.1 > all.1 + (all.3 - all.1) / 2, "the last line alone: {third:?} of {all:?}");
}

#[test]
fn a_line_separator_does_not_break_the_line() {
    let Some(mut r) = rasterizer() else { return };
    let text = TextContent::new("AB\u{2028}CD", 0.1);
    assert_eq!(engine::unit_ranges(&text.shaped_content(), TextUnit::Line).len(), 1, "one line, as drawn");
    let still = r.rasterize(&text, HEIGHT, 1.0);
    let only_d = ink_box(&r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[0.0, 0.0, 0.0, 1.0]))).expect("the D shows");
    let all = ink_box(&still).unwrap();
    assert!(only_d.0 > (all.0 + all.2) / 2, "the D is the last letter of the one line: {only_d:?} of {all:?}");
}

#[test]
fn a_letter_with_its_marks_moves_as_one() {
    let Some(mut r) = rasterizer() else { return };
    // "Việt" with the marks of ệ as combining characters: still 4 units.
    let text = TextContent::new("Vie\u{323}\u{302}t", 0.1);
    assert_eq!(engine::unit_ranges(&text.shaped_content(), TextUnit::Char).len(), 4);
    let only_v = ink_box(&r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[1.0, 0.0, 0.0, 0.0]))).unwrap();
    let only_e = ink_box(&r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[0.0, 0.0, 1.0, 0.0]))).unwrap();
    assert!(only_e.3 > only_v.3, "the dot below goes with the e: {only_e:?} {only_v:?}");
    let without_e = r.rasterize_units(&text, HEIGHT, 1.0, &shown(&[1.0, 1.0, 0.0, 1.0]));
    let still = r.rasterize(&text, HEIGHT, 1.0);
    let lowest = |image: &TextImage| ink_box(image).unwrap().3;
    assert!(lowest(&without_e) < lowest(&still), "and goes away with it");
}
