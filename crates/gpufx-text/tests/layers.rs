//! The layers of a text (outlines, shadows with their own outlines, inner shadows) and their paints
//! (solid, gradient, texture), checked at a few pixels of a tall "I".

use engine::project::{GradientStop, TextContent, TextImageRef, TextInnerShadow, TextOutline, TextPaint, TextShadow, TextTexture, TextureFit};
use gpufx_text::{TextImage, TextRasterizer, DEFAULT_FAMILY_CANDIDATES};

const HEIGHT: u32 = 720;
const SIZE: f32 = 0.3;

fn skip(reason: String) {
    if std::env::var("GPUFX_REQUIRE_FONTS").is_ok_and(|value| value == "1") {
        panic!("GPUFX_REQUIRE_FONTS=1 but {reason}");
    }
    eprintln!("skipped: {reason}");
}

fn rasterizer() -> Option<TextRasterizer> {
    let r = TextRasterizer::new();
    if r.default_family().is_some_and(|family| DEFAULT_FAMILY_CANDIDATES.iter().any(|known| known.eq_ignore_ascii_case(family))) {
        Some(r)
    } else {
        skip(format!("none of {DEFAULT_FAMILY_CANDIDATES:?} is installed (default is {:?})", r.default_family()));
        None
    }
}

fn pixel(image: &TextImage, x: i32, y: i32) -> [u8; 4] {
    let at = ((y as u32 * image.width + x as u32) * 4) as usize;
    image.pixels[at..at + 4].try_into().unwrap()
}

/// The stem of the "I" in row `y`: its first and last column at least half covered by the fill.
fn stem(image: &TextImage, y: i32, is_fill: impl Fn([u8; 4]) -> bool) -> (i32, i32) {
    let columns: Vec<i32> = (0..image.width as i32).filter(|&x| is_fill(pixel(image, x, y))).collect();
    assert!(!columns.is_empty(), "no fill in row {y}");
    (columns[0], *columns.last().unwrap())
}

/// The rows the stem covers in column `x`.
fn rows(image: &TextImage, x: i32, is_fill: impl Fn([u8; 4]) -> bool) -> (i32, i32) {
    let rows: Vec<i32> = (0..image.height as i32).filter(|&y| is_fill(pixel(image, x, y))).collect();
    assert!(!rows.is_empty(), "no fill in column {x}");
    (rows[0], *rows.last().unwrap())
}

fn red(p: [u8; 4]) -> bool {
    p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] > 200
}

#[test]
fn shadows_lie_under_outlines_and_outlines_widest_first_under_the_fill() {
    let Some(mut r) = rasterizer() else { return };
    let font_px = SIZE * HEIGHT as f32;
    let mut text = TextContent::new("I", SIZE);
    text.fill = TextPaint::solid([255, 0, 0, 255]);
    // Listed narrow first: the wide green one is still drawn first, under the black one.
    text.outlines = vec![TextOutline::solid([0, 0, 0, 255], 0.02), TextOutline::solid([0, 255, 0, 255], 0.06)];
    let mut shadow = TextShadow::solid([0, 0, 255, 255], [0.5, 0.0], 0.0);
    shadow.outlines = vec![TextOutline::solid([255, 255, 255, 255], 0.03)];
    text.shadows = vec![shadow];
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let y = image.height as i32 / 2;
    let (left, right) = stem(&image, y, red);
    let center = (left + right) / 2;
    let step = |parts: f32| (parts * font_px).round() as i32;
    assert_eq!(pixel(&image, center, y), [255, 0, 0, 255], "the fill on top");
    assert_eq!(pixel(&image, right + step(0.01), y), [0, 0, 0, 255], "the narrow outline over the wide one");
    assert_eq!(pixel(&image, right + step(0.045), y), [0, 255, 0, 255], "the wide outline past the narrow one");
    assert_eq!(pixel(&image, left - step(0.045), y), [0, 255, 0, 255], "on both sides");
    let shift = step(0.5);
    assert_eq!(pixel(&image, center + shift, y), [0, 0, 255, 255], "the shadow, moved");
    assert_eq!(pixel(&image, right + shift + step(0.02), y), [255, 255, 255, 255], "the shadow's own outline");
    assert_eq!(pixel(&image, left - step(0.08), y)[3], 0, "no shadow to the left");
    assert!(pixel(&image, image.width as i32 - 2, y)[3] < 255, "the picture holds the shadow outline");
}

