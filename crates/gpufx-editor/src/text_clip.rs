//! The pure rules of text clips that the panels follow: what a new text looks like, where it is
//! placed, how its place follows a change of its size, and which animation presets each combo
//! offers.

use engine::{AnimationGroup, ClipPlacement, ClipAnimation, AnimationRef, TextContent, TextBackground, TextOutline, TextShadow, Time, MAX_PLACEMENT_WIDTH, MIN_PLACEMENT_WIDTH, ANIMATION_PRESETS};

/// The font size of a new text, in parts of the output height.
pub const DEFAULT_TEXT_SIZE: f32 = 0.08;
/// The smallest and largest font size the inspector offers, in parts of the output height.
pub const TEXT_SIZE_RANGE: std::ops::RangeInclusive<f64> = 0.01..=0.5;
/// How long one run of an animation lasts when it is first chosen.
pub const DEFAULT_ANIMATION_DURATION: Time = Time::from_ticks(engine::TICKS_PER_SECOND / 2);

/// A new text clip showing `content`: white, centred, in the default font.
pub fn default_text(content: impl Into<String>) -> TextContent {
    TextContent::new(content, DEFAULT_TEXT_SIZE)
}

/// The place of a text whose picture is `pixels` wide and tall at its natural size, in an output
/// of `output` pixels: that size, centred in the frame. `None` when the text draws nothing.
pub fn natural_placement(pixels: (u32, u32), output: (u32, u32)) -> Option<ClipPlacement> {
    let (width, height) = parts(pixels, output)?;
    let width_clamped = width.clamp(MIN_PLACEMENT_WIDTH, MAX_PLACEMENT_WIDTH);
    let height = height * width_clamped / width;
    Some(ClipPlacement { x: (1.0 - width_clamped) / 2.0, y: (1.0 - height) / 2.0, width: width_clamped })
}

/// The place of a text after its picture went from `before` to `after` pixels: scaled as the
/// user scaled it before, around the same centre, so a bigger font draws bigger text. `None`
/// when either size is empty, which leaves the place as it is.
pub fn rescaled_placement(placement: ClipPlacement, before: (u32, u32), after: (u32, u32), output: (u32, u32)) -> Option<ClipPlacement> {
    let (old_width, old_height) = parts(before, output)?;
    let (new_width, new_height) = parts(after, output)?;
    let zoom = placement.width / old_width;
    let width = (new_width * zoom).clamp(MIN_PLACEMENT_WIDTH, MAX_PLACEMENT_WIDTH);
    let height = new_height * width / new_width;
    let centre = (placement.x + placement.width / 2.0, placement.y + old_height * zoom / 2.0);
    Some(ClipPlacement { x: centre.0 - width / 2.0, y: centre.1 - height / 2.0, width })
}

/// Width and height of `pixels` in parts of an `output` frame.
fn parts(pixels: (u32, u32), output: (u32, u32)) -> Option<(f64, f64)> {
    if pixels.0 == 0 || pixels.1 == 0 || output.0 == 0 || output.1 == 0 {
        return None;
    }
    Some((f64::from(pixels.0) / f64::from(output.0), f64::from(pixels.1) / f64::from(output.1)))
}

/// The ids of the presets a combo of `group` offers, in the order of the engine list.
pub fn presets_of(group: AnimationGroup) -> Vec<&'static str> {
    ANIMATION_PRESETS.iter().filter(|preset| preset.group == group).map(|preset| preset.id).collect()
}

/// The animation of `group` in `animation`.
pub fn animation_of(animation: &ClipAnimation, group: AnimationGroup) -> Option<&AnimationRef> {
    match group {
        AnimationGroup::Enter => animation.enter.as_ref(),
        AnimationGroup::Exit => animation.exit.as_ref(),
        AnimationGroup::Repeat => animation.repeat.as_ref(),
    }
}

/// `animation` with the animation of `group` replaced by `value`.
pub fn with_animation(animation: &ClipAnimation, group: AnimationGroup, value: Option<AnimationRef>) -> ClipAnimation {
    let mut next = animation.clone();
    match group {
        AnimationGroup::Enter => next.enter = value,
        AnimationGroup::Exit => next.exit = value,
        AnimationGroup::Repeat => next.repeat = value,
    }
    next
}

/// The animation of `group` once preset `id` is chosen, or none for `None`: the duration it had,
/// or [`DEFAULT_ANIMATION_DURATION`] when there was none.
pub fn choose_preset(animation: &ClipAnimation, group: AnimationGroup, id: Option<&str>) -> ClipAnimation {
    let duration = animation_of(animation, group).map_or(DEFAULT_ANIMATION_DURATION, |current| current.duration);
    with_animation(animation, group, id.map(|id| AnimationRef { preset: id.to_owned(), duration }))
}

/// The outline a text gets when it is turned on: black, a twentieth of the font size.
pub fn default_outline() -> TextOutline {
    TextOutline { color: [0, 0, 0, 255], width: 0.05 }
}

/// The box a text gets when it is turned on: translucent black with a little room and round corners.
pub fn default_background() -> TextBackground {
    TextBackground { color: [0, 0, 0, 160], padding: 0.25, radius: 0.2 }
}

