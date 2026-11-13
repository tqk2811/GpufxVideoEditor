//! Keyframes: values of a clip that change with the time since its start.
//!
//! # Rules
//!
//! * **Time.** A keyframe is at a whole number of timeline ticks from the start of its clip, so a
//!   frame at a keyframe's tick gets exactly its value, with no accumulation at any length.
//! * **Between two keyframes** the value follows the easing of the earlier one. Before the first and
//!   after the last it holds. No easing leaves the span of its two values, so a value that is valid
//!   at both keyframes is valid in between.
//! * **A keyframe belongs to the picture under it.** A start trim, a source-in trim and a split keep
//!   it on the same source frame by moving it in clip time; a speed change scales it. See
//!   [`crate::edit`]. A keyframe that an edit leaves outside the clip is kept, so the curve inside
//!   stays exactly the same, also through an easing, and it comes back when the clip grows again. The
//!   one exception: removing the last keyframe of a property inside the clip removes the ones
//!   outside it too, so the property goes back to its static value.
//! * **Without keyframes** a property has its static value, and one with keyframes ignores it. The
//!   static value is what the property goes back to when its last keyframe is removed.

use crate::project::{Clip, ClipPlacement, Effect, PlacedRect, MAX_PLACEMENT_OFFSET, MAX_PLACEMENT_WIDTH, MIN_PLACEMENT_WIDTH};
use crate::clip_animation::{animation_max_scale, animation_rotates, AnimationPose};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// How far a clip may be turned, either way, in degrees: ten turns.
pub const MAX_ROTATION: f64 = 3600.0;
/// The most keyframes one property may have.
pub const MAX_KEYFRAMES: usize = 4096;
/// How far from the start of its clip a keyframe may be, either way: about 400 years.
pub const MAX_KEYFRAME_TIME: Time = Time::from_ticks(1 << 50);

/// How the value moves from a keyframe to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    pub const ALL: [Easing; 4] = [Easing::Linear, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut];

    /// Where `t`, between 0 and 1, lands on the curve. 0 and 1 stay where they are.
    pub fn apply(self, t: f64) -> f64 {
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseInOut => t * t * (3.0 - 2.0 * t),
        }
    }
}

fn is_linear(easing: &Easing) -> bool {
    *easing == Easing::Linear
}

/// One value at one time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Timeline ticks from the start of the clip.
    pub at: Time,
    pub value: f64,
    /// How the value moves to the next keyframe.
    #[serde(default, skip_serializing_if = "is_linear")]
    pub easing: Easing,
}

/// The value of `keys`, sorted by strictly rising time, at `at`, or `None` without keyframes.
pub fn sample_keyframes(keys: &[Keyframe], at: Time) -> Option<f64> {
    let first = keys.first()?;
    let next = keys.partition_point(|key| key.at <= at);
    if next == 0 {
        return Some(first.value);
    }
    let from = keys[next - 1];
    // Exact at a keyframe's tick, without going through the arithmetic below.
    if from.at == at || next == keys.len() {
        return Some(from.value);
    }
    let to = keys[next];
    let elapsed = at.saturating_sub(from.at).ticks() as f64;
    let span = to.at.saturating_sub(from.at).ticks() as f64;
    // Not `from + (to - from) * s`: the difference of two values that are far apart overflows.
    let along = from.easing.apply(elapsed / span);
    Some(from.value * (1.0 - along) + to.value * along)
}

/// The tick of the keyframe of `keys` nearest to `at`, when it is no further than `tolerance`. The
/// playhead is a raw tick, and a keyframe was set where a frame was, so a pointer on a keyframe is
/// usually a few ticks off it. The earlier of two at the same distance wins.
pub fn nearest_keyframe(keys: &[Keyframe], at: Time, tolerance: Time) -> Option<Time> {
    keys.iter().map(|key| key.at).filter(|time| time.ticks().abs_diff(at.ticks()) <= tolerance.ticks().unsigned_abs()).min_by_key(|time| time.ticks().abs_diff(at.ticks()))
}