#[test]
fn a_stack_of_shadows_reads_as_depth_and_is_drawn_from_one_picture() {
    let Some(mut r) = rasterizer() else { return };
    let font_px = SIZE * HEIGHT as f32;
    let mut text = TextContent::new("I", SIZE);
    text.fill = TextPaint::solid([255, 0, 0, 255]);
    let gradient = TextPaint::Gradient { stops: vec![GradientStop { position: 0.0, color: [0, 0, 255, 255] }, GradientStop { position: 1.0, color: [0, 0, 255, 255] }], angle: 0.0 };
    text.shadows = (1..=engine::MAX_TEXT_SHADOWS).map(|i| TextShadow { paint: gradient.clone(), ..TextShadow::solid([0; 4], [0.01 * i as f32, 0.0], 0.0) }).collect();
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let y = image.height as i32 / 2;
    let (_, right) = stem(&image, y, red);
    // The farthest shadow ends 0.32 font sizes past the stem; every column up to it is covered.
    let reach = (0.31 * font_px) as i32;
    // Past the fill's anti-aliased edge.
    let gaps: Vec<i32> = (right + 3..right + reach).filter(|&x| pixel(&image, x, y) != [0, 0, 255, 255]).collect();
    assert!(gaps.is_empty(), "{gaps:?}");
}

#[test]
fn a_gradient_runs_across_the_whole_text_at_its_angle() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("I", SIZE);
    let stops = vec![GradientStop { position: 0.0, color: [255, 0, 0, 255] }, GradientStop { position: 1.0, color: [0, 0, 255, 255] }];
    text.fill = TextPaint::Gradient { stops: stops.clone(), angle: 90.0 };
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let filled = |p: [u8; 4]| p[3] == 255;
    let x = {
        let (left, right) = stem(&image, image.height as i32 / 2, filled);
        (left + right) / 2
    };
    let (top, bottom) = rows(&image, x, filled);
    let (high, low) = (pixel(&image, x, top + 2), pixel(&image, x, bottom - 2));
    assert!(high[0] > 150 && high[2] < 100, "red at the top: {high:?}");
    assert!(low[2] > 150 && low[0] < 100, "blue at the bottom: {low:?}");
    // At 0 degrees it runs from left to right instead: the top and bottom of the stem match.
    text.fill = TextPaint::Gradient { stops, angle: 0.0 };
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let (top, bottom) = rows(&image, x, filled);
    let (high, low) = (pixel(&image, x, top + 2), pixel(&image, x, bottom - 2));
    assert!(high.iter().zip(low).all(|(a, b)| a.abs_diff(b) <= 2), "{high:?} {low:?}");
}

