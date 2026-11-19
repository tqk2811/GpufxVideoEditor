//! Preset animations of clips. A preset only moves, scales, turns and fades the picture of the
//! clip, so it never draws the picture again and never changes the decoder key.
//!
//! Every preset is an [`AnimationCurve`]: tracks of keyframes over a normalized time `t` from 0 to
//! 1, one track per property, each segment with its own [`Ease`]. [`evaluate_curve`] turns a curve
//! and `t` into an [`AnimationPose`]. The built-in presets are curves like any other, so a package
//! can carry its own curves later.
//!
//! Timing (see [`animation_pose`]), over the time the clip is visible (its [`ClipClock`]):
//! - the enter preset runs from the first visible frame for its duration;
//! - the exit preset runs for its duration up to the last visible moment;
//! - the repeat preset runs the whole time, one cycle per its duration.
//!
//! When enter and exit together are longer than the visible time, both are shortened by the same
//! factor so they fill it exactly, the enter in the first part and the exit in the rest.
//! A preset id that is not known, or a duration that is not above zero, is left out.
//!
//! Presets of packs: a clip names one as `"<pack id>/<item id>"` (see [`PACK_PRESET_SEPARATOR`]).
//! The engine never reads a pack: the editor reads the installed packs and hands their curves over
//! with [`set_pack_presets`], one table for the whole process (as the fonts of the system are). A
//! pack preset that is not in the table is left out like an unknown id, but is not a validation
//! error, so a project opens and renders, without that animation, where its pack is missing.
//!
//! Effects of pack presets: a pack preset may also carry a [`PresetEffect`], a gpufx effect
//! package the reader of the packs found installed. While the preset runs, [`animation_effects`]
//! gives that effect, with its `progress` parameter set, and the renderer appends it after the
//! clip's own effects (see [`effects_with_animation`]). It never enters the `effects` of the clip,
//! so it is not saved and not undone. The order on a picture is: the clip's place and keyframes,
//! then the pose of the presets, then their effects. `progress`, over the same windows as the pose,
//! counted in output frames so the ends are frames that are drawn:
//! - enter: `p`, 0 at the first visible frame and 1 on the last frame of the window;
//! - exit: `1 - p`, 1 at the first frame of the window and 0 on the last visible frame, so a
//!   package written for an enter (0 = not shown yet, 1 = settled) plays backwards as the exit;
//! - repeat: the phase of the cycle, 0 up to (not including) 1.
//!
//! A package an enter or exit drives must draw the picture unchanged at `progress` 1: that is the
//! last frame of an enter, and the frame after it is drawn without the effect, so anything else
//! shows as a jump. Outside its window a preset gives no effect at all, nor on a clip with no
//! visible time. Enter, exit and repeat effects come in that order.

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use std::borrow::Cow;

use crate::keyframe::effects_at;
use crate::per_unit::{unit_poses, PerUnit, TextUnit, UnitOrder};
use crate::project::{ClipAnimation, AnimationRef, Effect};
use crate::schedule::SourceSample;
use crate::schedule::ClipClock;
use crate::frame_rate::FrameRate;
use crate::time::{Time, TICKS_PER_SECOND};

/// What parts a preset of a pack: `"<pack id>/<item id>"`. A built-in preset id never holds it.
pub const PACK_PRESET_SEPARATOR: char = '/';

/// Which part of a clip a preset is made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationGroup {
    /// Plays as the clip appears (`in` in a project).
    Enter,
    /// Plays as the clip goes away (`out` in a project).
    Exit,
    /// Repeats the whole time the clip is visible (`loop` in a project).
    Repeat,
}

impl AnimationGroup {
    /// The name of the group in a project file: `in`, `out` or `loop`.
    pub fn name(self) -> &'static str {
        match self {
            AnimationGroup::Enter => "in",
            AnimationGroup::Exit => "out",
            AnimationGroup::Repeat => "loop",
        }
    }
}

/// Which end of a [`Ease::Power`] curve is slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PowerMode {
    /// `u^e`: starts slow.
    #[default]
    In,
    /// `1 - (1 - u)^e`: ends slow.
    Out,
    /// The in curve over the first half and the out curve over the second.
    InOut,
}

/// Where the jumps of a [`Ease::Steps`] staircase are, as the jump terms of CSS `steps()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepPosition {
    /// A jump at the start of every step (CSS `jump-start`).
    Start,
    /// A jump at the end of every step (CSS `jump-end`).
    #[default]
    End,
    /// Jumps at both ends (CSS `jump-both`): `count + 1` jumps.
    Both,
    /// No jump at either end (CSS `jump-none`): `count - 1` jumps, so `count` must be 2 or more.
    None,
}

/// How a segment of a curve goes from one keyframe to the next. `apply(0)` is exactly 0 and
/// `apply(1)` exactly 1; between them a value may leave 0..1 (back, elastic, wave).
///
/// In JSON (camelCase): a bare name such as `"outCubic"`, or an object with the numbers, such as
/// `{"backOut":{"amplitude":1.70158}}`. `"elastic"` and `"bounce"` may also be written bare, for
/// their defaults; the numbers of `power`, `elastic`, `bounce`, `steps` and `wave` that are left out
/// take their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Ease {
    #[default]
    Linear,
    /// Keeps the value of the keyframe until the next one.
    Hold,
    /// The easings of clip keyframes (see [`crate::keyframe::Easing`]).
    EaseIn,
    EaseOut,
    EaseInOut,
    InCubic,
    OutCubic,
    InSine,
    OutSine,
    InOutSine,
    OutExpo,
    /// Goes below 0 a little before leaving; `amplitude` 1.70158 is the usual one.
    BackIn { amplitude: f64 },
    /// Goes past 1 a little before settling; `amplitude` 1.70158 is the usual one.
    BackOut { amplitude: f64 },
    /// Springs past 1 and back a few times before settling (out elastic, the easings.net form):
    /// `1 + a * 2^(-10u) * sin((u - s) * 2π / p)` with `s = p / 2π * asin(1 / a)`, so it starts at
    /// 0. `amplitude` (`a`, at least 1) scales the swings and `period` (`p`, above 0) is their length
    /// in parts of the segment. The defaults (1 and 0.3) are the classic out elastic.
    Elastic { amplitude: f64, period: f64 },
    /// Falls onto 1 like a dropped ball and bounces on it: `bounces` arcs touch 1 in all (the fall
    /// and `bounces - 1` rebounds, 1 to 20), the last at the end. Each rebound leaves with `decay`
    /// (in 0..1) times the speed of the one before, so it lasts `decay` times as long and rises
    /// `decay²` times as high (a coefficient of restitution). The defaults (4 and 0.5) give exactly
    /// the classic out bounce (rebounds of 1/4, 1/16 and 1/64).
    Bounce { bounces: u32, decay: f64 },
    /// A power curve of `exponent` (finite, above 0); see [`PowerMode`].
    Power { exponent: f64, mode: PowerMode },
    /// A staircase of `count` steps (1 to 1000) as CSS `steps(count, position)`. Its ends stay 0
    /// and 1 whatever the position: a staircase that jumps at its start still begins at 0.
    Steps { count: u32, position: StepPosition },
    /// Linear progress with a sine wobble that is 0 at both ends:
    /// `u + amplitude * (sin(2π c u) - u * sin(2π c)) * (1 - u)^decay`, `c` being `cycles` (above
    /// 0). The `- u * sin(2π c)` term only matters when `cycles` is not whole, and makes the end
    /// exactly 1 whatever `decay` is; `decay` (at least 0) fades the wobble toward the end, 0 keeps
    /// it even. `amplitude` may be negative.
    Wave { amplitude: f64, cycles: f64, decay: f64 },
    /// A CSS `cubic-bezier(x1, y1, x2, y2)`.
    CubicBezier([f64; 4]),
}

/// What the JSON of an [`Ease`] may hold: the object forms with their defaults. A bare `"elastic"`
/// or `"bounce"` is handled before (see the `Deserialize` of [`Ease`]).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
enum EaseData {
    Linear,
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
    InCubic,
    OutCubic,
    InSine,
    OutSine,
    InOutSine,
    OutExpo,
    BackIn {
        amplitude: f64,
    },
    BackOut {
        amplitude: f64,
    },
    Elastic {
        #[serde(default = "default_elastic_amplitude")]
        amplitude: f64,
        #[serde(default = "default_elastic_period")]
        period: f64,
    },
    Bounce {
        #[serde(default = "default_bounces")]
        bounces: u32,
        #[serde(default = "default_bounce_decay")]
        decay: f64,
    },
    Power {
        #[serde(default = "default_power_exponent")]
        exponent: f64,
        #[serde(default)]
        mode: PowerMode,
    },
    Steps {
        #[serde(default = "default_step_count")]
        count: u32,
        #[serde(default)]
        position: StepPosition,
    },
    Wave {
        #[serde(default = "default_wave_amplitude")]
        amplitude: f64,
        #[serde(default = "default_wave_cycles")]
        cycles: f64,
        #[serde(default)]
        decay: f64,
    },
    CubicBezier([f64; 4]),
}

fn default_elastic_amplitude() -> f64 {
    Ease::ELASTIC.amplitude()
}
fn default_elastic_period() -> f64 {
    0.3
}
fn default_bounces() -> u32 {
    4
}
fn default_bounce_decay() -> f64 {
    0.5
}
fn default_power_exponent() -> f64 {
    3.0
}
fn default_step_count() -> u32 {
    4
}
fn default_wave_amplitude() -> f64 {
    0.1
}
fn default_wave_cycles() -> f64 {
    2.0
}