/// Like [`nearest_keyframe`], but only among the keyframes inside a clip of `duration`: the filter
/// comes first, so a keyframe outside the clip never hides a farther one inside it.
pub fn nearest_keyframe_inside(keys: &[Keyframe], at: Time, tolerance: Time, duration: Time) -> Option<Time> {
    keys.iter().map(|key| key.at).filter(|time| *time >= Time::ZERO && *time < duration).filter(|time| time.ticks().abs_diff(at.ticks()) <= tolerance.ticks().unsigned_abs()).min_by_key(|time| time.ticks().abs_diff(at.ticks()))
}

/// How far from a keyframe the playhead still counts as on it: half an output frame.
pub fn keyframe_tolerance(frame_duration: Time) -> Time {
    frame_duration.half_floor()
}

/// The ticks of the keyframes just before and just after `position`, in ticks from the start of a
/// clip of `duration`, which may be outside it. Only keyframes inside the clip count, and the one
/// `position` is on, within `tolerance`, is neither of them.
pub fn keyframe_neighbors(keys: &[Keyframe], position: Time, tolerance: Time, duration: Time) -> (Option<Time>, Option<Time>) {
    let inside: Vec<Keyframe> = keys.iter().copied().filter(|key| key.at >= Time::ZERO && key.at < duration).collect();
    let here = nearest_keyframe(&inside, position, tolerance).unwrap_or(position);
    let before = inside.iter().map(|key| key.at).filter(|at| *at < here).max();
    let after = inside.iter().map(|key| key.at).filter(|at| *at > here).min();
    (before, after)
}

/// Puts a keyframe at `at` with `value`, replacing one that is there. `easing` of `None` keeps the
/// easing of the one replaced, and is linear for a new one.
pub fn upsert_keyframe(keys: &mut Vec<Keyframe>, at: Time, value: f64, easing: Option<Easing>) {
    match keys.binary_search_by_key(&at, |key| key.at) {
        Ok(index) => {
            let key = &mut keys[index];
            key.value = value;
            key.easing = easing.unwrap_or(key.easing);
        }
        Err(index) => keys.insert(index, Keyframe { at, value, easing: easing.unwrap_or_default() }),
    }
}

/// A property of a clip that can have keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipProperty {
    X,
    Y,
    Width,
    Opacity,
    Rotation,
}

impl ClipProperty {
    pub const ALL: [ClipProperty; 5] = [ClipProperty::X, ClipProperty::Y, ClipProperty::Width, ClipProperty::Opacity, ClipProperty::Rotation];

    /// Whether the property is one of the numbers of [`ClipPlacement`].
    pub fn is_placement(self) -> bool {
        matches!(self, ClipProperty::X | ClipProperty::Y | ClipProperty::Width)
    }

    /// Whether the property may have this value.
    pub fn accepts(self, value: f64) -> bool {
        value.is_finite()
            && match self {
                ClipProperty::X | ClipProperty::Y => value.abs() <= MAX_PLACEMENT_OFFSET,
                ClipProperty::Width => (MIN_PLACEMENT_WIDTH..=MAX_PLACEMENT_WIDTH).contains(&value),
                ClipProperty::Opacity => (0.0..=1.0).contains(&value),
                ClipProperty::Rotation => value.abs() <= MAX_ROTATION,
            }
    }

    /// The name in the project file and in messages.
    pub fn name(self) -> &'static str {
        match self {
            ClipProperty::X => "x",
            ClipProperty::Y => "y",
            ClipProperty::Width => "width",
            ClipProperty::Opacity => "opacity",
            ClipProperty::Rotation => "rotation",
        }
    }
}

/// What a keyframe animates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeTarget {
    /// A property of the clip itself.
    Property(ClipProperty),
    /// A numeric parameter of an effect of the clip.
    EffectParam { effect: usize, param: String },
}