#[test]
fn an_inner_shadow_darkens_the_edge_it_comes_from() {
    let Some(mut r) = rasterizer() else { return };
    let font_px = SIZE * HEIGHT as f32;
    let mut text = TextContent::new("I", SIZE);
    text.inner_shadows = vec![TextInnerShadow { distance: 0.04, angle: 0.0, blur: 0.0, paint: TextPaint::solid([0, 0, 0, 255]) }];
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let y = image.height as i32 / 2;
    let (left, right) = stem(&image, y, |p| p[3] == 255);
    let inset = (0.02 * font_px) as i32;
    assert_eq!(pixel(&image, left + inset, y), [0, 0, 0, 255], "shadow along the left edge, for a shadow cast to the right");
    assert_eq!(pixel(&image, right - inset, y), [255, 255, 255, 255], "none along the right edge");
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gpufx-text-layers-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_texture_covers_the_text_and_a_missing_one_is_named() {
    let Some(mut r) = rasterizer() else { return };
    let dir = scratch("texture");
    // Red over blue, one pixel each: stretched over the text box, the top half is red.
    let path = dir.join("two.png");
    image::RgbaImage::from_fn(1, 2, |_, y| if y == 0 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) }).save(&path).unwrap();
    let mut text = TextContent::new("I", SIZE);
    let texture = TextTexture { image: TextImageRef::File { path: path.to_string_lossy().into_owned() }, fit: TextureFit::Stretch, scale: 1.0, angle: 0.0, opacity: 1.0 };
    text.fill = TextPaint::Texture(texture.clone());
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(image.missing_texture, None);
    let filled = |p: [u8; 4]| p[3] == 255;
    let x = {
        let (left, right) = stem(&image, image.height as i32 / 2, filled);
        (left + right) / 2
    };
    let (top, bottom) = rows(&image, x, filled);
    // Enlarged, the two pixels blend smoothly into each other (and a little into the next copy).
    let (high, low) = (pixel(&image, x, top + 2), pixel(&image, x, bottom - 2));
    assert!(high[0] > 200 && high[2] < 55, "{high:?}");
    assert!(low[2] > 200 && low[0] < 55, "{low:?}");
    // Tiled a font size tall, the copies repeat down the stem: red at the top of each.
    text.fill = TextPaint::Texture(TextTexture { fit: TextureFit::Tile, scale: 0.1, ..texture.clone() });
    let tiled = r.rasterize(&text, HEIGHT, 1.0);
    let column: Vec<[u8; 4]> = (top + 2..bottom - 2).map(|y| pixel(&tiled, x, y)).collect();
    let reds = column.windows(2).filter(|w| w[0][0] < 128 && w[1][0] >= 128).count();
    assert!(reds >= 5, "several copies down the stem: {reds}");
    assert!(column.iter().any(|p| p[0] > 240) && column.iter().any(|p| p[2] > 240), "{column:?}");
    // A picture that is not there leaves its layer out and is named.
    let gone = dir.join("gone.png").to_string_lossy().into_owned();
    text.fill = TextPaint::Texture(TextTexture { image: TextImageRef::File { path: gone.clone() }, ..texture });
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!(image.missing_texture.as_deref(), Some(gone.as_str()));
    assert!(image.pixels.chunks(4).all(|p| p[3] == 0));
    // A pack picture the caller did not resolve is named as `<pack>/<file>`.
    text.fill = TextPaint::Texture(TextTexture { image: TextImageRef::text_style_pack("gold", "a.png"), fit: TextureFit::Stretch, scale: 1.0, angle: 0.0, opacity: 1.0 });
    assert_eq!(r.rasterize(&text, HEIGHT, 1.0).missing_texture.as_deref(), Some("gold/a.png"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_changed_texture_file_is_read_again() {
    let Some(mut r) = rasterizer() else { return };
    let dir = scratch("changed");
    let path = dir.join("one.png");
    let write = |color: [u8; 4], size: u32| image::RgbaImage::from_pixel(size, size, image::Rgba(color)).save(&path).unwrap();
    write([255, 0, 0, 255], 1);
    let mut text = TextContent::new("I", SIZE);
    text.fill = TextPaint::Texture(TextTexture { image: TextImageRef::File { path: path.to_string_lossy().into_owned() }, fit: TextureFit::Stretch, scale: 1.0, angle: 0.0, opacity: 1.0 });
    let stamps = gpufx_text::text_texture_stamps(&text);
    let image = r.rasterize(&text, HEIGHT, 1.0);
    let y = image.height as i32 / 2;
    let (left, right) = stem(&image, y, red);
    // A file of another length has another stamp, whatever its time.
    write([0, 0, 255, 255], 2);
    assert_ne!(gpufx_text::text_texture_stamps(&text), stamps);
    assert_eq!(pixel(&r.rasterize(&text, HEIGHT, 1.0), (left + right) / 2, y), [0, 0, 255, 255]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn characters_drawn_one_by_one_keep_the_paints_of_the_still_text() {
    let Some(mut r) = rasterizer() else { return };
    let mut text = TextContent::new("H H", 0.1);
    let stops = vec![GradientStop { position: 0.0, color: [255, 0, 0, 255] }, GradientStop { position: 1.0, color: [0, 0, 255, 255] }];
    text.fill = TextPaint::Gradient { stops: stops.clone(), angle: 30.0 };
    text.outlines = vec![TextOutline { width: 0.05, paint: TextPaint::Gradient { stops: stops.clone(), angle: 120.0 } }];
    let mut shadow = TextShadow { paint: TextPaint::Gradient { stops, angle: 0.0 }, ..TextShadow::solid([0; 4], [0.05, 0.05], 0.02) };
    shadow.outlines = vec![TextOutline::solid([0, 255, 0, 255], 0.03)];
    text.shadows = vec![shadow];
    text.inner_shadows = vec![TextInnerShadow { distance: 0.02, angle: 45.0, blur: 0.0, paint: TextPaint::solid([0, 0, 0, 128]) }];
    let still = r.rasterize(&text, HEIGHT, 1.0);
    // A move too small to see sends the text through the characters' own pictures.
    let nudged = vec![(engine::TextUnit::Char, vec![engine::AnimationPose { dx: 1e-7, ..engine::AnimationPose::IDENTITY }; 2])];
    let drawn = r.rasterize_units(&text, HEIGHT, 1.0, &nudged);
    assert_eq!((drawn.width, drawn.height), (still.width, still.height));
    let worst = still.pixels.iter().zip(&drawn.pixels).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 3, "{worst}");
}

#[test]
fn a_bloom_lights_around_the_text_under_it_and_moves_with_its_direction() {
    let Some(mut r) = rasterizer() else { return };
    let font_px = SIZE * HEIGHT as f32;
    let mut text = TextContent::new("I", SIZE);
    text.fill = TextPaint::solid([255, 0, 0, 255]);
    let plain = r.rasterize(&text, HEIGHT, 1.0);
    text.bloom = Some(engine::TextBloom { color: [0, 0, 255, 255], strength: 1.0, range: 0.2, dir_x: 0.0, dir_y: 0.0 });
    let image = r.rasterize(&text, HEIGHT, 1.0);
    assert!(image.width > plain.width && image.height > plain.height, "the picture grows by the spread");
    let y = image.height as i32 / 2;
    let (left, right) = stem(&image, y, red);
    let center = (left + right) / 2;
    assert_eq!(pixel(&image, center, y), [255, 0, 0, 255], "the fill stays on top");
    let step = |parts: f32| (parts * font_px).round() as i32;
    let near = pixel(&image, right + step(0.02), y);
    let far = pixel(&image, right + step(0.15), y);
    assert!(near[2] > 100 && near[0] < 10 && near[2] == near[3], "blue light next to the ink: {near:?}");
    assert!(far[3] < near[3], "fading away: {far:?} after {near:?}");
    assert_eq!(pixel(&image, 0, y)[3], 0, "nothing at the edge of the picture: the margin holds it");
    assert_eq!(pixel(&image, image.width as i32 - 1, y)[3], 0);
    // Moved right by its whole spread: more light on the right than on the left.
    text.bloom = Some(engine::TextBloom { dir_x: 1.0, ..text.bloom.unwrap() });
    let moved = r.rasterize(&text, HEIGHT, 1.0);
    let (left, right) = stem(&moved, y, red);
    let (l, rr) = (pixel(&moved, left - step(0.04), y), pixel(&moved, right + step(0.04), y));
    assert!(rr[3] > l[3], "right {rr:?} over left {l:?}");
    assert_eq!(pixel(&moved, moved.width as i32 - 1, y)[3], 0, "the margin follows the move");
    // No strength: no bloom and the picture as without one.
    text.bloom = Some(engine::TextBloom { strength: 0.0, ..text.bloom.unwrap() });
    let none = r.rasterize(&text, HEIGHT, 1.0);
    assert_eq!((none.width, none.height, &none.pixels), (plain.width, plain.height, &plain.pixels));
}