impl From<EaseData> for Ease {
    fn from(data: EaseData) -> Self {
        match data {
            EaseData::Linear => Ease::Linear,
            EaseData::Hold => Ease::Hold,
            EaseData::EaseIn => Ease::EaseIn,
            EaseData::EaseOut => Ease::EaseOut,
            EaseData::EaseInOut => Ease::EaseInOut,
            EaseData::InCubic => Ease::InCubic,
            EaseData::OutCubic => Ease::OutCubic,
            EaseData::InSine => Ease::InSine,
            EaseData::OutSine => Ease::OutSine,
            EaseData::InOutSine => Ease::InOutSine,
            EaseData::OutExpo => Ease::OutExpo,
            EaseData::BackIn { amplitude } => Ease::BackIn { amplitude },
            EaseData::BackOut { amplitude } => Ease::BackOut { amplitude },
            EaseData::Elastic { amplitude, period } => Ease::Elastic { amplitude, period },
            EaseData::Bounce { bounces, decay } => Ease::Bounce { bounces, decay },
            EaseData::Power { exponent, mode } => Ease::Power { exponent, mode },
            EaseData::Steps { count, position } => Ease::Steps { count, position },
            EaseData::Wave { amplitude, cycles, decay } => Ease::Wave { amplitude, cycles, decay },
            EaseData::CubicBezier(points) => Ease::CubicBezier(points),
        }
    }
}

impl<'de> Deserialize<'de> for Ease {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value.as_str() {
            Some("elastic") => Ok(Ease::ELASTIC),
            Some("bounce") => Ok(Ease::BOUNCE),
            _ => EaseData::deserialize(value).map(Ease::from).map_err(serde::de::Error::custom),
        }
    }
}

impl Ease {
    /// The usual amplitude of [`Ease::BackIn`] and [`Ease::BackOut`].
    pub const BACK_AMPLITUDE: f64 = 1.70158;
    /// The out elastic with its default numbers (what a bare `"elastic"` is).
    pub const ELASTIC: Ease = Ease::Elastic { amplitude: 1.0, period: 0.3 };
    /// The out bounce with its default numbers (what a bare `"bounce"` is).
    pub const BOUNCE: Ease = Ease::Bounce { bounces: 4, decay: 0.5 };
    /// The most steps of [`Ease::Steps`] and bounces of [`Ease::Bounce`].
    pub const MAX_STEPS: u32 = 1000;
    pub const MAX_BOUNCES: u32 = 20;

    /// The amplitude of an elastic, back or wave ease; 0 for the others.
    fn amplitude(self) -> f64 {
        match self {
            Ease::Elastic { amplitude, .. } | Ease::BackIn { amplitude } | Ease::BackOut { amplitude } | Ease::Wave { amplitude, .. } => amplitude,
            _ => 0.0,
        }
    }

    /// Whether the numbers of the ease make a curve: a finite back amplitude, a bezier with finite
    /// points whose x lie in 0..1 (as CSS asks), and the ranges each parametric ease documents.
    pub fn is_valid(self) -> bool {
        match self {
            Ease::BackIn { amplitude } | Ease::BackOut { amplitude } => amplitude.is_finite(),
            Ease::CubicBezier(points) => points.iter().all(|p| p.is_finite()) && (0.0..=1.0).contains(&points[0]) && (0.0..=1.0).contains(&points[2]),
            Ease::Elastic { amplitude, period } => amplitude.is_finite() && amplitude >= 1.0 && period.is_finite() && period > 0.0,
            Ease::Bounce { bounces, decay } => (1..=Self::MAX_BOUNCES).contains(&bounces) && decay.is_finite() && decay > 0.0 && decay < 1.0,
            Ease::Power { exponent, .. } => exponent.is_finite() && exponent > 0.0,
            Ease::Steps { count, position } => (1..=Self::MAX_STEPS).contains(&count) && (position != StepPosition::None || count >= 2),
            Ease::Wave { amplitude, cycles, decay } => amplitude.is_finite() && cycles.is_finite() && cycles > 0.0 && decay.is_finite() && decay >= 0.0,
            _ => true,
        }
    }

    /// Bounds of `apply` over 0..1: no value of the ease is below the first or above the second.
    /// Exact for the eases that stay in 0..1, from the formula for a wave and a bezier, and
    /// measured densely, with a margin, for back and elastic.
    pub fn range(self) -> (f64, f64) {
        match self {
            Ease::BackIn { .. } | Ease::BackOut { .. } => self.sampled_range(),
            Ease::Elastic { amplitude, period } => {
                // A short period swings faster than the samples could follow; |sin| <= 1 bounds it.
                if period >= 0.02 {
                    self.sampled_range()
                } else {
                    (1.0 - amplitude, 1.0 + amplitude)
                }
            }
            Ease::Wave { amplitude, cycles, .. } => {
                // |sin(2π c u) - u sin(2π c)| <= 1 + |sin(2π c)|, and (1 - u)^decay <= 1.
                let swing = amplitude.abs() * (1.0 + (2.0 * PI * cycles).sin().abs());
                (-swing, 1.0 + swing)
            }
            // The curve stays in the hull of its control points.
            Ease::CubicBezier([_, y1, _, y2]) => (0f64.min(y1).min(y2), 1f64.max(y1).max(y2)),
            _ => (0.0, 1.0),
        }
    }

    /// The range of `apply` from samples, widened by how far a smooth curve can bulge between two
    /// samples (`f'' h² / 8` for a bound `f''` of its second derivative), doubled.
    fn sampled_range(self) -> (f64, f64) {
        const SAMPLES: u32 = 8192;
        let step = 1.0 / f64::from(SAMPLES);
        let amplitude = self.amplitude().abs();
        let curvature = match self {
            Ease::Elastic { period, .. } => amplitude * (2.0 * PI / period + 10.0 * std::f64::consts::LN_2).powi(2),
            // (a + 1) t³ - a t²: |f''| <= 6 (a + 1) + 2 a.
            _ => 8.0 * amplitude + 6.0,
        };
        let margin = curvature * step * step / 4.0 + 1e-12;
        let (low, high) = (0..=SAMPLES).map(|i| self.apply(f64::from(i) * step)).fold((0.0f64, 1.0f64), |(low, high), v| (low.min(v), high.max(v)));
        (low - margin, high + margin)
    }

    /// Where `t`, between 0 and 1, lands on the curve.
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::Hold => {
                if t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Ease::EaseIn => t * t,
            Ease::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Ease::EaseInOut => t * t * (3.0 - 2.0 * t),
            Ease::InCubic => t * t * t,
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
            Ease::InSine => 1.0 - (PI * t / 2.0).cos(),
            Ease::OutSine => (PI * t / 2.0).sin(),
            Ease::InOutSine => (1.0 - (PI * t).cos()) / 2.0,
            Ease::OutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f64.powf(-10.0 * t)
                }
            }
            Ease::BackIn { amplitude } => (amplitude + 1.0) * t * t * t - amplitude * t * t,
            Ease::BackOut { amplitude } => 1.0 + (amplitude + 1.0) * (t - 1.0).powi(3) + amplitude * (t - 1.0).powi(2),
            Ease::Elastic { amplitude, period } => {
                if t <= 0.0 || t >= 1.0 {
                    t
                } else {
                    let shift = period / (2.0 * PI) * (1.0 / amplitude).asin();
                    amplitude * 2f64.powf(-10.0 * t) * ((t - shift) * (2.0 * PI / period)).sin() + 1.0
                }
            }
            Ease::Bounce { bounces, decay } => out_bounce(t, bounces, decay),
            Ease::Power { exponent, mode } => match mode {
                PowerMode::In => t.powf(exponent),
                PowerMode::Out => 1.0 - (1.0 - t).powf(exponent),
                PowerMode::InOut => {
                    if t < 0.5 {
                        0.5 * (2.0 * t).powf(exponent)
                    } else {
                        1.0 - 0.5 * (2.0 - 2.0 * t).powf(exponent)
                    }
                }
            },
            Ease::Steps { count, position } => steps(t, count, position),
            Ease::Wave { amplitude, cycles, decay } => {
                if t >= 1.0 {
                    1.0
                } else {
                    let turn = 2.0 * PI * cycles;
                    t + amplitude * ((turn * t).sin() - t * turn.sin()) * (1.0 - t).powf(decay)
                }
            }
            Ease::CubicBezier([x1, y1, x2, y2]) => cubic_bezier(x1, y1, x2, y2, t),
        }
    }
}

/// The out bounce of [`Ease::Bounce`]. In units where the fall lasts 1, arc `k` (from 1) lasts
/// `2 decay^k` and rises `decay^(2k)` below 1.
fn out_bounce(t: f64, bounces: u32, decay: f64) -> f64 {
    if t >= 1.0 {
        return 1.0;
    }
    let total = 1.0 + (1..bounces).map(|k| 2.0 * decay.powi(k as i32)).sum::<f64>();
    let mut x = t * total;
    if x < 1.0 {
        return x * x;
    }
    x -= 1.0;
    for k in 1..bounces {
        let half = decay.powi(k as i32);
        if x < 2.0 * half || k + 1 == bounces {
            let c = (x - half).min(half);
            return 1.0 - half * half + c * c;
        }
        x -= 2.0 * half;
    }
    1.0
}

/// CSS `steps(count, position)` at `t`, with its ends kept at 0 and 1.
fn steps(t: f64, count: u32, position: StepPosition) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let count = f64::from(count);
    let mut step = (t * count).floor();
    if matches!(position, StepPosition::Start | StepPosition::Both) {
        step += 1.0;
    }
    let jumps = match position {
        StepPosition::Start | StepPosition::End => count,
        StepPosition::Both => count + 1.0,
        StepPosition::None => count - 1.0,
    }
    .max(1.0);
    (step.min(jumps) / jumps).clamp(0.0, 1.0)
}

/// The `y` of the CSS cubic Bézier through (0,0), (x1,y1), (x2,y2), (1,1) where its `x` is `x`.
fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let bezier = |a: f64, b: f64, s: f64| {
        let r = 1.0 - s;
        3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
    };
    // x(s) only rises when x1 and x2 are in 0..1 (CSS requires it), so bisection always finds s.
    let (x1, x2) = (x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0));
    let (mut low, mut high) = (0.0f64, 1.0f64);
    let mut s = x;
    for _ in 0..64 {
        let at = bezier(x1, x2, s);
        if (at - x).abs() < 1e-12 {
            break;
        }
        if at < x {
            low = s;
        } else {
            high = s;
        }
        s = (low + high) / 2.0;
    }
    bezier(y1, y2, s)
}