/// Opacity, rotation and the keyframes of a clip. Left out of the file when it is all default, so a
/// project that does not use it loads and saves unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMotion {
    /// 0 is fully see-through and 1 opaque.
    #[serde(default = "full_opacity", skip_serializing_if = "is_full_opacity")]
    pub opacity: f64,
    /// Clockwise degrees around the middle of the picture.
    #[serde(default, skip_serializing_if = "is_no_rotation")]
    pub rotation: f64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keyframes: BTreeMap<ClipProperty, Vec<Keyframe>>,
}

fn full_opacity() -> f64 {
    1.0
}

fn is_full_opacity(opacity: &f64) -> bool {
    *opacity == 1.0
}

fn is_no_rotation(rotation: &f64) -> bool {
    *rotation == 0.0
}

impl Default for ClipMotion {
    fn default() -> Self {
        Self { opacity: full_opacity(), rotation: 0.0, keyframes: BTreeMap::new() }
    }
}

impl ClipMotion {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `property` has at least one keyframe.
    pub fn has_keyframes(&self, property: ClipProperty) -> bool {
        self.keyframes.get(&property).is_some_and(|keys| !keys.is_empty())
    }

    fn value_at(&self, property: ClipProperty, offset: Time) -> Option<f64> {
        sample_keyframes(self.keyframes.get(&property)?, offset)
    }
}

/// What a clip looks like at one moment: where it sits and how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipTransform {
    /// The static place of the clip. Absent: as large as fits, centred, in its own shape.
    pub placement: Option<ClipPlacement>,
    /// Keyframed numbers, which replace those of the place above.
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub opacity: f64,
    pub rotation: f64,
    /// Whether the clip turns at any moment, not only at this one: a clip with keyframes of
    /// rotation is drawn the same way at the frames where it happens to be upright.
    pub rotates: bool,
    /// Shift of the picture after everything above, in parts of the frame width and height (from
    /// a preset animation of a text clip, see [`crate::clip_animation`]).
    pub shift: [f64; 2],
    /// Scale of the picture around its middle, after everything above.
    pub zoom: f64,
    /// Scale of the width and the height alone, on top of `zoom`.
    pub stretch: [f64; 2],
    /// Blur radius of the placed picture, in parts of the output frame height. 0 is no blur.
    pub blur: f64,
}

impl Default for ClipTransform {
    fn default() -> Self {
        Self::still(None)
    }
}

impl ClipTransform {
    /// A clip with no motion at all at `placement`.
    pub fn still(placement: Option<ClipPlacement>) -> Self {
        Self { placement, x: None, y: None, width: None, opacity: 1.0, rotation: 0.0, rotates: false, shift: [0.0, 0.0], zoom: 1.0, stretch: [1.0, 1.0], blur: 0.0 }
    }

    /// This transform with `pose` of a preset animation on top.
    pub fn with_pose(mut self, pose: AnimationPose) -> Self {
        self.shift = [self.shift[0] + pose.dx, self.shift[1] + pose.dy];
        self.zoom *= pose.scale;
        self.stretch = [self.stretch[0] * pose.scale_x, self.stretch[1] * pose.scale_y];
        self.rotation += pose.rotation;
        self.opacity *= pose.opacity;
        self.blur += pose.blur;
        self
    }

    /// The rectangle of the picture before rotation, for a frame of `output_aspect` and a source of
    /// `source_aspect`, both width over height.
    pub fn rect(self, output_aspect: f64, source_aspect: f64) -> PlacedRect {
        let base = PlacedRect::of(self.placement, output_aspect, source_aspect);
        let mut rect = base;
        if let Some(width) = self.width {
            rect.height = base.height / base.width * width;
            rect.width = width;
            // A width that changes alone grows the picture around its middle.
            if self.x.is_none() {
                rect.x = base.x + (base.width - width) / 2.0;
            }
            if self.y.is_none() {
                rect.y = base.y + (base.height - rect.height) / 2.0;
            }
        }
        rect.x = self.x.unwrap_or(rect.x);
        rect.y = self.y.unwrap_or(rect.y);
        if self.zoom != 1.0 || self.stretch != [1.0, 1.0] {
            let (middle_x, middle_y) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            rect.width *= self.zoom * self.stretch[0];
            rect.height *= self.zoom * self.stretch[1];
            rect.x = middle_x - rect.width / 2.0;
            rect.y = middle_y - rect.height / 2.0;
        }
        rect.x += self.shift[0];
        rect.y += self.shift[1];
        rect
    }

