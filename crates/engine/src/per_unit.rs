//! Animation of the units of a text, one by one: its characters, words or lines.
//!
//! A preset may carry a [`PerUnit`] beside its whole-clip curve. The text is cut into units (see
//! [`unit_ranges`]); each unit gets a place in 0..1 from the [`UnitOrder`], and from that place
//! and the progress `t` of the preset (0 to 1) a pose of its own (see [`unit_poses`]):
//! - with `stagger` `s`, unit at place `p` plays the curve from `t = s * p` over `1 - s` of the
//!   time, so the first unit starts at once and the last ends at `t = 1`; `s = 0` plays every unit
//!   together, `s = 1` turns every unit at the moment of its place;
//! - with a [`RangeSelector`], the curve is played on `t` for every unit, and the selector says how
//!   much of it each unit takes (0 = the rest pose, 1 = the whole curve), like a range selector of
//!   an After Effects text animator.
//!
//! A unit is a grapheme (a letter with its marks, "ế" or "ß", is one unit), a word (a run without
//! white space, as [`crate::karaoke::word_ranges`]) or a line. White space is no unit. The ranges
//! are byte ranges of the text as shaped ([`crate::TextContent::shaped_content`]).
//!
//! In a unit pose `dx` and `dy` are in font sizes, the scale and the turn are around the middle of
//! the unit, and `blur` is not used.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::clip_animation::{check_curve, evaluate_curve, AnimationCurve, AnimationPose, CurveKey, CurveProperty};
use crate::karaoke::word_ranges;

/// What a text is cut into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextUnit {
    #[default]
    Char,
    Word,
    Line,
}

impl TextUnit {
    pub fn name(self) -> &'static str {
        match self {
            TextUnit::Char => "char",
            TextUnit::Word => "word",
            TextUnit::Line => "line",
        }
    }
}

/// Which unit comes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitOrder {
    /// From the first unit of the text to the last.
    #[default]
    Forward,
    Backward,
    /// From the middle out to both ends.
    Center,
    /// A shuffle that the seed fixes.
    Random,
}

/// How the amount of a range selector falls off over its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorShape {
    /// 1 inside the range, 0 outside.
    #[default]
    Square,
    /// 0 before the range, rising to 1 at its end, 1 after.
    RampUp,
    /// 1 before the range, falling to 0 at its end, 0 after.
    RampDown,
    /// 0 at both ends of the range, 1 in its middle.
    Triangle,
    /// A half circle over the range.
    Round,
    /// A smooth bell over the range.
    Smooth,
}

/// A range selector: `start`, `end` and `offset` are tracks over the progress `t` of the preset,
/// in parts of the units (0 = before the first, 1 = after the last). A track left empty keeps its
/// rest value: `start` 0, `end` 1, `offset` 0.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeSelector {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub start: Vec<CurveKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub end: Vec<CurveKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offset: Vec<CurveKey>,
    #[serde(default)]
    pub shape: SelectorShape,
}

/// The per-unit part of a preset (see the module notes).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerUnit {
    #[serde(default)]
    pub unit: TextUnit,
    #[serde(default)]
    pub order: UnitOrder,
    /// The seed of the `random` order.
    #[serde(default)]
    pub seed: u64,
    /// 0 to 1; exactly one of `stagger` and `selector`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stagger: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<RangeSelector>,
    /// The motion of each unit.
    pub curve: AnimationCurve,
}

fn check_track(name: &str, keys: &[CurveKey]) -> Result<(), String> {
    let mut last = 0.0;
    for key in keys {
        if !(key.t.is_finite() && (0.0..=1.0).contains(&key.t)) || !key.value.is_finite() {
            return Err(format!("a keyframe of the selector {name} is not finite or its t is not in 0..1"));
        }
        if key.t < last {
            return Err(format!("the keyframes of the selector {name} are not in rising t"));
        }
        last = key.t;
    }
    Ok(())
}