/// A property a curve can move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CurveProperty {
    /// Shift, in parts of the output frame width and height.
    Dx,
    Dy,
    /// Scale around the middle of the picture, both ways.
    Scale,
    /// Scale of the width or the height alone, on top of `scale`.
    ScaleX,
    ScaleY,
    /// Added turn, in degrees.
    Rotation,
    /// Multiplies the opacity.
    Opacity,
    /// Blur radius, in parts of the output frame height.
    Blur,
}

impl CurveProperty {
    /// The value of the property when a curve has no track of it.
    pub fn rest(self) -> f64 {
        match self {
            CurveProperty::Scale | CurveProperty::ScaleX | CurveProperty::ScaleY | CurveProperty::Opacity => 1.0,
            CurveProperty::Dx | CurveProperty::Dy | CurveProperty::Rotation | CurveProperty::Blur => 0.0,
        }
    }
}

/// One keyframe of a curve track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurveKey {
    /// Normalized time, 0 to 1.
    pub t: f64,
    pub value: f64,
    /// How the value goes from this keyframe to the next.
    #[serde(default)]
    pub ease: Ease,
}

/// A preset animation as data: per property, keyframes in rising `t`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnimationCurve {
    pub tracks: BTreeMap<CurveProperty, Vec<CurveKey>>,
}

impl AnimationCurve {
    /// This curve with `keys` as the track of `property`.
    pub fn with(mut self, property: CurveProperty, keys: Vec<CurveKey>) -> Self {
        self.tracks.insert(property, keys);
        self
    }

    /// The value of `property` at `t`: the first or last keyframe outside the keyframes, the eased
    /// mix of the two around `t` between them.
    pub fn value_at(&self, property: CurveProperty, t: f64) -> f64 {
        let Some(keys) = self.tracks.get(&property).filter(|keys| !keys.is_empty()) else {
            return property.rest();
        };
        let first = keys[0];
        if t <= first.t {
            return first.value;
        }
        for pair in keys.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if t < b.t {
                let span = b.t - a.t;
                let u = if span > 0.0 { (t - a.t) / span } else { 1.0 };
                return a.value + (b.value - a.value) * a.ease.apply(u);
            }
        }
        keys[keys.len() - 1].value
    }
}

/// Checks a curve that came from outside (a pack): at least one track, none empty, every
/// keyframe finite with its `t` in 0..1, in rising `t`. The message says what is wrong.
pub fn check_curve(curve: &AnimationCurve) -> Result<(), String> {
    if curve.tracks.is_empty() {
        return Err("the curve has no track".to_owned());
    }
    for (property, keys) in &curve.tracks {
        if keys.is_empty() {
            return Err(format!("the {property:?} track has no keyframe"));
        }
        let mut last = 0.0;
        for key in keys {
            if !(key.t.is_finite() && (0.0..=1.0).contains(&key.t)) || !key.value.is_finite() {
                return Err(format!("a keyframe of the {property:?} track is not finite or its t is not in 0..1"));
            }
            if key.t < last {
                return Err(format!("the keyframes of the {property:?} track are not in rising t"));
            }
            if !key.ease.is_valid() {
                return Err(format!("a keyframe of the {property:?} track has an ease with bad numbers"));
            }
            last = key.t;
        }
    }
    Ok(())
}

/// A keyframe of a curve.
pub fn key(t: f64, value: f64, ease: Ease) -> CurveKey {
    CurveKey { t, value, ease }
}

/// The pose of `curve` at `t`.
pub fn evaluate_curve(curve: &AnimationCurve, t: f64) -> AnimationPose {
    let at = |property| curve.value_at(property, t);
    AnimationPose {
        dx: at(CurveProperty::Dx),
        dy: at(CurveProperty::Dy),
        scale: at(CurveProperty::Scale),
        scale_x: at(CurveProperty::ScaleX),
        scale_y: at(CurveProperty::ScaleY),
        rotation: at(CurveProperty::Rotation),
        opacity: at(CurveProperty::Opacity),
        blur: at(CurveProperty::Blur).max(0.0),
    }
}

/// One preset a clip can name.
#[derive(Debug, Clone, Copy)]
pub struct AnimationPreset {
    pub id: &'static str,
    pub group: AnimationGroup,
    /// The duration a new animation of this preset gets (for a repeat preset, one cycle).
    pub default_duration: Time,
    build: fn() -> AnimationCurve,
    /// The per-unit part of a preset that moves the units of a text one by one; such a preset
    /// is for text clips only.
    build_per_unit: Option<fn() -> PerUnit>,
}

impl PartialEq for AnimationPreset {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.group == other.group
    }
}

/// What the presets know once their curves are built.
struct Built {
    curve: AnimationCurve,
    per_unit: Option<PerUnit>,
    rotates: bool,
    max_scale: f64,
}

/// The largest size the track of `property` ever gives, by the bounds of its eases (see
/// [`Ease::range`]), so an ease that overshoots between samples is bounded all the same. The rest
/// value when the curve has no such track.
fn track_bound(curve: &AnimationCurve, property: CurveProperty) -> f64 {
    let Some(keys) = curve.tracks.get(&property).filter(|keys| !keys.is_empty()) else {
        return property.rest().abs();
    };
    let mut bound = keys.iter().map(|key| key.value.abs()).fold(0.0, f64::max);
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (low, high) = a.ease.range();
        bound = bound.max((a.value + (b.value - a.value) * low).abs()).max((a.value + (b.value - a.value) * high).abs());
    }
    bound
}

/// Whether `curve` turns the picture, and a bound of the largest scale it gives: the bound of the
/// scale track times the larger bound of the width and height tracks.
fn measure(curve: &AnimationCurve) -> (bool, f64) {
    let rotates = curve.tracks.get(&CurveProperty::Rotation).is_some_and(|keys| keys.iter().any(|key| key.value != 0.0));
    let bound = |property| track_bound(curve, property);
    let max_scale = (bound(CurveProperty::Scale) * bound(CurveProperty::ScaleX).max(bound(CurveProperty::ScaleY))).max(1.0);
    (rotates, max_scale)
}

fn built() -> &'static [Built] {
    static BUILT: OnceLock<Vec<Built>> = OnceLock::new();
    BUILT.get_or_init(|| {
        ANIMATION_PRESETS
            .iter()
            .map(|preset| {
                let curve = (preset.build)();
                let (rotates, max_scale) = measure(&curve);
                Built { curve, per_unit: preset.build_per_unit.map(|build| build()), rotates, max_scale }
            })
            .collect()
    })
}

impl AnimationPreset {
    fn built(&self) -> &'static Built {
        let index = ANIMATION_PRESETS.iter().position(|preset| preset == self).expect("a preset of the list");
        &built()[index]
    }

    /// The curve of the preset.
    pub fn curve(&self) -> &'static AnimationCurve {
        &self.built().curve
    }

    /// The per-unit part of the preset, for text clips.
    pub fn per_unit(&self) -> Option<&'static PerUnit> {
        self.built().per_unit.as_ref()
    }

    /// Whether the preset turns the picture.
    pub fn rotates(&self) -> bool {
        self.built().rotates
    }

    /// The largest scale the preset ever gives (a bound measured on its curve).
    pub fn max_scale(&self) -> f64 {
        self.built().max_scale
    }
}

/// A preset of a pack, as the editor read it.
#[derive(Debug, Clone, PartialEq)]
pub struct PackPreset {
    /// What a clip names: `"<pack id>/<item id>"`.
    pub reference: String,
    pub group: AnimationGroup,
    pub curve: AnimationCurve,
    /// The effect the preset runs on top of its curve, when its package is installed.
    pub effect: Option<PresetEffect>,
    /// The per-unit part of the preset, for text clips.
    pub per_unit: Option<PerUnit>,
    rotates: bool,
    max_scale: f64,
}

/// A gpufx effect a pack preset runs while it plays (see the module notes).
#[derive(Debug, Clone, PartialEq)]
pub struct PresetEffect {
    /// The package as an effect of a project names it.
    pub package: String,
    /// The parameter given the progress of the preset.
    pub progress: String,
    /// Static parameters.
    pub params: serde_json::Map<String, serde_json::Value>,
}

impl PresetEffect {
    /// The effect with its progress parameter at `progress`.
    pub fn at(&self, progress: f64) -> Effect {
        let mut params = self.params.clone();
        params.insert(self.progress.clone(), serde_json::Value::from(progress));
        Effect { package: self.package.clone(), params, keyframes: BTreeMap::new(), region: None }
    }
}

impl PackPreset {
    pub fn new(reference: String, group: AnimationGroup, curve: AnimationCurve) -> Self {
        let (rotates, max_scale) = measure(&curve);
        PackPreset { reference, group, curve, effect: None, per_unit: None, rotates, max_scale }
    }

    /// The preset with `per_unit` moving the units of a text.
    pub fn with_per_unit(mut self, per_unit: Option<PerUnit>) -> Self {
        self.per_unit = per_unit;
        self
    }

    /// The preset with `effect` run on top of its curve.
    pub fn with_effect(mut self, effect: Option<PresetEffect>) -> Self {
        self.effect = effect;
        self
    }

    /// Whether the preset turns the picture.
    pub fn rotates(&self) -> bool {
        self.rotates
    }

    /// The largest scale the preset ever gives.
    pub fn max_scale(&self) -> f64 {
        self.max_scale
    }
}

/// The presets of the installed packs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackPresets {
    presets: Vec<PackPreset>,
}

impl PackPresets {
    pub fn new(presets: Vec<PackPreset>) -> Self {
        PackPresets { presets }
    }

    /// The preset `reference` of `group`.
    pub fn find(&self, group: AnimationGroup, reference: &str) -> Option<&PackPreset> {
        self.presets.iter().find(|preset| preset.group == group && preset.reference == reference)
    }