    /// The numbers the place of the clip has now, for a source of `source_aspect`.
    pub fn placement_now(self, output_aspect: f64, source_aspect: f64) -> ClipPlacement {
        let rect = self.rect(output_aspect, source_aspect);
        ClipPlacement { x: rect.x, y: rect.y, width: rect.width }
    }
}

impl Clip {
    /// The transform `offset` ticks after the start of the clip.
    pub fn transform_at(&self, offset: Time) -> ClipTransform {
        let motion = &self.motion;
        ClipTransform {
            placement: self.placement,
            x: motion.value_at(ClipProperty::X, offset),
            y: motion.value_at(ClipProperty::Y, offset),
            width: motion.value_at(ClipProperty::Width, offset),
            opacity: motion.value_at(ClipProperty::Opacity, offset).unwrap_or(motion.opacity),
            rotation: motion.value_at(ClipProperty::Rotation, offset).unwrap_or(motion.rotation),
            rotates: motion.has_keyframes(ClipProperty::Rotation) || motion.rotation != 0.0 || self.text.as_ref().is_some_and(|text| animation_rotates(&text.animation)),
            shift: [0.0, 0.0],
            zoom: 1.0,
            stretch: [1.0, 1.0],
            blur: 0.0,
        }
    }

    /// The widest the picture ever is, in parts of the frame width, for a source of
    /// `source_aspect`: its static place or the largest of its width keyframes, times the largest
    /// scale of the preset animations of a text clip.
    pub fn widest(&self, output_aspect: f64, source_aspect: f64) -> f64 {
        let base = PlacedRect::of(self.placement, output_aspect, source_aspect).width;
        let keys = self.motion.keyframes.get(&ClipProperty::Width).map_or(&[][..], Vec::as_slice);
        let widest = keys.iter().map(|key| key.value).filter(|value| value.is_finite()).fold(base, f64::max);
        widest * self.text.as_ref().map_or(1.0, |text| animation_max_scale(&text.animation))
    }

    /// The value of a property `offset` ticks after the start of the clip, for a source of `source_aspect`.
    pub fn property_at(&self, property: ClipProperty, offset: Time, output_aspect: f64, source_aspect: f64) -> f64 {
        let transform = self.transform_at(offset);
        match property {
            ClipProperty::X => transform.rect(output_aspect, source_aspect).x,
            ClipProperty::Y => transform.rect(output_aspect, source_aspect).y,
            ClipProperty::Width => transform.rect(output_aspect, source_aspect).width,
            ClipProperty::Opacity => transform.opacity,
            ClipProperty::Rotation => transform.rotation,
        }
    }

    /// The keyframes of `target`, or `None` when it names no effect of the clip.
    pub fn keyframes_of(&self, target: &KeyframeTarget) -> Option<&[Keyframe]> {
        match target {
            KeyframeTarget::Property(property) => Some(self.motion.keyframes.get(property).map_or(&[][..], Vec::as_slice)),
            KeyframeTarget::EffectParam { effect, param } => self.effects.get(*effect).map(|effect| effect.keyframes.get(param).map_or(&[][..], Vec::as_slice)),
        }
    }

    pub(crate) fn keyframes_mut(&mut self, target: &KeyframeTarget) -> Option<&mut Vec<Keyframe>> {
        match target {
            KeyframeTarget::Property(property) => Some(self.motion.keyframes.entry(*property).or_default()),
            KeyframeTarget::EffectParam { effect, param } => self.effects.get_mut(*effect).map(|effect| effect.keyframes.entry(param.clone()).or_default()),
        }
    }

    /// Drops the lists that have no keyframe left.
    pub(crate) fn prune_keyframes(&mut self) {
        self.motion.keyframes.retain(|_, keys| !keys.is_empty());
        for effect in &mut self.effects {
            effect.keyframes.retain(|_, keys| !keys.is_empty());
        }
    }