/// Checks a per-unit part that came from outside (a pack). The message says what is wrong.
pub fn check_per_unit(per_unit: &PerUnit) -> Result<(), String> {
    match (per_unit.stagger, &per_unit.selector) {
        (Some(_), Some(_)) => return Err("per_unit has both stagger and selector".to_owned()),
        (None, None) => return Err("per_unit needs stagger or selector".to_owned()),
        (Some(stagger), None) if !(stagger.is_finite() && (0.0..=1.0).contains(&stagger)) => return Err("the stagger of per_unit must be in 0..1".to_owned()),
        (None, Some(selector)) => {
            check_track("start", &selector.start)?;
            check_track("end", &selector.end)?;
            check_track("offset", &selector.offset)?;
        }
        _ => {}
    }
    check_curve(&per_unit.curve).map_err(|message| format!("the per_unit curve: {message}"))?;
    if per_unit.curve.tracks.contains_key(&CurveProperty::Blur) {
        return Err("the per_unit curve cannot blur a unit".to_owned());
    }
    Ok(())
}

/// The units of `shaped` (see the module notes), in the order of the text.
pub fn unit_ranges(shaped: &str, unit: TextUnit) -> Vec<Range<usize>> {
    match unit {
        TextUnit::Char => shaped.grapheme_indices(true).filter(|(_, g)| !g.chars().all(char::is_whitespace)).map(|(at, g)| at..at + g.len()).collect(),
        TextUnit::Word => word_ranges(shaped),
        TextUnit::Line => {
            let mut lines = Vec::new();
            let mut at = 0;
            for piece in shaped.split_inclusive('\n') {
                let body = piece.trim_end_matches(['\n', '\r']);
                let lead = body.len() - body.trim_start().len();
                let trimmed = body.trim();
                if !trimmed.is_empty() {
                    lines.push(at + lead..at + lead + trimmed.len());
                }
                at += piece.len();
            }
            lines
        }
    }
}

/// A small generator with a fixed sequence for a seed (SplitMix64).
fn next_random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The place of each of `count` units in 0..1: 0 for the unit that comes first, 1 for the last.
pub fn unit_places(count: usize, order: UnitOrder, seed: u64) -> Vec<f64> {
    if count <= 1 {
        return vec![0.0; count];
    }
    let last = (count - 1) as f64;
    match order {
        UnitOrder::Forward => (0..count).map(|i| i as f64 / last).collect(),
        UnitOrder::Backward => (0..count).map(|i| 1.0 - i as f64 / last).collect(),
        UnitOrder::Center => (0..count).map(|i| (i as f64 - last / 2.0).abs() / (last / 2.0)).collect(),
        UnitOrder::Random => {
            let mut ranks: Vec<usize> = (0..count).collect();
            let mut state = seed;
            for i in (1..count).rev() {
                let j = (next_random(&mut state) % (i as u64 + 1)) as usize;
                ranks.swap(i, j);
            }
            ranks.into_iter().map(|rank| rank as f64 / last).collect()
        }
    }
}

fn track_value(keys: &[CurveKey], rest: f64, t: f64) -> f64 {
    let mut curve = AnimationCurve::default();
    if keys.is_empty() {
        return rest;
    }
    curve.tracks.insert(CurveProperty::Dx, keys.to_vec());
    curve.value_at(CurveProperty::Dx, t)
}

/// How much of the curve a unit centred at `place` (0..1 over the units) takes from `selector` at
/// `t`.
pub fn selector_amount(selector: &RangeSelector, place: f64, t: f64) -> f64 {
    let start = track_value(&selector.start, 0.0, t);
    let end = track_value(&selector.end, 1.0, t);
    let offset = track_value(&selector.offset, 0.0, t);
    let (low, high) = (start.min(end) + offset, start.max(end) + offset);
    let span = high - low;
    // Where the unit is inside the range, 0 to 1; outside it, below 0 or above 1.
    let x = if span > 0.0 { (place - low) / span } else if place < low { -1.0 } else { 2.0 };
    let inside = (0.0..=1.0).contains(&x) && span > 0.0;
    match selector.shape {
        SelectorShape::Square => f64::from(u8::from(inside)),
        SelectorShape::RampUp => x.clamp(0.0, 1.0),
        SelectorShape::RampDown => 1.0 - x.clamp(0.0, 1.0),
        SelectorShape::Triangle if inside => 1.0 - (2.0 * x - 1.0).abs(),
        SelectorShape::Round if inside => (1.0 - (2.0 * x - 1.0).powi(2)).max(0.0).sqrt(),
        SelectorShape::Smooth if inside => (1.0 - (2.0 * std::f64::consts::PI * x).cos()) / 2.0,
        _ => 0.0,
    }
}

