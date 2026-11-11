//! Preset animations of text clips. A preset only moves, scales, turns and fades the picture the
//! text was drawn into, so it never draws the text again and never changes the decoder key.
//!
//! Timing (see [`animation_pose`]), over the time the clip is visible (its [`ClipClock`]):
//! - the enter preset runs from the first visible frame for its duration;
//! - the exit preset runs for its duration up to the last visible moment;
//! - the repeat preset runs the whole time, one cycle per its duration.
//!
//! When enter and exit together are longer than the visible time, both are shortened by the same
//! factor so they fill it exactly, the enter in the first part and the exit in the rest.
//! A preset id that is not known, or a duration that is not above zero, is left out.

use std::f64::consts::PI;

use crate::project::{TextAnimation, TextAnimationRef};
use crate::schedule::ClipClock;

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

/// One preset a text clip can name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationPreset {
    pub id: &'static str,
    pub group: AnimationGroup,
    /// Whether the preset turns the picture.
    pub rotates: bool,
    /// The largest scale the preset ever gives.
    pub max_scale: f64,
}

/// How a preset changes the picture at one moment, on top of the clip's own motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationPose {
    /// Shift, in parts of the output frame width and height.
    pub dx: f64,
    pub dy: f64,
    /// Scale around the middle of the picture.
    pub scale: f64,
    /// Added turn, in degrees.
    pub rotation: f64,
    /// Multiplies the opacity.
    pub opacity: f64,
}

impl AnimationPose {
    pub const IDENTITY: AnimationPose = AnimationPose { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0, opacity: 1.0 };

    /// Both poses at once.
    pub fn then(self, other: AnimationPose) -> AnimationPose {
        AnimationPose { dx: self.dx + other.dx, dy: self.dy + other.dy, scale: self.scale * other.scale, rotation: self.rotation + other.rotation, opacity: self.opacity * other.opacity }
    }
}

const fn preset(id: &'static str, group: AnimationGroup, rotates: bool, max_scale: f64) -> AnimationPreset {
    AnimationPreset { id, group, rotates, max_scale }
}

/// A bound of the highest point of [`ease_out_back`], a little above 1.
const POP_PEAK: f64 = 1.11;
/// How far a slide travels, in parts of the frame.
const SLIDE_DISTANCE: f64 = 0.2;
/// How high a bounce goes, in parts of the frame height.
const BOUNCE_HEIGHT: f64 = 0.03;
/// The largest scale of a pulse.
const PULSE_PEAK: f64 = 1.08;
/// How far a shake moves to each side, in parts of the frame width.
const SHAKE_DISTANCE: f64 = 0.01;
/// How many times a shake goes back and forth in one cycle.
const SHAKE_TIMES: f64 = 3.0;

/// Every preset, by group. Enter and exit presets share their ids and mirror each other.
pub const TEXT_ANIMATION_PRESETS: &[AnimationPreset] = &[
    preset("fade", AnimationGroup::Enter, false, 1.0),
    preset("slide-left", AnimationGroup::Enter, false, 1.0),
    preset("slide-right", AnimationGroup::Enter, false, 1.0),
    preset("slide-up", AnimationGroup::Enter, false, 1.0),
    preset("slide-down", AnimationGroup::Enter, false, 1.0),
    preset("zoom", AnimationGroup::Enter, false, 1.0),
    preset("pop", AnimationGroup::Enter, false, POP_PEAK),
    preset("spin", AnimationGroup::Enter, true, 1.0),
    preset("fade", AnimationGroup::Exit, false, 1.0),
    preset("slide-left", AnimationGroup::Exit, false, 1.0),
    preset("slide-right", AnimationGroup::Exit, false, 1.0),
    preset("slide-up", AnimationGroup::Exit, false, 1.0),
    preset("slide-down", AnimationGroup::Exit, false, 1.0),
    preset("zoom", AnimationGroup::Exit, false, 1.0),
    preset("pop", AnimationGroup::Exit, false, POP_PEAK),
    preset("spin", AnimationGroup::Exit, true, 1.0),
    preset("bounce", AnimationGroup::Repeat, false, 1.0),
    preset("pulse", AnimationGroup::Repeat, false, PULSE_PEAK),
    preset("shake", AnimationGroup::Repeat, false, 1.0),
];