    pub fn presets(&self) -> &[PackPreset] {
        &self.presets
    }
}

fn pack_table() -> &'static RwLock<Arc<PackPresets>> {
    static TABLE: OnceLock<RwLock<Arc<PackPresets>>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

/// Replaces the presets of packs every clip of the process can name.
pub fn set_pack_presets(presets: PackPresets) {
    *pack_table().write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(presets);
}

/// The presets of packs now set.
pub fn pack_presets() -> Arc<PackPresets> {
    pack_table().read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Whether `id` names a preset of a pack rather than a built-in one.
pub fn is_pack_preset(id: &str) -> bool {
    id.contains(PACK_PRESET_SEPARATOR)
}

/// A preset found, built in or from a pack.
#[derive(Clone, Copy)]
struct Resolved<'a> {
    curve: &'a AnimationCurve,
    per_unit: Option<&'a PerUnit>,
    rotates: bool,
    max_scale: f64,
}

fn resolve<'a>(group: AnimationGroup, id: &str, packs: &'a PackPresets) -> Option<Resolved<'a>> {
    if let Some(preset) = find_preset(group, id) {
        return Some(Resolved { curve: preset.curve(), per_unit: preset.per_unit(), rotates: preset.rotates(), max_scale: preset.max_scale() });
    }
    packs.find(group, id).map(|preset| Resolved { curve: &preset.curve, per_unit: preset.per_unit.as_ref(), rotates: preset.rotates, max_scale: preset.max_scale })
}

/// How a preset changes the picture at one moment, on top of the clip's own motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationPose {
    /// Shift, in parts of the output frame width and height.
    pub dx: f64,
    pub dy: f64,
    /// Scale around the middle of the picture.
    pub scale: f64,
    /// Scale of the width or the height alone, on top of `scale`.
    pub scale_x: f64,
    pub scale_y: f64,
    /// Added turn, in degrees.
    pub rotation: f64,
    /// Multiplies the opacity.
    pub opacity: f64,
    /// Added blur radius, in parts of the output frame height. 0 is no blur.
    pub blur: f64,
}

impl AnimationPose {
    pub const IDENTITY: AnimationPose = AnimationPose { dx: 0.0, dy: 0.0, scale: 1.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, opacity: 1.0, blur: 0.0 };

    /// Both poses at once.
    pub fn then(self, other: AnimationPose) -> AnimationPose {
        AnimationPose {
            dx: self.dx + other.dx,
            dy: self.dy + other.dy,
            scale: self.scale * other.scale,
            scale_x: self.scale_x * other.scale_x,
            scale_y: self.scale_y * other.scale_y,
            rotation: self.rotation + other.rotation,
            opacity: self.opacity * other.opacity,
            blur: self.blur + other.blur,
        }
    }
}

/// How far a slide travels, in parts of the frame.
const SLIDE_DISTANCE: f64 = 0.2;
/// How high a bounce goes, in parts of the frame height.
const BOUNCE_HEIGHT: f64 = 0.03;
/// The largest scale of a pulse.
const PULSE_PEAK: f64 = 1.08;
/// How far a shake moves to each side, in parts of the frame width.
const SHAKE_DISTANCE: f64 = 0.01;
/// How many times a shake goes back and forth in one cycle.
const SHAKE_TIMES: usize = 3;

/// Of the time of a typewriter, the part over which the characters start.
const TYPEWRITER_STAGGER: f64 = 0.9;
/// Of the time of a tumble, the part over which the characters start; each one turns in the rest.
const TUMBLE_STAGGER: f64 = 0.6;
/// How far a tumbling character rises into place, in font sizes.
const TUMBLE_RISE: f64 = 0.3;
/// The overshoot of a tumbling character.
const TUMBLE_OVERSHOOT: f64 = 1.8;

/// Half a second, the default duration of an enter or exit preset.
const HALF_SECOND: Time = Time::from_ticks(TICKS_PER_SECOND / 2);
/// One second, the default cycle of a repeat preset.
const ONE_SECOND: Time = Time::from_ticks(TICKS_PER_SECOND);

const fn preset(id: &'static str, group: AnimationGroup, build: fn() -> AnimationCurve) -> AnimationPreset {
    let default_duration = match group {
        AnimationGroup::Repeat => ONE_SECOND,
        _ => HALF_SECOND,
    };
    AnimationPreset { id, group, default_duration, build, build_per_unit: None }
}

/// A preset that moves the units of a text one by one, `ticks` long by default.
const fn unit_preset(id: &'static str, group: AnimationGroup, ticks: i64, build_per_unit: fn() -> PerUnit) -> AnimationPreset {
    AnimationPreset { id, group, default_duration: Time::from_ticks(ticks), build: curve, build_per_unit: Some(build_per_unit) }
}

/// Typewriter: each character shows at once, in turn; the last one at 90 % of the time, so the
/// whole text holds a moment before the animation ends.
fn typewriter_in() -> PerUnit {
    PerUnit { unit: TextUnit::Char, order: UnitOrder::Forward, seed: 0, stagger: Some(TYPEWRITER_STAGGER), selector: None, curve: curve().with(Opacity, vec![key(0.0, 0.0, Ease::Hold), key(0.0, 1.0, Ease::Linear)]) }
}
/// The typewriter backwards: the last character goes first.
fn typewriter_out() -> PerUnit {
    PerUnit { unit: TextUnit::Char, order: UnitOrder::Backward, seed: 0, stagger: Some(TYPEWRITER_STAGGER), selector: None, curve: curve().with(Opacity, vec![key(0.0, 1.0, Ease::Hold), key(0.0, 0.0, Ease::Linear)]) }
}
/// Tumble: each character flips up into place in turn, rising a little and overshooting, as the
/// `tumble` of the AI video studio (a 3D turn about the line, drawn here as a squash of the height).
fn tumble_in() -> PerUnit {
    let settle = Ease::BackOut { amplitude: TUMBLE_OVERSHOOT };
    PerUnit {
        unit: TextUnit::Char,
        order: UnitOrder::Forward,
        seed: 0,
        stagger: Some(TUMBLE_STAGGER),
        selector: None,
        curve: curve()
            .with(CurveProperty::ScaleY, ramp(0.0, 1.0, settle))
            .with(Dy, ramp(TUMBLE_RISE, 0.0, settle))
            .with(Opacity, vec![key(0.0, 0.0, Ease::Linear), key(1.0 / 3.0, 1.0, Ease::Linear)]),
    }
}

/// A track from `from` to `to` over the whole curve with `ease`.
fn ramp(from: f64, to: f64, ease: Ease) -> Vec<CurveKey> {
    vec![key(0.0, from, ease), key(1.0, to, Ease::Linear)]
}

fn curve() -> AnimationCurve {
    AnimationCurve::default()
}

use CurveProperty::{Dx, Dy, Opacity, Rotation, Scale};

fn appear() -> Vec<CurveKey> {
    ramp(0.0, 1.0, Ease::OutCubic)
}

fn vanish() -> Vec<CurveKey> {
    ramp(1.0, 0.0, Ease::InCubic)
}

fn fade_in() -> AnimationCurve {
    curve().with(Opacity, ramp(0.0, 1.0, Ease::Linear))
}
fn fade_out() -> AnimationCurve {
    curve().with(Opacity, ramp(1.0, 0.0, Ease::Linear))
}
// A slide is named by the way it moves: `slide-left` comes in from the right and leaves to the left.
fn slide_in(property: CurveProperty, from: f64) -> AnimationCurve {
    curve().with(property, ramp(from, 0.0, Ease::OutCubic)).with(Opacity, appear())
}
fn slide_out(property: CurveProperty, to: f64) -> AnimationCurve {
    curve().with(property, ramp(0.0, to, Ease::InCubic)).with(Opacity, vanish())
}
fn slide_left_in() -> AnimationCurve {
    slide_in(Dx, SLIDE_DISTANCE)
}
fn slide_right_in() -> AnimationCurve {
    slide_in(Dx, -SLIDE_DISTANCE)
}
fn slide_up_in() -> AnimationCurve {
    slide_in(Dy, SLIDE_DISTANCE)
}
fn slide_down_in() -> AnimationCurve {
    slide_in(Dy, -SLIDE_DISTANCE)
}
fn slide_left_out() -> AnimationCurve {
    slide_out(Dx, -SLIDE_DISTANCE)
}
fn slide_right_out() -> AnimationCurve {
    slide_out(Dx, SLIDE_DISTANCE)
}
fn slide_up_out() -> AnimationCurve {
    slide_out(Dy, -SLIDE_DISTANCE)
}
fn slide_down_out() -> AnimationCurve {
    slide_out(Dy, SLIDE_DISTANCE)
}
fn zoom_in() -> AnimationCurve {
    curve().with(Scale, appear()).with(Opacity, appear())
}
fn zoom_out() -> AnimationCurve {
    curve().with(Scale, vanish()).with(Opacity, vanish())
}
fn pop_in() -> AnimationCurve {
    curve()
        .with(Scale, ramp(0.0, 1.0, Ease::BackOut { amplitude: Ease::BACK_AMPLITUDE }))
        .with(Opacity, vec![key(0.0, 0.0, Ease::Linear), key(1.0 / 3.0, 1.0, Ease::Linear)])
}
fn pop_out() -> AnimationCurve {
    curve()
        .with(Scale, ramp(1.0, 0.0, Ease::BackIn { amplitude: Ease::BACK_AMPLITUDE }))
        .with(Opacity, vec![key(2.0 / 3.0, 1.0, Ease::Linear), key(1.0, 0.0, Ease::Linear)])
}
fn spin_in() -> AnimationCurve {
    curve().with(Scale, appear()).with(Rotation, ramp(-180.0, 0.0, Ease::OutCubic)).with(Opacity, appear())
}
fn spin_out() -> AnimationCurve {
    curve().with(Scale, vanish()).with(Rotation, ramp(0.0, 180.0, Ease::InCubic)).with(Opacity, vanish())
}
fn bounce() -> AnimationCurve {
    curve().with(Dy, vec![key(0.0, 0.0, Ease::OutSine), key(0.5, -BOUNCE_HEIGHT, Ease::InSine), key(1.0, 0.0, Ease::Linear)])
}
fn pulse() -> AnimationCurve {
    curve().with(Scale, vec![key(0.0, 1.0, Ease::InOutSine), key(0.5, PULSE_PEAK, Ease::InOutSine), key(1.0, 1.0, Ease::Linear)])
}
fn shake() -> AnimationCurve {
    // Each quarter of a back-and-forth is a quarter of a sine.
    let quarters = 4 * SHAKE_TIMES;
    let peaks = [0.0, SHAKE_DISTANCE, 0.0, -SHAKE_DISTANCE];
    let keys = (0..=quarters)
        .map(|i| {
            let ease = if i % 2 == 0 { Ease::OutSine } else { Ease::InSine };
            key(i as f64 / quarters as f64, peaks[i % 4], ease)
        })
        .collect();
    curve().with(Dx, keys)
}

