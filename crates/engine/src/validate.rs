//! Project validation. Every error names its location (track, clip, transition, effect index).

use crate::project::Project;
use crate::time::Time;
use std::collections::HashMap;
use std::fmt;

/// Answers the questions validation cannot decide from the project text alone.
/// Tests pass an in-memory implementation, so no real files or packages are needed.
pub trait Resolver {
    /// Whether the source media file exists.
    fn source_exists(&self, source: &str) -> bool;
    /// Whether an effect or transition package can be found.
    fn package_exists(&self, package: &str) -> bool;
}

/// A resolver that accepts everything, for checking structure only.
pub struct AcceptAll;

impl Resolver for AcceptAll {
    fn source_exists(&self, _source: &str) -> bool {
        true
    }
    fn package_exists(&self, _package: &str) -> bool {
        true
    }
}

/// One validation problem. Indices are zero-based positions in the project JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// `output.width` or `output.height` is zero.
    InvalidOutputSize { width: u32, height: u32 },
    /// `output.frame_duration` is zero or negative.
    NonPositiveFrameDuration { frame_duration: Time },
    /// A clip duration is zero or negative.
    NonPositiveClipDuration {
        track: usize,
        clip: usize,
        duration: Time,
    },
    /// A clip `start` or `source_in` is negative.
    NegativeClipTime {
        track: usize,
        clip: usize,
        field: &'static str,
        value: Time,
    },
    /// `start + duration` overflows.
    ClipTimeOverflow { track: usize, clip: usize },
    /// Two clips of one track overlap in time.
    ClipOverlap {
        track: usize,
        clip: usize,
        other_clip: usize,
    },
    /// A clip source file was not found.
    SourceNotFound {
        track: usize,
        clip: usize,
        source: String,
    },
    /// An effect package of a clip was not found.
    EffectPackageNotFound {
        track: usize,
        clip: usize,
        effect: usize,
        package: String,
    },
    /// A transition duration is zero or negative.
    NonPositiveTransitionDuration {
        track: usize,
        transition: usize,
        duration: Time,
    },
    /// A transition package was not found.
    TransitionPackageNotFound {
        track: usize,
        transition: usize,
        package: String,
    },
    /// `after_clip + 1` is not a clip of the track.
    TransitionClipMissing {
        track: usize,
        transition: usize,
        after_clip: usize,
    },
    /// Two transitions use the same `after_clip`.
    DuplicateTransition {
        track: usize,
        transition: usize,
        after_clip: usize,
    },
    /// The clips around the transition do not touch (clip end != next clip start).
    TransitionNotAdjacent {
        track: usize,
        transition: usize,
        after_clip: usize,
    },
    /// One side of the transition window is longer than the part of its clip that is
    /// not already used by the neighbouring transition of the same clip.
    TransitionTooLong {
        track: usize,
        transition: usize,
        /// Index of the clip whose usable length is exceeded.
        clip: usize,
        /// Ticks the window needs from that clip.
        needed: Time,
        /// Ticks of that clip left for this transition.
        usable: Time,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ValidationError::*;
        match self {
            InvalidOutputSize { width, height } => {
                write!(f, "output: size {width}x{height} must be positive")
            }
            NonPositiveFrameDuration { frame_duration } => {
                write!(f, "output: frame_duration {frame_duration} must be positive")
            }
            NonPositiveClipDuration { track, clip, duration } => {
                write!(f, "track {track}, clip {clip}: duration {duration} must be positive")
            }
            NegativeClipTime { track, clip, field, value } => {
                write!(f, "track {track}, clip {clip}: {field} {value} must not be negative")
            }
            ClipTimeOverflow { track, clip } => {
                write!(f, "track {track}, clip {clip}: start + duration overflows")
            }
            ClipOverlap { track, clip, other_clip } => {
                write!(f, "track {track}: clip {clip} overlaps clip {other_clip}")
            }
            SourceNotFound { track, clip, source } => {
                write!(f, "track {track}, clip {clip}: source '{source}' not found")
            }
            EffectPackageNotFound { track, clip, effect, package } => write!(
                f,
                "track {track}, clip {clip}, effect {effect}: package '{package}' not found"
            ),
            NonPositiveTransitionDuration { track, transition, duration } => write!(
                f,
                "track {track}, transition {transition}: duration {duration} must be positive"
            ),
            TransitionPackageNotFound { track, transition, package } => write!(
                f,
                "track {track}, transition {transition}: package '{package}' not found"
            ),
            TransitionClipMissing { track, transition, after_clip } => write!(
                f,
                "track {track}, transition {transition}: no clip after clip {after_clip}"
            ),
            DuplicateTransition { track, transition, after_clip } => write!(
                f,
                "track {track}, transition {transition}: after_clip {after_clip} already has a transition"
            ),
            TransitionNotAdjacent { track, transition, after_clip } => write!(
                f,
                "track {track}, transition {transition}: clips {after_clip} and {} are not adjacent",
                after_clip + 1
            ),
            TransitionTooLong { track, transition, clip, needed, usable } => write!(
                f,
                "track {track}, transition {transition}: needs {needed} from clip {clip} but only {usable} is usable"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Checks `project` and returns every problem found (empty when valid).
///
/// Transition geometry is defined in [`crate::schedule`]: the window is centred on the cut,
/// `duration / 2` (rounded down) ticks on the first clip and the rest on the second.
pub fn validate(project: &Project, resolver: &dyn Resolver) -> Vec<ValidationError> {
    use ValidationError::*;
    let mut errors = Vec::new();

    if project.output.width == 0 || project.output.height == 0 {
        errors.push(InvalidOutputSize {
            width: project.output.width,
            height: project.output.height,
        });
    }
    if project.output.frame_duration <= Time::ZERO {
        errors.push(NonPositiveFrameDuration {
            frame_duration: project.output.frame_duration,
        });
    }

    for (t, track) in project.tracks.iter().enumerate() {
        // Clips whose own fields are sound enough to take part in overlap/transition checks.
        let mut sound: Vec<(usize, Time, Time)> = Vec::new();
        for (c, clip) in track.clips.iter().enumerate() {
            let mut ok = true;
            if clip.duration <= Time::ZERO {
                errors.push(NonPositiveClipDuration {
                    track: t,
                    clip: c,
                    duration: clip.duration,
                });
                ok = false;
            }
            if clip.start < Time::ZERO {
                errors.push(NegativeClipTime {
                    track: t,
                    clip: c,
                    field: "start",
                    value: clip.start,
                });
                ok = false;
            }
            if clip.source_in < Time::ZERO {
                errors.push(NegativeClipTime {
                    track: t,
                    clip: c,
                    field: "source_in",
                    value: clip.source_in,
                });
                ok = false;
            }
            let end = clip.end();
            if end.is_none() {
                errors.push(ClipTimeOverflow { track: t, clip: c });
                ok = false;
            }
            if !resolver.source_exists(&clip.source) {
                errors.push(SourceNotFound {
                    track: t,
                    clip: c,
                    source: clip.source.clone(),
                });
            }
            for (e, effect) in clip.effects.iter().enumerate() {
                if !resolver.package_exists(&effect.package) {
                    errors.push(EffectPackageNotFound {
                        track: t,
                        clip: c,
                        effect: e,
                        package: effect.package.clone(),
                    });
                }
            }
            if let (true, Some(end)) = (ok, end) {
                sound.push((c, clip.start, end));
            }
        }

        // Overlap: after sorting by start, any overlap shows up between neighbours.
        sound.sort_by_key(|&(c, start, _)| (start, c));
        for pair in sound.windows(2) {
            let (a, _, a_end) = pair[0];
            let (b, b_start, _) = pair[1];
            if a_end > b_start {
                errors.push(ClipOverlap {
                    track: t,
                    clip: a,
                    other_clip: b,
                });
            }
        }

        // Transitions. `head_used[k]`/`tail_used[k]` are the ticks other transitions already take
        // from the start and end of clip k.
        let mut seen: HashMap<usize, usize> = HashMap::new();
        let mut valid: Vec<(usize, usize, Time)> = Vec::new(); // (transition, after_clip, duration)
        for (i, tr) in track.transitions.iter().enumerate() {
            let mut ok = true;
            if tr.duration <= Time::ZERO {
                errors.push(NonPositiveTransitionDuration {
                    track: t,
                    transition: i,
                    duration: tr.duration,
                });
                ok = false;
            }
            if !resolver.package_exists(&tr.package) {
                errors.push(TransitionPackageNotFound {
                    track: t,
                    transition: i,
                    package: tr.package.clone(),
                });
            }
            if tr
                .after_clip
                .checked_add(1)
                .is_none_or(|n| n >= track.clips.len())
            {
                errors.push(TransitionClipMissing {
                    track: t,
                    transition: i,
                    after_clip: tr.after_clip,
                });
                continue;
            }
            if seen.insert(tr.after_clip, i).is_some() {
                errors.push(DuplicateTransition {
                    track: t,
                    transition: i,
                    after_clip: tr.after_clip,
                });
                continue;
            }
            let a = &track.clips[tr.after_clip];
            let b = &track.clips[tr.after_clip + 1];
            if a.end() != Some(b.start) {
                errors.push(TransitionNotAdjacent {
                    track: t,
                    transition: i,
                    after_clip: tr.after_clip,
                });
                continue;
            }
            if ok {
                valid.push((i, tr.after_clip, tr.duration));
            }
        }

        let before_of = |d: Time| d.half_floor();
        let after_of = |d: Time| Time::from_ticks(d.ticks() - d.half_floor().ticks());
        let duration_by_after: HashMap<usize, Time> =
            valid.iter().map(|&(_, after, d)| (after, d)).collect();
        for &(i, after, d) in &valid {
            let a = &track.clips[after];
            let b = &track.clips[after + 1];
            // Clip `after` also gives its head to the transition after `after - 1`.
            let head_used = after
                .checked_sub(1)
                .and_then(|p| duration_by_after.get(&p))
                .map_or(Time::ZERO, |&pd| after_of(pd));
            // Clip `after + 1` also gives its tail to the transition after `after + 1`.
            let tail_used = duration_by_after
                .get(&(after + 1))
                .map_or(Time::ZERO, |&nd| before_of(nd));
            let usable_a = a.duration.saturating_sub(head_used);
            let usable_b = b.duration.saturating_sub(tail_used);
            if before_of(d) > usable_a {
                errors.push(TransitionTooLong {
                    track: t,
                    transition: i,
                    clip: after,
                    needed: before_of(d),
                    usable: usable_a,
                });
            }
            if after_of(d) > usable_b {
                errors.push(TransitionTooLong {
                    track: t,
                    transition: i,
                    clip: after + 1,
                    needed: after_of(d),
                    usable: usable_b,
                });
            }
        }
    }
    errors
}