/// The shadow a text gets when it is turned on: soft, down and to the right.
pub fn default_shadow() -> TextShadow {
    TextShadow { color: [0, 0, 0, 180], offset: [0.05, 0.05], blur: 0.05 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::PlacedRect;

    #[test]
    fn a_new_text_is_white_centred_and_of_the_default_size() {
        let text = default_text("Hi");
        assert_eq!(text.content, "Hi");
        assert_eq!(text.size, DEFAULT_TEXT_SIZE);
        assert_eq!(text.font, engine::FontRef::Default);
        assert_eq!(text.color, [255, 255, 255, 255]);
        assert_eq!(text.align, engine::TextAlign::Center);
        assert!(text.outline.is_none() && text.background.is_none() && text.shadow.is_none());
    }

    #[test]
    fn the_natural_place_has_the_picture_size_and_is_centred() {
        let placement = natural_placement((480, 108), (1920, 1080)).unwrap();
        assert!((placement.width - 0.25).abs() < 1e-9);
        assert!((placement.x - 0.375).abs() < 1e-9);
        // The engine derives the height from the shape: 108 of 1080 is a tenth.
        let rect = PlacedRect::of(Some(placement), 1920.0 / 1080.0, 480.0 / 108.0);
        assert!((rect.height - 0.1).abs() < 1e-9);
        assert!((rect.y + rect.height / 2.0 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_text_drawing_nothing_has_no_natural_place() {
        assert_eq!(natural_placement((0, 0), (1920, 1080)), None);
        assert_eq!(natural_placement((10, 10), (0, 1080)), None);
    }

    #[test]
    fn a_text_wider_than_the_limit_keeps_its_shape() {
        let placement = natural_placement((1920 * 10, 108), (1920, 1080)).unwrap();
        assert_eq!(placement.width, MAX_PLACEMENT_WIDTH);
        assert!((placement.x + placement.width / 2.0 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_bigger_font_scales_the_place_around_its_centre() {
        let output = (1920, 1080);
        let before = natural_placement((480, 108), output).unwrap();
        // The user made it twice as large and moved it left.
        let placed = ClipPlacement { x: 0.1, y: 0.2, width: 0.5 };
        let after = rescaled_placement(placed, (480, 108), (960, 216), output).unwrap();
        assert!((after.width - 1.0).abs() < 1e-9);
        assert!((after.x + after.width / 2.0 - (placed.x + placed.width / 2.0)).abs() < 1e-9);
        let old_rect = PlacedRect::of(Some(placed), 1920.0 / 1080.0, 480.0 / 108.0);
        let new_rect = PlacedRect::of(Some(after), 1920.0 / 1080.0, 960.0 / 216.0);
        assert!((old_rect.y + old_rect.height / 2.0 - (new_rect.y + new_rect.height / 2.0)).abs() < 1e-9);
        // An unchanged picture keeps its place.
        let same = rescaled_placement(before, (480, 108), (480, 108), output).unwrap();
        assert!((same.x - before.x).abs() < 1e-12 && (same.y - before.y).abs() < 1e-12 && (same.width - before.width).abs() < 1e-12);
        assert_eq!(rescaled_placement(placed, (0, 0), (960, 216), output), None);
    }

    #[test]
    fn each_combo_offers_only_the_presets_of_its_group() {
        let enter = presets_of(AnimationGroup::Enter);
        assert_eq!(enter.first(), Some(&"fade"));
        assert!(enter.contains(&"pop") && !enter.contains(&"bounce"));
        let repeat = presets_of(AnimationGroup::Repeat);
        assert_eq!(repeat, vec!["bounce", "pulse", "shake"]);
        for group in [AnimationGroup::Enter, AnimationGroup::Exit, AnimationGroup::Repeat] {
            for id in presets_of(group) {
                assert!(engine::find_preset(group, id).is_some(), "{group:?} {id}");
            }
        }
    }

    #[test]
    fn choosing_a_preset_keeps_the_duration_and_the_other_groups() {
        let first = choose_preset(&ClipAnimation::default(), AnimationGroup::Enter, Some("fade"));
        assert_eq!(first.enter, Some(AnimationRef { preset: "fade".to_owned(), duration: DEFAULT_ANIMATION_DURATION }));
        let longer = with_animation(&first, AnimationGroup::Enter, Some(AnimationRef { preset: "fade".to_owned(), duration: Time::from_ticks(engine::TICKS_PER_SECOND) }));
        let looped = choose_preset(&longer, AnimationGroup::Repeat, Some("pulse"));
        let swapped = choose_preset(&looped, AnimationGroup::Enter, Some("zoom"));
        assert_eq!(swapped.enter.as_ref().map(|value| (value.preset.as_str(), value.duration)), Some(("zoom", Time::from_ticks(engine::TICKS_PER_SECOND))));
        assert_eq!(swapped.repeat.as_ref().map(|value| value.preset.as_str()), Some("pulse"));
        assert!(choose_preset(&swapped, AnimationGroup::Enter, None).enter.is_none());
    }
}