    fn all_keyframes_mut(&mut self) -> impl Iterator<Item = &mut Vec<Keyframe>> {
        self.motion.keyframes.values_mut().chain(self.effects.iter_mut().flat_map(|effect| effect.keyframes.values_mut()))
    }

    /// Moves every keyframe by `delta` ticks in clip time.
    pub(crate) fn shift_keyframes(&mut self, delta: Time) {
        for keys in self.all_keyframes_mut() {
            for key in keys.iter_mut() {
                key.at = key.at.saturating_add(delta);
            }
        }
    }

    /// Stretches clip time by `numerator / denominator`, rounded to the nearest tick. Keyframes that
    /// land on one tick become one, the later keeping its value.
    pub(crate) fn scale_keyframes(&mut self, numerator: i64, denominator: i64) {
        for keys in self.all_keyframes_mut() {
            for key in keys.iter_mut() {
                let scaled = i128::from(key.at.ticks()) * i128::from(numerator);
                let half = i128::from(denominator) / 2;
                let rounded = if scaled < 0 { (scaled - half) / i128::from(denominator) } else { (scaled + half) / i128::from(denominator) };
                key.at = Time::from_ticks(i64::try_from(rounded).unwrap_or(if rounded < 0 { i64::MIN } else { i64::MAX }));
            }
            let mut kept: Vec<Keyframe> = Vec::with_capacity(keys.len());
            for key in keys.drain(..) {
                match kept.last_mut() {
                    Some(last) if last.at == key.at => *last = key,
                    _ => kept.push(key),
                }
            }
            *keys = kept;
        }
    }

    /// Every time inside the clip with a keyframe of anything, sorted and without repeats.
    pub fn keyframe_times(&self) -> Vec<Time> {
        let lists = self.motion.keyframes.values().chain(self.effects.iter().flat_map(|effect| effect.keyframes.values()));
        let mut times: Vec<Time> = lists.flatten().map(|key| key.at).filter(|at| *at >= Time::ZERO && *at < self.duration).collect();
        times.sort();
        times.dedup();
        times
    }
}

/// `effects` with their keyframed parameters set to the value `offset` ticks after the start of the
/// clip. Borrows when no effect has keyframes, the usual case: this runs for every clip of every frame.
pub fn effects_at(effects: &[Effect], offset: Time) -> Cow<'_, [Effect]> {
    if effects.iter().all(|effect| effect.keyframes.is_empty()) {
        return Cow::Borrowed(effects);
    }
    let resolved = effects
        .iter()
        .map(|effect| {
            let mut params = effect.params.clone();
            for (name, keys) in &effect.keyframes {
                if let Some(value) = sample_keyframes(keys, offset) {
                    params.insert(name.clone(), Value::from(value));
                }
            }
            Effect { package: effect.package.clone(), params, keyframes: BTreeMap::new() }
        })
        .collect();
    Cow::Owned(resolved)
}