/// Every preset, by group. Enter and exit presets share their ids and mirror each other.
pub const ANIMATION_PRESETS: &[AnimationPreset] = &[
    preset("fade", AnimationGroup::Enter, fade_in),
    preset("slide-left", AnimationGroup::Enter, slide_left_in),
    preset("slide-right", AnimationGroup::Enter, slide_right_in),
    preset("slide-up", AnimationGroup::Enter, slide_up_in),
    preset("slide-down", AnimationGroup::Enter, slide_down_in),
    preset("zoom", AnimationGroup::Enter, zoom_in),
    preset("pop", AnimationGroup::Enter, pop_in),
    preset("spin", AnimationGroup::Enter, spin_in),
    unit_preset("typewriter", AnimationGroup::Enter, TICKS_PER_SECOND, typewriter_in),
    unit_preset("tumble", AnimationGroup::Enter, TICKS_PER_SECOND, tumble_in),
    preset("fade", AnimationGroup::Exit, fade_out),
    preset("slide-left", AnimationGroup::Exit, slide_left_out),
    preset("slide-right", AnimationGroup::Exit, slide_right_out),
    preset("slide-up", AnimationGroup::Exit, slide_up_out),
    preset("slide-down", AnimationGroup::Exit, slide_down_out),
    preset("zoom", AnimationGroup::Exit, zoom_out),
    preset("pop", AnimationGroup::Exit, pop_out),
    preset("spin", AnimationGroup::Exit, spin_out),
    unit_preset("typewriter", AnimationGroup::Exit, TICKS_PER_SECOND, typewriter_out),
    preset("bounce", AnimationGroup::Repeat, bounce),
    preset("pulse", AnimationGroup::Repeat, pulse),
    preset("shake", AnimationGroup::Repeat, shake),
];

/// The preset `id` of `group`, `None` when there is none.
pub fn find_preset(group: AnimationGroup, id: &str) -> Option<&'static AnimationPreset> {
    ANIMATION_PRESETS.iter().find(|preset| preset.group == group && preset.id == id)
}

/// The pose of preset `id` of `group` at `progress` (0 to 1; for a repeat preset, the phase in
/// cycles, of which only the fraction counts), `None` when the preset is not known.
/// A pack preset is looked up in the table [`set_pack_presets`] set.
pub fn preset_pose(group: AnimationGroup, id: &str, progress: f64) -> Option<AnimationPose> {
    let packs = pack_presets();
    resolve(group, id, &packs).map(|preset| curve_pose(group, preset.curve, progress))
}

fn curve_pose(group: AnimationGroup, curve: &AnimationCurve, progress: f64) -> AnimationPose {
    let t = match group {
        AnimationGroup::Repeat => progress.rem_euclid(1.0),
        _ => progress.clamp(0.0, 1.0),
    };
    evaluate_curve(curve, t)
}

/// The preset and its ticks when it will run, `None` when it is unknown or does not last.
fn usable<'a>(group: AnimationGroup, reference: &Option<AnimationRef>, packs: &'a PackPresets) -> Option<(Resolved<'a>, f64)> {
    let reference = reference.as_ref()?;
    // A hold (negative duration) runs for its length, over the still it adds to the clip.
    let ticks = reference.length().ticks();
    if ticks <= 0 {
        return None;
    }
    resolve(group, &reference.preset, packs).map(|preset| (preset, ticks as f64))
}

/// The presets of `animation` that run at `clock`, enter, exit then repeat, each with how far it
/// has played: 0 to 1 for enter and exit, cycles for repeat (see the module timing rules).
/// The presets running at `clock`: group, id, progress over the window and the window's length in
/// ticks.
fn running<'a>(animation: &'a ClipAnimation, clock: ClipClock, packs: &PackPresets) -> Vec<(AnimationGroup, &'a str, f64, f64)> {
    let elapsed = clock.elapsed.ticks() as f64;
    let total = clock.duration.ticks().max(0) as f64;
    let name = |reference: &'a Option<AnimationRef>| reference.as_ref().map_or("", |reference| reference.preset.as_str());
    let enter = usable(AnimationGroup::Enter, &animation.enter, packs).map(|(_, d)| d);
    let exit = usable(AnimationGroup::Exit, &animation.exit, packs).map(|(_, d)| d);
    let both = enter.unwrap_or(0.0) + exit.unwrap_or(0.0);
    let shrink = if both > total { total / both } else { 1.0 };
    let mut runs = Vec::new();
    if let Some(duration) = enter {
        let duration = duration * shrink;
        if duration > 0.0 && elapsed < duration {
            runs.push((AnimationGroup::Enter, name(&animation.enter), elapsed / duration, duration));
        } else if duration <= 0.0 {
            // Nothing visible to play over: keep the first pose rather than a settled one.
            runs.push((AnimationGroup::Enter, name(&animation.enter), 0.0, 0.0));
        }
    }
    if let Some(duration) = exit {
        let duration = duration * shrink;
        let from = total - duration;
        if duration > 0.0 && elapsed > from {
            runs.push((AnimationGroup::Exit, name(&animation.exit), (elapsed - from) / duration, duration));
        }
    }
    if let Some((_, period)) = usable(AnimationGroup::Repeat, &animation.repeat, packs) {
        runs.push((AnimationGroup::Repeat, name(&animation.repeat), elapsed / period, period));
    }
    runs
}

/// The pose of `animation` at `clock` (see the module timing rules).
pub fn animation_pose(animation: &ClipAnimation, clock: ClipClock) -> AnimationPose {
    let packs = pack_presets();
    let mut pose = AnimationPose::IDENTITY;
    for (group, id, progress, _) in running(animation, clock, &packs) {
        if let Some(preset) = resolve(group, id, &packs) {
            pose = pose.then(curve_pose(group, preset.curve, progress));
        }
    }
    pose
}

/// The effects the presets of `animation` run at `clock`, each with its progress parameter set
/// (see the module notes for the progress of each group), for output frames of `frame_rate`. Empty
/// outside every window, for presets without an effect and for a clip with no visible time.
pub fn animation_effects(animation: &ClipAnimation, clock: ClipClock, frame_rate: FrameRate) -> Vec<Effect> {
    if animation.is_empty() || clock.duration.ticks() <= 0 {
        return Vec::new();
    }
    // The exact length of one frame (3753.75 ticks at 23.976 fps), not a whole-tick floor of it.
    let frame = if frame_rate.is_valid() { f64::from(frame_rate.den) * TICKS_PER_SECOND as f64 / f64::from(frame_rate.num) } else { 0.0 };
    // The last frame of a window starts one frame before its end, and has progress 1.
    let by_frames = |progress: f64, window: f64| {
        let span = window - frame;
        if span <= 0.0 {
            1.0
        } else {
            (progress * window / span).clamp(0.0, 1.0)
        }
    };
    let packs = pack_presets();
    running(animation, clock, &packs)
        .into_iter()
        .filter_map(|(group, id, progress, window)| {
            let effect = packs.find(group, id)?.effect.as_ref()?;
            let value = match group {
                AnimationGroup::Enter => by_frames(progress, window),
                AnimationGroup::Exit => 1.0 - by_frames(progress, window),
                AnimationGroup::Repeat => progress.rem_euclid(1.0),
            };
            Some(effect.at(value))
        })
        .collect()
}

/// The effects a sample is drawn with: the clip's own, their keyframes applied, then those of its
/// preset animations (see [`animation_effects`]), for output frames of `frame_rate`.
pub fn effects_with_animation<'a>(sample: &SourceSample<'a>, frame_rate: FrameRate) -> Cow<'a, [Effect]> {
    let own = effects_at(sample.effects, sample.offset);
    let added = animation_effects(&sample.model.animation, sample.clock, frame_rate);
    if added.is_empty() {
        return own;
    }
    let mut all = own.into_owned();
    all.extend(added);
    Cow::Owned(all)
}

/// The pose of each unit of a text, by the kind of unit, of every per-unit preset of `animation`
/// that runs at `clock`, in the order enter, exit, repeat. `count` gives how many units of a kind
/// the text has. Empty when none runs, so the text is drawn as it is.
pub fn animation_unit_poses(animation: &ClipAnimation, clock: ClipClock, count: impl Fn(TextUnit) -> usize) -> Vec<(TextUnit, Vec<AnimationPose>)> {
    if animation.is_empty() || clock.duration.ticks() <= 0 {
        return Vec::new();
    }
    let packs = pack_presets();
    let poses = running(animation, clock, &packs)
        .into_iter()
        .filter_map(|(group, id, progress, _)| {
            let per_unit = resolve(group, id, &packs)?.per_unit?;
            let t = match group {
                AnimationGroup::Repeat => progress.rem_euclid(1.0),
                _ => progress.clamp(0.0, 1.0),
            };
            Some((per_unit.unit, unit_poses(per_unit, count(per_unit.unit), t)))
        })
        .collect();
    poses
}