/// The preset `id` of `group`, `None` when there is none.
pub fn find_preset(group: AnimationGroup, id: &str) -> Option<&'static AnimationPreset> {
    TEXT_ANIMATION_PRESETS.iter().find(|preset| preset.group == group && preset.id == id)
}

fn ease_out_cubic(p: f64) -> f64 {
    1.0 - (1.0 - p).powi(3)
}

/// Goes past 1 a little before settling, peaking below [`POP_PEAK`].
fn ease_out_back(p: f64) -> f64 {
    const C1: f64 = 1.70158;
    const C3: f64 = C1 + 1.0;
    1.0 + C3 * (p - 1.0).powi(3) + C1 * (p - 1.0).powi(2)
}

/// The pose of an enter preset, `progress` 0 at the start and 1 once settled.
fn enter_pose(id: &str, progress: f64) -> Option<AnimationPose> {
    let p = progress.clamp(0.0, 1.0);
    let eased = ease_out_cubic(p);
    let away = (1.0 - eased) * SLIDE_DISTANCE;
    let pose = |dx, dy, scale, rotation, opacity| AnimationPose { dx, dy, scale, rotation, opacity };
    Some(match id {
        "fade" => pose(0.0, 0.0, 1.0, 0.0, p),
        // A slide is named by the way it moves: `slide-left` comes in from the right.
        "slide-left" => pose(away, 0.0, 1.0, 0.0, eased),
        "slide-right" => pose(-away, 0.0, 1.0, 0.0, eased),
        "slide-up" => pose(0.0, away, 1.0, 0.0, eased),
        "slide-down" => pose(0.0, -away, 1.0, 0.0, eased),
        "zoom" => pose(0.0, 0.0, eased, 0.0, eased),
        "pop" => pose(0.0, 0.0, ease_out_back(p).max(0.0), 0.0, (p * 3.0).min(1.0)),
        "spin" => pose(0.0, 0.0, eased, -180.0 * (1.0 - eased), eased),
        _ => return None,
    })
}

/// The pose of an exit preset, `progress` 0 while still settled and 1 once gone: the enter preset
/// played backwards, moving on the way it is named.
fn exit_pose(id: &str, progress: f64) -> Option<AnimationPose> {
    let mut pose = enter_pose(id, 1.0 - progress.clamp(0.0, 1.0))?;
    if id.starts_with("slide-") || id == "spin" {
        pose.dx = -pose.dx;
        pose.dy = -pose.dy;
        pose.rotation = -pose.rotation;
    }
    Some(pose)
}

/// The pose of a repeat preset, `phase` in cycles (only its fraction counts).
fn repeat_pose(id: &str, phase: f64) -> Option<AnimationPose> {
    let p = phase.rem_euclid(1.0);
    let mut pose = AnimationPose::IDENTITY;
    match id {
        "bounce" => pose.dy = -BOUNCE_HEIGHT * (PI * p).sin().abs(),
        "pulse" => pose.scale = 1.0 + (PULSE_PEAK - 1.0) * (PI * p).sin().powi(2),
        "shake" => pose.dx = SHAKE_DISTANCE * (2.0 * PI * SHAKE_TIMES * p).sin(),
        _ => return None,
    }
    Some(pose)
}

/// The pose of preset `id` of `group` at `progress` (for a repeat preset, the phase in cycles),
/// `None` when the preset is not known.
pub fn preset_pose(group: AnimationGroup, id: &str, progress: f64) -> Option<AnimationPose> {
    match group {
        AnimationGroup::Enter => enter_pose(id, progress),
        AnimationGroup::Exit => exit_pose(id, progress),
        AnimationGroup::Repeat => repeat_pose(id, progress),
    }
}

/// The preset and its ticks when it will run, `None` when it is unknown or does not last.
fn usable(group: AnimationGroup, reference: &Option<TextAnimationRef>) -> Option<(&'static AnimationPreset, f64)> {
    let reference = reference.as_ref()?;
    let ticks = reference.duration.ticks();
    if ticks <= 0 {
        return None;
    }
    find_preset(group, &reference.preset).map(|preset| (preset, ticks as f64))
}