/// Why `keys` is not a valid keyframe list, if it is not. `accepts` judges each value.
pub(crate) fn keyframes_problem(keys: &[Keyframe], accepts: impl Fn(f64) -> bool) -> Option<&'static str> {
    if keys.len() > MAX_KEYFRAMES {
        return Some("there are too many keyframes");
    }
    if keys.iter().any(|key| key.at > MAX_KEYFRAME_TIME || key.at < Time::from_ticks(-MAX_KEYFRAME_TIME.ticks())) {
        return Some("a keyframe time is too far from the clip");
    }
    if keys.windows(2).any(|pair| pair[0].at >= pair[1].at) {
        return Some("keyframe times must rise");
    }
    if keys.iter().any(|key| !accepts(key.value)) {
        return Some("a keyframe value is out of its limits");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(at: i64, value: f64, easing: Easing) -> Keyframe {
        Keyframe { at: Time::from_ticks(at), value, easing }
    }

    fn at(ticks: i64) -> Time {
        Time::from_ticks(ticks)
    }

    #[test]
    fn no_keyframes_give_no_value() {
        assert_eq!(sample_keyframes(&[], at(5)), None);
    }

    #[test]
    fn a_single_keyframe_holds_everywhere() {
        let keys = [key(100, 0.5, Easing::EaseIn)];
        for ticks in [-1000, 0, 100, 99_999] {
            assert_eq!(sample_keyframes(&keys, at(ticks)), Some(0.5));
        }
    }

    #[test]
    fn values_hold_before_the_first_and_after_the_last() {
        let keys = [key(100, 1.0, Easing::Linear), key(200, 3.0, Easing::Linear)];
        assert_eq!(sample_keyframes(&keys, at(99)), Some(1.0));
        assert_eq!(sample_keyframes(&keys, at(100)), Some(1.0));
        assert_eq!(sample_keyframes(&keys, at(200)), Some(3.0));
        assert_eq!(sample_keyframes(&keys, at(201)), Some(3.0));
    }

    #[test]
    fn linear_is_proportional_to_the_ticks() {
        let keys = [key(0, 0.0, Easing::Linear), key(3000, 3.0, Easing::Linear)];
        assert_eq!(sample_keyframes(&keys, at(1500)), Some(1.5));
        assert_eq!(sample_keyframes(&keys, at(1000)), Some(1.0));
    }

    /// A value that has no exact binary form still comes out exactly at the tick of its keyframe.
    #[test]
    fn a_keyframes_own_tick_gives_its_exact_value_however_late() {
        let late = 2_000_000_003_i64;
        let keys = [key(1, 0.1, Easing::EaseInOut), key(late, 0.3, Easing::EaseInOut), key(late + 7, 0.7, Easing::Linear)];
        assert_eq!(sample_keyframes(&keys, at(late)).unwrap().to_bits(), 0.3_f64.to_bits());
        assert_eq!(sample_keyframes(&keys, at(late + 7)).unwrap().to_bits(), 0.7_f64.to_bits());
        assert_eq!(sample_keyframes(&keys, at(1)).unwrap().to_bits(), 0.1_f64.to_bits());
    }

    #[test]
    fn a_sample_does_not_depend_on_how_it_was_reached() {
        let keys = [key(0, 0.0, Easing::Linear), key(3003 * 100_000, 1.0, Easing::Linear)];
        let exact = |frame: i64| sample_keyframes(&keys, at(3003 * frame)).unwrap();
        assert_eq!(exact(33_333), exact(33_333), "a pure function of the frame");
        assert_eq!(exact(50_000), 0.5);
    }

    #[test]
    fn easings_keep_their_ends_and_bend_the_middle() {
        for easing in Easing::ALL {
            assert_eq!(easing.apply(0.0), 0.0, "{easing:?}");
            assert_eq!(easing.apply(1.0), 1.0, "{easing:?}");
        }
        assert_eq!(Easing::EaseIn.apply(0.5), 0.25);
        assert_eq!(Easing::EaseOut.apply(0.5), 0.75);
        assert_eq!(Easing::EaseInOut.apply(0.5), 0.5);
        assert!(Easing::EaseInOut.apply(0.25) < 0.25 && Easing::EaseInOut.apply(0.75) > 0.75);
    }

    #[test]
    fn no_easing_leaves_the_span_of_its_two_values() {
        for easing in Easing::ALL {
            let keys = [key(0, 0.25, easing), key(1000, 0.75, Easing::Linear)];
            let mut previous = 0.25;
            for ticks in 0..=1000 {
                let value = sample_keyframes(&keys, at(ticks)).unwrap();
                assert!((0.25..=0.75).contains(&value), "{easing:?} at {ticks}: {value}");
                assert!(value >= previous, "{easing:?} at {ticks} goes back");
                previous = value;
            }
        }
    }

    #[test]
    fn the_easing_is_the_one_of_the_earlier_keyframe() {
        let keys = [key(0, 0.0, Easing::EaseIn), key(100, 1.0, Easing::Linear), key(200, 2.0, Easing::Linear)];
        assert_eq!(sample_keyframes(&keys, at(50)), Some(0.25), "out of the first");
        assert_eq!(sample_keyframes(&keys, at(150)), Some(1.5), "out of the second");
    }

    #[test]
    fn upsert_replaces_at_the_same_tick_and_stays_sorted() {
        let mut keys = vec![key(100, 1.0, Easing::EaseOut)];
        upsert_keyframe(&mut keys, at(50), 0.5, None);
        upsert_keyframe(&mut keys, at(100), 2.0, None);
        assert_eq!(keys, vec![key(50, 0.5, Easing::Linear), key(100, 2.0, Easing::EaseOut)], "the easing stays when not given");
        upsert_keyframe(&mut keys, at(100), 2.0, Some(Easing::EaseIn));
        assert_eq!(keys[1].easing, Easing::EaseIn);
    }

    #[test]
    fn a_list_is_judged_for_order_time_count_and_value() {
        let fine = [key(-5, 0.0, Easing::Linear), key(5, 1.0, Easing::Linear)];
        assert_eq!(keyframes_problem(&fine, |_| true), None, "a time before the clip is allowed: an edit can leave it there");
        assert!(keyframes_problem(&[key(5, 0.0, Easing::Linear), key(5, 1.0, Easing::Linear)], |_| true).is_some(), "the same tick twice");
        assert!(keyframes_problem(&[key(5, 0.0, Easing::Linear), key(1, 1.0, Easing::Linear)], |_| true).is_some(), "out of order");
        assert!(keyframes_problem(&[key(MAX_KEYFRAME_TIME.ticks() + 1, 0.0, Easing::Linear)], |_| true).is_some(), "too far");
        assert!(keyframes_problem(&[key(0, 2.0, Easing::Linear)], |value| value < 1.0).is_some(), "a value");
        let many: Vec<Keyframe> = (0..=MAX_KEYFRAMES as i64).map(|i| key(i, 0.0, Easing::Linear)).collect();
        assert!(keyframes_problem(&many, |_| true).is_some(), "too many");
    }

    #[test]
    fn a_static_transform_is_plain_and_keyed_numbers_win() {
        let mut clip = crate::project::tests_support::clip();
        clip.placement = Some(ClipPlacement { x: 0.1, y: 0.2, width: 0.5 });
        assert_eq!(clip.transform_at(at(10)), ClipTransform::still(clip.placement));
        clip.motion.keyframes.insert(ClipProperty::X, vec![key(0, 0.7, Easing::Linear)]);
        clip.motion.opacity = 0.25;
        let rect = clip.transform_at(at(10)).rect(16.0 / 9.0, 16.0 / 9.0);
        assert_eq!((rect.x, rect.y, rect.width), (0.7, 0.2, 0.5));
        assert_eq!(clip.transform_at(at(10)).opacity, 0.25);
    }

    #[test]
    fn a_keyed_width_keeps_the_shape_of_the_picture() {
        let mut clip = crate::project::tests_support::clip();
        clip.motion.keyframes.insert(ClipProperty::Width, vec![key(0, 0.5, Easing::Linear)]);
        let rect = clip.transform_at(at(0)).rect(2.0, 1.0);
        assert_eq!((rect.width, rect.height), (0.5, 1.0), "as wide as half the frame, height 2 x its width");
    }

    #[test]
    fn keyed_params_of_an_effect_are_filled_in_only_when_there_are_some() {
        let plain = Effect { package: "a".into(), params: serde_json::Map::new(), keyframes: BTreeMap::new() };
        assert!(matches!(effects_at(std::slice::from_ref(&plain), at(0)), Cow::Borrowed(_)));
        let mut keyed = plain.clone();
        keyed.keyframes.insert("amount".into(), vec![key(0, 0.0, Easing::Linear), key(100, 4.0, Easing::Linear)]);
        let resolved = effects_at(std::slice::from_ref(&keyed), at(50));
        assert_eq!(resolved[0].params.get("amount"), Some(&Value::from(2.0)));
        assert!(resolved[0].keyframes.is_empty());
    }
}