/// Whether a preset of `animation` moves the units of a text one by one.
pub fn animation_has_per_unit(animation: &ClipAnimation) -> bool {
    if animation.is_empty() {
        return false;
    }
    let packs = pack_presets();
    let found = presets_of(animation, &packs).any(|(_, preset)| preset.per_unit.is_some());
    found
}

/// Whether the preset `id` of `group`, built in or of a pack, moves the units of a text.
pub fn preset_is_per_unit(group: AnimationGroup, id: &str) -> bool {
    let packs = pack_presets();
    let found = resolve(group, id, &packs).is_some_and(|preset| preset.per_unit.is_some());
    found
}

fn presets_of<'a>(animation: &'a ClipAnimation, packs: &'a PackPresets) -> impl Iterator<Item = (AnimationGroup, Resolved<'a>)> + 'a {
    [(AnimationGroup::Enter, &animation.enter), (AnimationGroup::Exit, &animation.exit), (AnimationGroup::Repeat, &animation.repeat)]
        .into_iter()
        .filter_map(move |(group, reference)| usable(group, reference, packs).map(|(preset, _)| (group, preset)))
}

/// Whether any preset of `animation` turns the picture.
pub fn animation_rotates(animation: &ClipAnimation) -> bool {
    if animation.is_empty() {
        return false;
    }
    let packs = pack_presets();
    let rotates = presets_of(animation, &packs).any(|(_, preset)| preset.rotates);
    rotates
}