/// The pose of `animation` at `clock` (see the module timing rules).
pub fn animation_pose(animation: &TextAnimation, clock: ClipClock) -> AnimationPose {
    let elapsed = clock.elapsed.ticks() as f64;
    let total = clock.duration.ticks().max(0) as f64;
    let enter = usable(AnimationGroup::Enter, &animation.enter);
    let exit = usable(AnimationGroup::Exit, &animation.exit);
    let both = enter.map_or(0.0, |(_, d)| d) + exit.map_or(0.0, |(_, d)| d);
    let shrink = if both > total { total / both } else { 1.0 };
    let mut pose = AnimationPose::IDENTITY;
    let mut add = |group, preset: &AnimationPreset, progress| {
        if let Some(part) = preset_pose(group, preset.id, progress) {
            pose = pose.then(part);
        }
    };
    if let Some((preset, duration)) = enter {
        let duration = duration * shrink;
        if duration > 0.0 && elapsed < duration {
            add(AnimationGroup::Enter, preset, elapsed / duration);
        } else if duration <= 0.0 {
            // Nothing visible to play over: keep the first pose rather than a settled one.
            add(AnimationGroup::Enter, preset, 0.0);
        }
    }
    if let Some((preset, duration)) = exit {
        let duration = duration * shrink;
        let from = total - duration;
        if duration > 0.0 && elapsed > from {
            add(AnimationGroup::Exit, preset, (elapsed - from) / duration);
        }
    }
    if let Some((preset, period)) = usable(AnimationGroup::Repeat, &animation.repeat) {
        add(AnimationGroup::Repeat, preset, elapsed / period);
    }
    pose
}

fn presets_of(animation: &TextAnimation) -> impl Iterator<Item = &'static AnimationPreset> + '_ {
    [(AnimationGroup::Enter, &animation.enter), (AnimationGroup::Exit, &animation.exit), (AnimationGroup::Repeat, &animation.repeat)]
        .into_iter()
        .filter_map(|(group, reference)| usable(group, reference).map(|(preset, _)| preset))
}

/// Whether any preset of `animation` turns the picture.
pub fn animation_rotates(animation: &TextAnimation) -> bool {
    presets_of(animation).any(|preset| preset.rotates)
}

/// The largest scale `animation` ever gives: the larger of enter and exit, times the repeat.
pub fn animation_max_scale(animation: &TextAnimation) -> f64 {
    let mut once = 1.0f64;
    let mut repeat = 1.0f64;
    for preset in presets_of(animation) {
        match preset.group {
            AnimationGroup::Repeat => repeat = repeat.max(preset.max_scale),
            _ => once = once.max(preset.max_scale),
        }
    }
    once * repeat
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_preset_scales_past_its_max_or_turns_when_it_says_not() {
        for preset in TEXT_ANIMATION_PRESETS {
            for step in 0..=1000 {
                let pose = preset_pose(preset.group, preset.id, f64::from(step) / 1000.0).unwrap();
                assert!(pose.scale <= preset.max_scale + 1e-9, "{} {:?} at {step}: {}", preset.id, preset.group, pose.scale);
                assert!(preset.rotates || pose.rotation == 0.0, "{} turns", preset.id);
                assert!((0.0..=1.0).contains(&pose.opacity));
            }
        }
    }

    #[test]
    fn enter_ends_settled_and_exit_starts_settled() {
        for preset in TEXT_ANIMATION_PRESETS.iter().filter(|p| p.group != AnimationGroup::Repeat) {
            let at = if preset.group == AnimationGroup::Enter { 1.0 } else { 0.0 };
            let settled = preset_pose(preset.group, preset.id, at).unwrap();
            let still = AnimationPose::IDENTITY;
            assert!((settled.scale - still.scale).abs() < 1e-9 && settled.dx.abs() < 1e-9 && settled.dy.abs() < 1e-9 && settled.rotation.abs() < 1e-9 && (settled.opacity - 1.0).abs() < 1e-9, "{}", preset.id);
        }
    }
}
