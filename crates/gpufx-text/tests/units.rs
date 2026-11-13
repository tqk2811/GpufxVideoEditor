use engine::project::{TextContent, TextGlow, TextOutline, TextShadow};
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

/// A text with every layer under its fill, for the timing and the frame before rest.
fn decorated(content: &str) -> TextContent {
    let mut text = TextContent::new(content, 0.1);
    text.outline = Some(TextOutline { color: [0, 0, 255, 255], width: 0.05 });
    text.shadow = Some(TextShadow { color: [0, 0, 0, 160], offset: [0.04, 0.04], blur: 0.05 });
    text.glow = Some(TextGlow { color: [255, 200, 0, 255], outer_color: None, radius: 0.15, intensity: 0.8, falloff: 2.0 });
    text
}

/// How long a frame of a moving 1080p text takes; run with `--ignored --nocapture`, in release.
#[test]
#[ignore]
fn time_a_frame_of_moving_text() {
    let Some(mut r) = rasterizer() else { return };
    let text = decorated("Per-unit animation at 1080p");
    let count = engine::unit_ranges(&text.shaped_content(), TextUnit::Char).len();
    let frame = |i: usize| vec![(TextUnit::Char, (0..count).map(|u| AnimationPose { dy: ((i + u) % 7) as f64 * 0.05, rotation: ((i + u) % 5) as f64 * 3.0, ..AnimationPose::IDENTITY }).collect())];
    let _ = r.rasterize_units(&text, 1080, 1.0, &frame(0));
    let frames = 30;
    let started = std::time::Instant::now();
    for i in 1..=frames {
        std::hint::black_box(r.rasterize_units(&text, 1080, 1.0, &frame(i)));
    }
    let per_frame = started.elapsed() / frames as u32;
    let still = std::time::Instant::now();
    std::hint::black_box(r.rasterize(&text, 1080, 1.0));
    println!("moving frame: {per_frame:?}; still picture: {:?}", still.elapsed());
}

/// Whether any pixel of the outermost rows and columns shows, as a unit cut at the edge does.
fn touches_edge(image: &TextImage) -> bool {
    let shows = |x: u32, y: u32| image.pixels[((y * image.width + x) * 4 + 3) as usize] > 16;
    (0..image.width).any(|x| shows(x, 0) || shows(x, image.height - 1)) || (0..image.height).any(|y| shows(0, y) || shows(image.width - 1, y))
}

#[test]
fn a_tumbling_character_is_never_cut_at_the_edge() {
    let Some(mut r) = rasterizer() else { return };
    let text = TextContent::new("Hgjy", 0.1);
    let per_unit = engine::find_preset(engine::AnimationGroup::Enter, "tumble").and_then(|preset| preset.per_unit()).expect("the tumble preset");
    let count = engine::unit_ranges(&text.shaped_content(), TextUnit::Char).len();
    assert!(!touches_edge(&r.rasterize(&text, HEIGHT, 1.0)), "the still text has room");
    for step in 0..=40 {
        let poses = vec![(TextUnit::Char, engine::per_unit::unit_poses(per_unit, count, f64::from(step) / 40.0))];
        let image = r.rasterize_units(&text, HEIGHT, 1.0, &poses);
        assert!(!touches_edge(&image), "cut at step {step}");
    }
}

#[test]
fn a_character_moved_past_the_edge_grows_the_picture_around_its_middle() {
    let Some(mut r) = rasterizer() else { return };
    let text = TextContent::new("HH", 0.1);
    let still = r.rasterize(&text, HEIGHT, 1.0);
    let up = vec![(TextUnit::Char, vec![AnimationPose { dy: -1.0, ..AnimationPose::IDENTITY }; 2])];
    let image = r.rasterize_units(&text, HEIGHT, 1.0, &up);
    let (mx, my) = image.margin;
    assert_eq!((image.width, image.height), (still.width + 2 * mx, still.height + 2 * my), "the same margin on opposite sides");
    assert!(my > 0 && !touches_edge(&image), "grown enough: {:?}", image.margin);
    let (moved, rest) = (ink_box(&image).unwrap(), ink_box(&still).unwrap());
    let font_px = 0.1 * HEIGHT as f64;
    let rise = f64::from(rest.1 + my) - f64::from(moved.1);
    assert!((rise - font_px).abs() <= 2.0, "moved up a font size from where it rests: {rise}");
    assert_eq!(moved.3 - moved.1, rest.3 - rest.1, "whole");
}

#[test]
fn the_frame_before_rest_looks_like_the_still_text() {
    let Some(mut r) = rasterizer() else { return };
    // Glows and shadows that meet between the letters.
    let text = decorated("HHO");
    let still = r.rasterize(&text, HEIGHT, 1.0);
    let nudged = vec![(TextUnit::Char, vec![AnimationPose { dx: 1e-7, ..AnimationPose::IDENTITY }; 3])];
    let drawn = r.rasterize_units(&text, HEIGHT, 1.0, &nudged);
    assert_eq!((drawn.width, drawn.height), (still.width, still.height));
    let worst = still.pixels.iter().zip(&drawn.pixels).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 2, "the moving text just before rest is the still text: {worst}");
}

#[test]
fn two_texts_moving_in_turn_keep_their_plans() {
    let Some(mut r) = rasterizer() else { return };
    let (a, b) = (TextContent::new("first", 0.1), TextContent::new("second", 0.1));
    let nudged = |count: usize| vec![(TextUnit::Char, vec![AnimationPose { dy: 0.1, ..AnimationPose::IDENTITY }; count])];
    for _ in 0..5 {
        r.rasterize_units(&a, HEIGHT, 1.0, &nudged(5));
        r.rasterize_units(&b, HEIGHT, 1.0, &nudged(6));
    }
    assert_eq!(r.unit_plan_builds(), 2, "each text is prepared once");
    r.rasterize_units(&a, HEIGHT / 2, 1.0, &nudged(5));
    assert_eq!(r.unit_plan_builds(), 3, "another size is another plan");
    let plan = r.prepare_units(&a, HEIGHT, 1.0);
    assert!(plan.bytes() > 0 && plan.bytes() < gpufx_text::UNIT_PLAN_BYTES);
    assert_eq!(plan.draw(&nudged(5)), r.rasterize_units(&a, HEIGHT, 1.0, &nudged(5)), "a plan draws without the rasterizer");
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