/// The rest pose mixed toward `pose` by `amount`.
fn mixed(pose: AnimationPose, amount: f64) -> AnimationPose {
    let mix = |rest: f64, value: f64| rest + (value - rest) * amount;
    AnimationPose {
        dx: mix(0.0, pose.dx),
        dy: mix(0.0, pose.dy),
        scale: mix(1.0, pose.scale),
        scale_x: mix(1.0, pose.scale_x),
        scale_y: mix(1.0, pose.scale_y),
        rotation: mix(0.0, pose.rotation),
        opacity: mix(1.0, pose.opacity),
        blur: 0.0,
    }
}

/// The time of the curve of the unit at `place` when the preset is at `t`, with `stagger`.
pub fn staggered_time(stagger: f64, place: f64, t: f64) -> f64 {
    let begin = stagger * place;
    let window = 1.0 - stagger;
    if window <= 0.0 {
        return if t >= begin { 1.0 } else { 0.0 };
    }
    ((t - begin) / window).clamp(0.0, 1.0)
}

/// The pose of each of `count` units when the preset is at `t` (0 to 1).
pub fn unit_poses(per_unit: &PerUnit, count: usize, t: f64) -> Vec<AnimationPose> {
    let places = unit_places(count, per_unit.order, per_unit.seed);
    places
        .into_iter()
        .map(|place| {
            let mut pose = match (&per_unit.selector, per_unit.stagger) {
                (Some(selector), _) => {
                    // The middle of the unit among the units.
                    let middle = if count == 0 { 0.0 } else { (place * (count - 1) as f64 + 0.5) / count as f64 };
                    mixed(evaluate_curve(&per_unit.curve, t), selector_amount(selector, middle, t))
                }
                (None, stagger) => evaluate_curve(&per_unit.curve, staggered_time(stagger.unwrap_or(0.0), place, t)),
            };
            pose.blur = 0.0;
            pose
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_animation::{key, Ease};

    fn opacity_in() -> PerUnit {
        PerUnit { stagger: Some(0.5), curve: AnimationCurve::default().with(CurveProperty::Opacity, vec![key(0.0, 0.0, Ease::Linear), key(1.0, 1.0, Ease::Linear)]), ..PerUnit::default() }
    }

    #[test]
    fn a_grapheme_is_one_unit() {
        let text = "Tiếng Việt ß e\u{301}";
        let units: Vec<&str> = unit_ranges(text, TextUnit::Char).into_iter().map(|r| &text[r]).collect();
        assert_eq!(units, vec!["T", "i", "ế", "n", "g", "V", "i", "ệ", "t", "ß", "e\u{301}"]);
        assert_eq!(unit_ranges("a b\r\n  cd  \n\nx", TextUnit::Line).into_iter().map(|r| &"a b\r\n  cd  \n\nx"[r]).collect::<Vec<_>>(), vec!["a b", "cd", "x"]);
        assert_eq!(unit_ranges("one  two", TextUnit::Word).len(), 2);
    }

    #[test]
    fn stagger_runs_from_the_first_unit_to_the_last() {
        let poses = |t| unit_poses(&opacity_in(), 3, t).iter().map(|p| p.opacity).collect::<Vec<_>>();
        assert_eq!(poses(0.0), vec![0.0, 0.0, 0.0], "nothing has started");
        assert_eq!(poses(1.0), vec![1.0, 1.0, 1.0], "everything has ended");
        let middle = poses(0.5);
        assert!(middle[0] == 1.0 && middle[2] == 0.0 && middle[1] > 0.0 && middle[1] < 1.0, "{middle:?}");
        assert_eq!(staggered_time(1.0, 1.0, 0.999), 0.0, "a full stagger turns the last unit only at the end");
        assert_eq!(staggered_time(1.0, 1.0, 1.0), 1.0);
        assert_eq!(staggered_time(0.0, 1.0, 0.25), 0.25, "no stagger plays every unit together");
    }

    #[test]
    fn orders_place_the_units() {
        assert_eq!(unit_places(3, UnitOrder::Backward, 0), vec![1.0, 0.5, 0.0]);
        assert_eq!(unit_places(5, UnitOrder::Center, 0), vec![1.0, 0.5, 0.0, 0.5, 1.0]);
        assert_eq!(unit_places(1, UnitOrder::Random, 7), vec![0.0]);
        let a = unit_places(20, UnitOrder::Random, 42);
        assert_eq!(a, unit_places(20, UnitOrder::Random, 42), "the same seed, the same order");
        assert_ne!(a, unit_places(20, UnitOrder::Random, 43));
        let mut sorted = a.clone();
        sorted.sort_by(f64::total_cmp);
        assert_eq!(sorted, unit_places(20, UnitOrder::Forward, 0), "a shuffle of the places");
    }

    #[test]
    fn a_selector_takes_units_in_its_range() {
        let square = RangeSelector { start: vec![key(0.0, 0.0, Ease::Linear)], end: vec![key(0.0, 0.5, Ease::Linear)], ..RangeSelector::default() };
        assert_eq!(selector_amount(&square, 0.0, 0.0), 1.0, "the start is in the range");
        assert_eq!(selector_amount(&square, 0.5, 0.0), 1.0, "so is the end");
        assert_eq!(selector_amount(&square, 0.51, 0.0), 0.0);
        let moving = RangeSelector { offset: vec![key(0.0, 0.0, Ease::Linear), key(1.0, 1.6, Ease::Linear)], end: vec![key(0.0, 0.0, Ease::Linear)], start: vec![key(0.0, -1.0, Ease::Linear)], shape: SelectorShape::RampDown };
        assert_eq!(selector_amount(&moving, 0.5, 0.0), 0.0, "a ramp down is 0 past its range");
        assert_eq!(selector_amount(&moving, 0.5, 1.0), 1.0, "and 1 before it");
        let tri = RangeSelector { shape: SelectorShape::Triangle, ..RangeSelector::default() };
        assert_eq!(selector_amount(&tri, 0.5, 0.3), 1.0);
        assert_eq!(selector_amount(&tri, 0.0, 0.3), 0.0);
        let round = RangeSelector { shape: SelectorShape::Round, ..RangeSelector::default() };
        assert!((selector_amount(&round, 0.5, 0.0) - 1.0).abs() < 1e-12);
        let smooth = RangeSelector { shape: SelectorShape::Smooth, ..RangeSelector::default() };
        assert!(selector_amount(&smooth, 0.0, 0.0).abs() < 1e-12 && (selector_amount(&smooth, 0.5, 0.0) - 1.0).abs() < 1e-12);
        let up = RangeSelector { shape: SelectorShape::RampUp, ..RangeSelector::default() };
        assert_eq!((selector_amount(&up, -0.1, 0.0), selector_amount(&up, 1.1, 0.0)), (0.0, 1.0));
    }

    #[test]
    fn a_per_unit_part_is_checked() {
        assert!(check_per_unit(&opacity_in()).is_ok());
        assert!(check_per_unit(&PerUnit { stagger: Some(1.5), ..opacity_in() }).is_err());
        assert!(check_per_unit(&PerUnit { stagger: None, ..opacity_in() }).is_err());
        assert!(check_per_unit(&PerUnit { selector: Some(RangeSelector::default()), ..opacity_in() }).is_err(), "not both");
        let blur = AnimationCurve::default().with(CurveProperty::Blur, vec![key(0.0, 1.0, Ease::Linear)]);
        assert!(check_per_unit(&PerUnit { curve: blur, ..opacity_in() }).is_err());
        let back = RangeSelector { start: vec![key(0.5, 0.0, Ease::Linear), key(0.2, 0.0, Ease::Linear)], ..RangeSelector::default() };
        assert!(check_per_unit(&PerUnit { stagger: None, selector: Some(back), ..opacity_in() }).is_err());
    }
}