/// The largest scale `animation` ever gives: the larger of enter and exit, times the repeat.
pub fn animation_max_scale(animation: &ClipAnimation) -> f64 {
    if animation.is_empty() {
        return 1.0;
    }
    let mut once = 1.0f64;
    let mut repeat = 1.0f64;
    let packs = pack_presets();
    for (group, preset) in presets_of(animation, &packs) {
        match group {
            AnimationGroup::Repeat => repeat = repeat.max(preset.max_scale),
            _ => once = once.max(preset.max_scale),
        }
    }
    once * repeat
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only test of the crate that sets the pack table, so tests running at the same time do
    /// not race on it.
    #[test]
    fn a_pack_preset_plays_from_the_table_and_a_missing_one_is_left_out() {
        let rise = AnimationCurve::default().with(CurveProperty::Opacity, vec![key(0.0, 0.0, Ease::Linear), key(1.0, 1.0, Ease::Linear)]).with(CurveProperty::Scale, vec![key(0.0, 2.0, Ease::Linear), key(1.0, 1.0, Ease::Linear)]);
        set_pack_presets(PackPresets::new(vec![PackPreset::new("test-pack/rise".to_owned(), AnimationGroup::Enter, rise)]));
        let clock = |elapsed| ClipClock { elapsed: Time::from_ticks(elapsed), duration: Time::from_ticks(100_000) };
        let named = |preset: &str| ClipAnimation { enter: Some(AnimationRef { preset: preset.to_owned(), duration: Time::from_ticks(10_000) }), ..ClipAnimation::default() };

        let animation = named("test-pack/rise");
        let pose = animation_pose(&animation, clock(5000));
        assert!((pose.opacity - 0.5).abs() < 1e-9 && (pose.scale - 1.5).abs() < 1e-9, "{pose:?}");
        assert!((animation_max_scale(&animation) - 2.0).abs() < 1e-9);
        assert!(!animation_rotates(&animation));
        assert!(preset_pose(AnimationGroup::Exit, "test-pack/rise", 0.5).is_none(), "only in its own group");

        let missing = named("gone-pack/rise");
        assert_eq!(animation_pose(&missing, clock(5000)), AnimationPose::IDENTITY);
        assert!(is_pack_preset("gone-pack/rise") && !is_pack_preset("fade"));

        preset_effects_play_in_their_windows();
    }

    /// Part of the test above, which owns the pack table.
    fn preset_effects_play_in_their_windows() {
        let effect = |package: &str| {
            let mut params = serde_json::Map::new();
            params.insert("amount".to_owned(), serde_json::Value::from(2.0));
            Some(PresetEffect { package: package.to_owned(), progress: "t".to_owned(), params })
        };
        let opacity = AnimationCurve::default().with(CurveProperty::Opacity, vec![key(0.0, 1.0, Ease::Linear)]);
        set_pack_presets(PackPresets::new(vec![
            PackPreset::new("p/in".to_owned(), AnimationGroup::Enter, opacity.clone()).with_effect(effect("fx-in")),
            PackPreset::new("p/out".to_owned(), AnimationGroup::Exit, opacity.clone()).with_effect(effect("fx-out")),
            PackPreset::new("p/loop".to_owned(), AnimationGroup::Repeat, opacity.clone()).with_effect(effect("fx-loop")),
            // A preset whose package was not installed keeps its curve and has no effect.
            PackPreset::new("p/bare".to_owned(), AnimationGroup::Enter, opacity),
        ]));
        let slot = |preset: &str, ticks| Some(AnimationRef { preset: preset.to_owned(), duration: Time::from_ticks(ticks) });
        let clock = |elapsed| ClipClock { elapsed: Time::from_ticks(elapsed), duration: Time::from_ticks(1000) };
        let progress = |effects: &[Effect], package: &str| effects.iter().find(|e| e.package == package).map(|e| e.params["t"].as_f64().unwrap());
        // Frames 100 ticks long: frames start at 0, 100, ..., 900 of a clip 1000 long.
        let frame = FrameRate::whole(900);

        let both = ClipAnimation { enter: slot("p/in", 200), exit: slot("p/out", 400), ..ClipAnimation::default() };
        let at = |elapsed| animation_effects(&both, clock(elapsed), frame);
        assert_eq!(progress(&at(0), "fx-in"), Some(0.0), "enter starts at 0");
        assert_eq!(progress(&at(50), "fx-in"), Some(0.5));
        assert_eq!(progress(&at(100), "fx-in"), Some(1.0), "the last frame of the enter is settled");
        assert_eq!(at(100)[0].params["amount"], serde_json::Value::from(2.0), "static parameters kept");
        assert!(at(200).is_empty() && at(600).is_empty(), "nothing between the windows, and at their edges");
        assert_eq!(progress(&at(700), "fx-out"), Some(1.0 - 100.0 / 300.0), "exit falls from 1");
        assert_eq!(progress(&at(750), "fx-out"), Some(0.5));
        assert_eq!(progress(&at(900), "fx-out"), Some(0.0), "exit ends at 0 on the last drawn frame");
        assert_eq!(at(800).len(), 1);
        let empty = ClipClock { elapsed: Time::from_ticks(0), duration: Time::from_ticks(0) };
        assert!(animation_effects(&both, empty, frame).is_empty(), "no effect on a clip with no visible time");

        let looped = ClipAnimation { enter: slot("p/in", 200), repeat: slot("p/loop", 300), ..ClipAnimation::default() };
        let effects = animation_effects(&looped, clock(150), frame);
        assert_eq!(effects.iter().map(|e| e.package.as_str()).collect::<Vec<_>>(), vec!["fx-in", "fx-loop"], "enter before repeat");
        assert_eq!(progress(&effects, "fx-loop"), Some(0.5));
        assert_eq!(progress(&animation_effects(&looped, clock(300), frame), "fx-loop"), Some(0.0), "a new cycle starts at 0");
        assert_eq!(progress(&animation_effects(&looped, clock(750), frame), "fx-loop"), Some(0.5));

        let bare = ClipAnimation { enter: slot("p/bare", 200), ..ClipAnimation::default() };
        assert!(animation_effects(&bare, clock(50), frame).is_empty());
        assert_eq!(animation_pose(&bare, clock(50)).opacity, 1.0);
        assert!(animation_effects(&ClipAnimation { enter: slot("fade", 200), ..ClipAnimation::default() }, clock(50), frame).is_empty(), "built-in presets have no effect");

        // 23.976 fps: an enter of four frames is 15015 ticks and its last frame starts 3753.75 ticks
        // before the end. Measured with the floored 3753-tick frame it would settle late.
        let film = ClipAnimation { enter: slot("p/in", 15_015), ..ClipAnimation::default() };
        let long = |elapsed| ClipClock { elapsed: Time::from_ticks(elapsed), duration: Time::from_ticks(90_000) };
        let last = progress(&animation_effects(&film, long(11_261), FrameRate::NTSC_FILM), "fx-in").unwrap();
        assert!((1.0 - last).abs() < 5e-5, "the last frame of the enter is settled: {last}");
        assert_eq!(progress(&animation_effects(&film, long(11_262), FrameRate::NTSC_FILM), "fx-in"), Some(1.0));
    }

    /// The hard-coded presets the curves replaced, kept to show the curves give the same poses.
    mod legacy {
        use std::f64::consts::PI;

        /// dx, dy, scale, rotation, opacity.
        pub type Pose = (f64, f64, f64, f64, f64);

        fn ease_out_cubic(p: f64) -> f64 {
            1.0 - (1.0 - p).powi(3)
        }

        fn ease_out_back(p: f64) -> f64 {
            const C1: f64 = 1.70158;
            const C3: f64 = C1 + 1.0;
            1.0 + C3 * (p - 1.0).powi(3) + C1 * (p - 1.0).powi(2)
        }

        pub fn enter(id: &str, progress: f64) -> Pose {
            let p = progress.clamp(0.0, 1.0);
            let eased = ease_out_cubic(p);
            let away = (1.0 - eased) * 0.2;
            match id {
                "fade" => (0.0, 0.0, 1.0, 0.0, p),
                "slide-left" => (away, 0.0, 1.0, 0.0, eased),
                "slide-right" => (-away, 0.0, 1.0, 0.0, eased),
                "slide-up" => (0.0, away, 1.0, 0.0, eased),
                "slide-down" => (0.0, -away, 1.0, 0.0, eased),
                "zoom" => (0.0, 0.0, eased, 0.0, eased),
                "pop" => (0.0, 0.0, ease_out_back(p).max(0.0), 0.0, (p * 3.0).min(1.0)),
                "spin" => (0.0, 0.0, eased, -180.0 * (1.0 - eased), eased),
                _ => unreachable!(),
            }
        }

        pub fn exit(id: &str, progress: f64) -> Pose {
            let (dx, dy, scale, rotation, opacity) = enter(id, 1.0 - progress.clamp(0.0, 1.0));
            if id.starts_with("slide-") || id == "spin" {
                (-dx, -dy, scale, -rotation, opacity)
            } else {
                (dx, dy, scale, rotation, opacity)
            }
        }

        pub fn repeat(id: &str, phase: f64) -> Pose {
            let p = phase.rem_euclid(1.0);
            match id {
                "bounce" => (0.0, -0.03 * (PI * p).sin().abs(), 1.0, 0.0, 1.0),
                "pulse" => (0.0, 0.0, 1.0 + 0.08 * (PI * p).sin().powi(2), 0.0, 1.0),
                "shake" => (0.01 * (2.0 * PI * 3.0 * p).sin(), 0.0, 1.0, 0.0, 1.0),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn curves_give_the_poses_of_the_old_presets() {
        for preset in ANIMATION_PRESETS.iter().filter(|preset| preset.per_unit().is_none()) {
            for step in -20..=4020 {
                let progress = f64::from(step) / 4000.0;
                let old = match preset.group {
                    AnimationGroup::Enter => legacy::enter(preset.id, progress),
                    AnimationGroup::Exit => legacy::exit(preset.id, progress),
                    AnimationGroup::Repeat => legacy::repeat(preset.id, progress),
                };
                let new = preset_pose(preset.group, preset.id, progress).unwrap();
                let got = (new.dx, new.dy, new.scale, new.rotation, new.opacity);
                let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
                assert!(near(got.0, old.0) && near(got.1, old.1) && near(got.2, old.2) && near(got.3, old.3) && near(got.4, old.4), "{} {:?} at {progress}: {got:?} vs {old:?}", preset.id, preset.group);
                assert_eq!((new.scale_x, new.scale_y, new.blur), (1.0, 1.0, 0.0));
            }
        }
    }

    #[test]
    fn per_unit_presets_settle_every_unit_and_stop_after_their_window() {
        use crate::per_unit::unit_poses;
        let units = 7;
        for preset in ANIMATION_PRESETS.iter().filter(|preset| preset.per_unit().is_some()) {
            let per_unit = preset.per_unit().unwrap();
            let at = if preset.group == AnimationGroup::Enter { 1.0 } else { 0.0 };
            for pose in unit_poses(per_unit, units, at) {
                assert!(pose.dx.abs() < 1e-9 && pose.dy.abs() < 1e-9 && (pose.scale_y - 1.0).abs() < 1e-9 && (pose.opacity - 1.0).abs() < 1e-9, "{} {:?}: {pose:?}", preset.id, preset.group);
            }
            assert_eq!(preset_pose(preset.group, preset.id, 0.5), Some(AnimationPose::IDENTITY), "the whole clip stays");
        }
        let animation = ClipAnimation { enter: Some(AnimationRef { preset: "typewriter".to_owned(), duration: Time::from_ticks(90_000) }), ..ClipAnimation::default() };
        assert!(animation_has_per_unit(&animation));
        assert!(preset_is_per_unit(AnimationGroup::Enter, "typewriter") && !preset_is_per_unit(AnimationGroup::Enter, "fade"));
        let clock = |elapsed| ClipClock { elapsed: Time::from_ticks(elapsed), duration: Time::from_ticks(900_000) };
        let middle = animation_unit_poses(&animation, clock(45_000), |_| units);
        assert_eq!(middle.len(), 1);
        let shown: Vec<f64> = middle[0].1.iter().map(|pose| pose.opacity).collect();
        assert!(shown[0] == 1.0 && shown[units - 1] == 0.0, "the first character shows before the last: {shown:?}");
        assert!(animation_unit_poses(&animation, clock(90_000), |_| units).is_empty(), "drawn as it is once the enter has played");
        assert!(animation_unit_poses(&animation, clock(0), |_| units)[0].1.iter().all(|pose| pose.opacity == 0.0), "nothing typed yet");
    }

    #[test]
    fn no_preset_scales_past_its_max_or_turns_when_it_says_not() {
        for preset in ANIMATION_PRESETS {
            for step in 0..=1000 {
                let pose = preset_pose(preset.group, preset.id, f64::from(step) / 1000.0).unwrap();
                assert!(pose.scale <= preset.max_scale() + 1e-9, "{} {:?} at {step}: {}", preset.id, preset.group, pose.scale);
                assert!(preset.rotates() || pose.rotation == 0.0, "{} turns", preset.id);
                assert!((0.0..=1.0).contains(&pose.opacity));
            }
        }
        assert!(find_preset(AnimationGroup::Enter, "pop").unwrap().max_scale() > 1.09);
        assert!(find_preset(AnimationGroup::Enter, "spin").unwrap().rotates());
    }

    #[test]
    fn enter_ends_settled_and_exit_starts_settled() {
        for preset in ANIMATION_PRESETS.iter().filter(|p| p.group != AnimationGroup::Repeat) {
            let at = if preset.group == AnimationGroup::Enter { 1.0 } else { 0.0 };
            let settled = preset_pose(preset.group, preset.id, at).unwrap();
            let still = AnimationPose::IDENTITY;
            assert!((settled.scale - still.scale).abs() < 1e-9 && settled.dx.abs() < 1e-9 && settled.dy.abs() < 1e-9 && settled.rotation.abs() < 1e-9 && (settled.opacity - 1.0).abs() < 1e-9, "{}", preset.id);
        }
    }

    #[test]
    fn every_ease_keeps_its_ends() {
        let eases = [
            Ease::Linear,
            Ease::EaseIn,
            Ease::EaseOut,
            Ease::EaseInOut,
            Ease::InCubic,
            Ease::OutCubic,
            Ease::InSine,
            Ease::OutSine,
            Ease::InOutSine,
            Ease::OutExpo,
            Ease::BackIn { amplitude: 1.70158 },
            Ease::BackOut { amplitude: 1.70158 },
            Ease::ELASTIC,
            Ease::BOUNCE,
            Ease::CubicBezier([0.25, 0.1, 0.25, 1.0]),
        ];
        for ease in eases {
            assert!(ease.apply(0.0).abs() < 1e-9 && (ease.apply(1.0) - 1.0).abs() < 1e-9, "{ease:?}");
        }
        assert!(Ease::BackOut { amplitude: 1.70158 }.apply(0.7) > 1.0);
        assert!((Ease::CubicBezier([0.0, 0.0, 1.0, 1.0]).apply(0.3) - 0.3).abs() < 1e-9, "the straight bezier");
        // CSS `ease` at x=0.5 is about 0.8024.
        assert!((Ease::CubicBezier([0.25, 0.1, 0.25, 1.0]).apply(0.5) - 0.8024).abs() < 1e-3);
        assert_eq!(Ease::Hold.apply(0.99), 0.0);
    }

    /// The classic out elastic and out bounce, as they were before they took numbers.
    fn classic_elastic(t: f64) -> f64 {
        if t <= 0.0 || t >= 1.0 {
            t
        } else {
            2f64.powf(-10.0 * t) * ((t * 10.0 - 0.75) * (2.0 * PI / 3.0)).sin() + 1.0
        }
    }

    fn classic_bounce(t: f64) -> f64 {
        const N: f64 = 7.5625;
        const D: f64 = 2.75;
        if t < 1.0 / D {
            N * t * t
        } else if t < 2.0 / D {
            let t = t - 1.5 / D;
            N * t * t + 0.75
        } else if t < 2.5 / D {
            let t = t - 2.25 / D;
            N * t * t + 0.9375
        } else {
            let t = t - 2.625 / D;
            N * t * t + 0.984375
        }
    }

    fn parametric_eases() -> Vec<Ease> {
        let mut eases = vec![
            Ease::ELASTIC,
            Ease::Elastic { amplitude: 1.8, period: 0.45 },
            Ease::Elastic { amplitude: 3.0, period: 0.005 },
            Ease::BOUNCE,
            Ease::Bounce { bounces: 1, decay: 0.5 },
            Ease::Bounce { bounces: 20, decay: 0.9 },
            Ease::Bounce { bounces: 7, decay: 0.01 },
            Ease::Wave { amplitude: 0.1, cycles: 2.0, decay: 0.0 },
            Ease::Wave { amplitude: -0.3, cycles: 2.37, decay: 0.0 },
            Ease::Wave { amplitude: 0.5, cycles: 0.25, decay: 3.0 },
            Ease::Wave { amplitude: 0.2, cycles: 500.5, decay: 1.0 },
        ];
        for mode in [PowerMode::In, PowerMode::Out, PowerMode::InOut] {
            for exponent in [0.3, 1.0, 3.0, 12.5] {
                eases.push(Ease::Power { exponent, mode });
            }
        }
        for position in [StepPosition::Start, StepPosition::End, StepPosition::Both, StepPosition::None] {
            for count in [2, 3, 1000] {
                eases.push(Ease::Steps { count, position });
            }
        }
        eases.push(Ease::Steps { count: 1, position: StepPosition::End });
        eases.push(Ease::Steps { count: 1, position: StepPosition::Start });
        eases
    }

    #[test]
    fn parametric_eases_end_exactly_on_0_and_1_and_stay_in_their_range() {
        for ease in parametric_eases() {
            assert!(ease.is_valid(), "{ease:?}");
            assert_eq!(ease.apply(0.0), 0.0, "{ease:?}");
            assert_eq!(ease.apply(1.0), 1.0, "{ease:?}");
            let (low, high) = ease.range();
            for i in 0..=100_000 {
                let v = ease.apply(f64::from(i) / 100_000.0);
                assert!(v.is_finite() && v >= low && v <= high, "{ease:?} at {i}: {v} not in {low}..{high}");
            }
        }
    }

    #[test]
    fn the_default_elastic_and_bounce_are_the_classic_ones() {
        for i in 0..=10_000 {
            let t = f64::from(i) / 10_000.0;
            assert!((Ease::ELASTIC.apply(t) - classic_elastic(t)).abs() < 1e-12, "elastic at {t}");
            assert!((Ease::BOUNCE.apply(t) - classic_bounce(t)).abs() < 1e-12, "bounce at {t}");
        }
    }

    #[test]
    fn parametric_eases_have_the_shapes_they_say() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert!(near(Ease::Power { exponent: 3.0, mode: PowerMode::In }.apply(0.5), 0.125));
        assert!(near(Ease::Power { exponent: 2.0, mode: PowerMode::Out }.apply(0.5), 0.75));
        let in_out = Ease::Power { exponent: 4.0, mode: PowerMode::InOut };
        assert!(near(in_out.apply(0.5), 0.5) && near(in_out.apply(0.25), 0.5 * 0.5f64.powi(4)) && near(in_out.apply(0.25) + in_out.apply(0.75), 1.0), "symmetric halves");
        // CSS steps(4, end) and steps(4, start).
        let end = Ease::Steps { count: 4, position: StepPosition::End };
        assert_eq!([0.1, 0.25, 0.6, 0.99].map(|t| end.apply(t)), [0.0, 0.25, 0.5, 0.75]);
        let start = Ease::Steps { count: 4, position: StepPosition::Start };
        assert_eq!([0.1, 0.25, 0.6, 0.99].map(|t| start.apply(t)), [0.25, 0.5, 0.75, 1.0]);
        let both = Ease::Steps { count: 4, position: StepPosition::Both };
        assert_eq!([0.1, 0.99].map(|t| both.apply(t)), [0.2, 0.8]);
        let none = Ease::Steps { count: 5, position: StepPosition::None };
        assert_eq!([0.1, 0.5, 0.99].map(|t| none.apply(t)), [0.0, 0.5, 1.0]);
        // The wave of whole cycles is the plain sine wobble.
        let wave = Ease::Wave { amplitude: 0.1, cycles: 2.0, decay: 0.0 };
        assert!((wave.apply(0.125) - (0.125 + 0.1)).abs() < 1e-12);
        let fading = Ease::Wave { amplitude: 0.1, cycles: 2.0, decay: 2.0 };
        assert!((fading.apply(0.125) - (0.125 + 0.1 * 0.875f64.powi(2))).abs() < 1e-12);
        assert!(Ease::Elastic { amplitude: 2.0, period: 0.3 }.apply(0.1) > Ease::ELASTIC.apply(0.1), "a larger amplitude swings further");
        // A ball that bounces once more lands 4 times... and touches 1 at each landing.
        let bounce = Ease::Bounce { bounces: 3, decay: 0.5 };
        assert!(near(bounce.apply(1.0 / 2.5), 1.0) && near(bounce.apply(2.0 / 2.5), 1.0));
        assert!(near(bounce.apply(1.5 / 2.5), 0.75), "the first rebound rises decay² = 1/4");
    }

    #[test]
    fn eases_with_bad_numbers_are_not_valid() {
        let bad = [
            Ease::Elastic { amplitude: 0.5, period: 0.3 },
            Ease::Elastic { amplitude: 1.0, period: 0.0 },
            Ease::Elastic { amplitude: f64::NAN, period: 0.3 },
            Ease::Elastic { amplitude: 1.0, period: f64::INFINITY },
            Ease::Bounce { bounces: 0, decay: 0.5 },
            Ease::Bounce { bounces: 21, decay: 0.5 },
            Ease::Bounce { bounces: 4, decay: 0.0 },
            Ease::Bounce { bounces: 4, decay: 1.0 },
            Ease::Bounce { bounces: 4, decay: f64::NAN },
            Ease::Power { exponent: 0.0, mode: PowerMode::In },
            Ease::Power { exponent: -2.0, mode: PowerMode::Out },
            Ease::Power { exponent: f64::INFINITY, mode: PowerMode::InOut },
            Ease::Steps { count: 0, position: StepPosition::End },
            Ease::Steps { count: 1001, position: StepPosition::End },
            Ease::Steps { count: 1, position: StepPosition::None },
            Ease::Wave { amplitude: f64::NAN, cycles: 2.0, decay: 0.0 },
            Ease::Wave { amplitude: 0.1, cycles: 0.0, decay: 0.0 },
            Ease::Wave { amplitude: 0.1, cycles: f64::INFINITY, decay: 0.0 },
            Ease::Wave { amplitude: 0.1, cycles: 2.0, decay: -1.0 },
        ];
        for ease in bad {
            assert!(!ease.is_valid(), "{ease:?}");
            let curve = curve().with(CurveProperty::Scale, ramp(0.0, 1.0, ease));
            assert!(check_curve(&curve).is_err(), "{ease:?} refused in a curve");
        }
        for json in [r#"{"bounce":{"bounces":2.5}}"#, r#"{"steps":{"count":-1}}"#, r#"{"power":{"mode":"sideways"}}"#, r#"{"wave":{"amplitude":0.1,"speed":2}}"#, r#""wave""#, r#""power""#] {
            assert!(serde_json::from_str::<Ease>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn parametric_eases_go_through_json_with_their_defaults() {
        let read = |json: &str| serde_json::from_str::<Ease>(json).unwrap();
        assert_eq!(read(r#""elastic""#), Ease::ELASTIC);
        assert_eq!(read(r#""bounce""#), Ease::BOUNCE);
        assert_eq!(read(r#"{"elastic":{}}"#), Ease::Elastic { amplitude: 1.0, period: 0.3 });
        assert_eq!(read(r#"{"elastic":{"period":0.5}}"#), Ease::Elastic { amplitude: 1.0, period: 0.5 });
        assert_eq!(read(r#"{"bounce":{"decay":0.3}}"#), Ease::Bounce { bounces: 4, decay: 0.3 });
        assert_eq!(read(r#"{"power":{}}"#), Ease::Power { exponent: 3.0, mode: PowerMode::In });
        assert_eq!(read(r#"{"power":{"exponent":2,"mode":"inOut"}}"#), Ease::Power { exponent: 2.0, mode: PowerMode::InOut });
        assert_eq!(read(r#"{"steps":{}}"#), Ease::Steps { count: 4, position: StepPosition::End });
        assert_eq!(read(r#"{"steps":{"count":3,"position":"both"}}"#), Ease::Steps { count: 3, position: StepPosition::Both });
        assert_eq!(read(r#"{"wave":{}}"#), Ease::Wave { amplitude: 0.1, cycles: 2.0, decay: 0.0 });
        assert_eq!(read(r#""outCubic""#), Ease::OutCubic);
        assert_eq!(read(r#"{"backOut":{"amplitude":2}}"#), Ease::BackOut { amplitude: 2.0 });
        for ease in parametric_eases().into_iter().chain([Ease::Linear, Ease::Hold, Ease::BackIn { amplitude: 1.2 }, Ease::CubicBezier([0.1, 0.2, 0.3, 0.4])]) {
            let json = serde_json::to_string(&ease).unwrap();
            assert_eq!(read(&json), ease, "{json}");
        }
        let written: AnimationCurve = serde_json::from_str(r#"{"tracks":{"scale":[{"t":0,"value":0,"ease":"bounce"},{"t":1,"value":1}]}}"#).unwrap();
        assert_eq!(written.tracks[&CurveProperty::Scale][0].ease, Ease::BOUNCE);
    }

    #[test]
    fn the_max_scale_bounds_overshoot_between_samples() {
        // A wave of many cycles peaks between any fixed samples of the curve.
        for ease in parametric_eases() {
            let preset = PackPreset::new("p/x".to_owned(), AnimationGroup::Enter, curve().with(CurveProperty::Scale, ramp(0.0, 1.0, ease)).with(CurveProperty::ScaleX, ramp(1.0, 1.5, ease)));
            for i in 0..=200_000 {
                let pose = evaluate_curve(&preset.curve, f64::from(i) / 200_000.0);
                let scale = pose.scale * pose.scale_x.max(pose.scale_y);
                assert!(scale <= preset.max_scale(), "{ease:?} at {i}: {scale} above {}", preset.max_scale());
            }
        }
        let elastic = PackPreset::new("p/e".to_owned(), AnimationGroup::Enter, curve().with(CurveProperty::Scale, ramp(0.0, 1.0, Ease::ELASTIC)));
        assert!(elastic.max_scale() > 1.3 && elastic.max_scale() < 1.4, "tight for a smooth ease: {}", elastic.max_scale());
    }

    #[test]
    fn curves_go_through_json() {
        let curve = pop_in().with(CurveProperty::Blur, vec![key(0.0, 0.02, Ease::CubicBezier([0.2, 0.0, 0.0, 1.0])), key(1.0, 0.0, Ease::Linear)]);
        let json = serde_json::to_string(&curve).unwrap();
        assert!(json.contains("\"scale\"") && json.contains("backOut") && json.contains("cubicBezier"), "{json}");
        let back: AnimationCurve = serde_json::from_str(&json).unwrap();
        assert_eq!(back, curve);
        let written: AnimationCurve = serde_json::from_str(r#"{"tracks":{"opacity":[{"t":0,"value":0},{"t":1,"value":1}]}}"#).unwrap();
        assert!((written.value_at(CurveProperty::Opacity, 0.25) - 0.25).abs() < 1e-12, "the ease defaults to linear");
    }

    #[test]
    fn a_blur_track_blurs_and_poses_add_their_blur() {
        let curve = curve().with(CurveProperty::Blur, ramp(0.04, 0.0, Ease::Linear));
        assert!((evaluate_curve(&curve, 0.5).blur - 0.02).abs() < 1e-12);
        assert_eq!(evaluate_curve(&curve, 1.0).blur, 0.0);
        let negative = AnimationCurve::default().with(CurveProperty::Blur, ramp(-1.0, -1.0, Ease::Linear));
        assert_eq!(evaluate_curve(&negative, 0.5).blur, 0.0, "a blur is never below 0");
        let both = evaluate_curve(&curve, 0.0).then(evaluate_curve(&curve, 0.5));
        assert!((both.blur - 0.06).abs() < 1e-12);
    }
}
