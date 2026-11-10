//! Project validation. Every error names its location (track, clip, transition, effect index).

use crate::keyframe::{keyframes_problem, ClipProperty, MAX_ROTATION};
use crate::project::{speed_in_range, Clip, Project, MAX_SPEED, MIN_FRAME_DURATION, MIN_SPEED};
use crate::time::Time;
use std::collections::HashMap;
use std::fmt;

/// Answers the questions validation cannot decide from the project text alone.
/// Tests pass an in-memory implementation, so no real files or packages are needed.
pub trait Resolver {
    /// Whether the source media file exists.
    fn source_exists(&self, source: &str) -> bool;
    /// Why a source that may not exist cannot be used, when more can be said than that it is
    /// missing, such as the pack that must be installed. The default says nothing.
    fn source_problem(&self, _source: &str) -> Option<String> {
        None
    }
    /// Whether an effect or transition package can be found.
    fn package_exists(&self, package: &str) -> bool;
    /// Whether the source has an audio stream. Asked only for existing sources of audio tracks.
    /// The default accepts every source, so resolvers that cannot tell keep working.
    fn source_has_audio(&self, _source: &str) -> bool {
        true
    }
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
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// `output.width` or `output.height` is zero.
    InvalidOutputSize { width: u32, height: u32 },
    /// `output.frame_duration` is zero or negative.
    NonPositiveFrameDuration { frame_duration: Time },
    /// `output.frame_duration` is shorter than [`MIN_FRAME_DURATION`]: more than 60 fps.
    FrameDurationTooShort { frame_duration: Time },
    /// `output.audio.sample_rate` is not 48000.
    UnsupportedSampleRate { sample_rate: u32 },
    /// `output.audio.channels` is not 2.
    UnsupportedChannelCount { channels: u32 },
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
    /// A clip source exists as a name, such as a sound of an installed pack, but cannot be used;
    /// `reason` says why.
    SourceUnavailable {
        track: usize,
        clip: usize,
        source: String,
        reason: String,
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
    /// A clip `placement` is not finite or is out of its limits.
    InvalidPlacement { track: usize, clip: usize },
    /// A clip `opacity` is not a number from 0 to 1.
    InvalidOpacity { track: usize, clip: usize, opacity: f64 },
    /// A clip `rotation` is not a number within [`MAX_ROTATION`] degrees.
    InvalidRotation { track: usize, clip: usize, rotation: f64 },
    /// A list of keyframes is not usable; `what` names the list and `reason` says why.
    InvalidKeyframes {
        track: usize,
        clip: usize,
        what: String,
        reason: &'static str,
    },
    /// A clip on a track of kind `audio` has opacity, rotation or keyframes.
    AudioTrackMotion { track: usize, clip: usize },
    /// A clip `speed` is not a number between [`MIN_SPEED`] and [`MAX_SPEED`].
    InvalidSpeed { track: usize, clip: usize, speed: f64 },
    /// A clip `gain_db` is infinite or not a number.
    NonFiniteGain { track: usize, clip: usize },
    /// A clip `fade_in` or `fade_out` is negative.
    NegativeFade {
        track: usize,
        clip: usize,
        field: &'static str,
        value: Time,
    },
    /// A clip has a `fade_in` while it is the later clip of a transition, or a `fade_out` while it
    /// is the earlier clip. The fade would leave a silent hole inside the transition window.
    FadeBesideTransition {
        track: usize,
        clip: usize,
        field: &'static str,
    },
    /// A clip `gain_db` is above 60.
    GainTooLarge { track: usize, clip: usize, gain_db: f64 },
    /// `output.audio.bitrate` is zero.
    InvalidAudioBitrate,
    /// `fade_in + fade_out` is longer than the clip.
    FadesExceedClip {
        track: usize,
        clip: usize,
        fade_in: Time,
        fade_out: Time,
        duration: Time,
    },
    /// A track of kind `audio` has a transition.
    AudioTrackTransition { track: usize, transition: usize },
    /// A clip on a track of kind `audio` has an effect.
    AudioTrackEffect {
        track: usize,
        clip: usize,
        effect: usize,
    },
    /// A clip on a track of kind `audio` shows a still image.
    ImageOnAudioTrack {
        track: usize,
        clip: usize,
        source: String,
    },
    /// The source of a clip on a track of kind `audio` has no audio stream.
    SourceHasNoAudio {
        track: usize,
        clip: usize,
        source: String,
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
            FrameDurationTooShort { frame_duration } => {
                write!(f, "output: frame_duration {frame_duration} is more than 60 fps, it must be at least {MIN_FRAME_DURATION} ticks")
            }
            UnsupportedSampleRate { sample_rate } => {
                write!(f, "output: audio sample_rate {sample_rate} must be 48000")
            }
            UnsupportedChannelCount { channels } => {
                write!(f, "output: audio channels {channels} must be 2")
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
            SourceUnavailable { track, clip, reason, .. } => {
                write!(f, "track {track}, clip {clip}: {reason}")
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
            InvalidPlacement { track, clip } => {
                write!(f, "track {track}, clip {clip}: placement is out of its limits")
            }
            InvalidOpacity { track, clip, opacity } => {
                write!(f, "track {track}, clip {clip}: opacity {opacity} must be from 0 to 1")
            }
            InvalidRotation { track, clip, rotation } => {
                write!(f, "track {track}, clip {clip}: rotation {rotation} must be within {MAX_ROTATION} degrees")
            }
            InvalidKeyframes { track, clip, what, reason } => {
                write!(f, "track {track}, clip {clip}: keyframes of {what}: {reason}")
            }
            AudioTrackMotion { track, clip } => {
                write!(f, "track {track}, clip {clip}: a clip on an audio track has no picture to animate")
            }
            InvalidSpeed { track, clip, speed } => {
                write!(f, "track {track}, clip {clip}: speed {speed} must be between {MIN_SPEED} and {MAX_SPEED}")
            }
            NonFiniteGain { track, clip } => {
                write!(f, "track {track}, clip {clip}: gain_db must be finite")
            }
            NegativeFade { track, clip, field, value } => {
                write!(f, "track {track}, clip {clip}: {field} {value} must not be negative")
            }
            FadeBesideTransition { track, clip, field } => write!(
                f,
                "track {track}, clip {clip}: {field} cannot be used on the side of a transition"
            ),
            GainTooLarge { track, clip, gain_db } => {
                write!(f, "track {track}, clip {clip}: gain_db {gain_db} must not be above 60")
            }
            InvalidAudioBitrate => write!(f, "output: audio bitrate must be positive"),
            FadesExceedClip { track, clip, fade_in, fade_out, duration } => write!(
                f,
                "track {track}, clip {clip}: fade_in {fade_in} plus fade_out {fade_out} is longer than duration {duration}"
            ),
            AudioTrackTransition { track, transition } => {
                write!(f, "track {track}, transition {transition}: an audio track cannot have transitions")
            }
            AudioTrackEffect { track, clip, effect } => {
                write!(f, "track {track}, clip {clip}, effect {effect}: an audio track cannot have effects")
            }
            ImageOnAudioTrack { track, clip, source } => {
                write!(f, "track {track}, clip {clip}: the image '{source}' cannot go on an audio track")
            }
            SourceHasNoAudio { track, clip, source } => {
                write!(f, "track {track}, clip {clip}: source '{source}' has no audio stream")
            }
            TransitionTooLong { track, transition, clip, needed, usable } => write!(
                f,
                "track {track}, transition {transition}: needs {needed} from clip {clip} but only {usable} is usable"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Adds the problems of the opacity, rotation and keyframes of one clip to `errors`.
fn motion_errors(clip: &Clip, track: usize, c: usize, on_audio_track: bool, errors: &mut Vec<ValidationError>) {
    use ValidationError::*;
    let motion = &clip.motion;
    if !ClipProperty::Opacity.accepts(motion.opacity) {
        errors.push(InvalidOpacity { track, clip: c, opacity: motion.opacity });
    }
    if !ClipProperty::Rotation.accepts(motion.rotation) {
        errors.push(InvalidRotation { track, clip: c, rotation: motion.rotation });
    }
    if on_audio_track && !motion.is_default() {
        errors.push(AudioTrackMotion { track, clip: c });
    }
    for (property, keys) in &motion.keyframes {
        if let Some(reason) = keyframes_problem(keys, |value| property.accepts(value)) {
            errors.push(InvalidKeyframes { track, clip: c, what: property.name().to_owned(), reason });
        }
    }
    for (e, effect) in clip.effects.iter().enumerate() {
        for (name, keys) in &effect.keyframes {
            if let Some(reason) = keyframes_problem(keys, f64::is_finite) {
                errors.push(InvalidKeyframes { track, clip: c, what: format!("effect {e} parameter {name}"), reason });
            }
        }
    }
}

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
    } else if project.output.frame_duration < MIN_FRAME_DURATION {
        errors.push(FrameDurationTooShort {
            frame_duration: project.output.frame_duration,
        });
    }
    if let Some(audio) = &project.output.audio {
        if audio.sample_rate != 48_000 {
            errors.push(UnsupportedSampleRate {
                sample_rate: audio.sample_rate,
            });
        }
        if audio.channels != 2 {
            errors.push(UnsupportedChannelCount {
                channels: audio.channels,
            });
        }
        if audio.bitrate == 0 {
            errors.push(InvalidAudioBitrate);
        }
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
            // A hidden track's file and package are not needed for the output, so one that is missing does not matter.
            if track.hidden {
                // Its structure is still checked below.
            } else if let Some(reason) = resolver.source_problem(&clip.source) {
                errors.push(SourceUnavailable {
                    track: t,
                    clip: c,
                    source: clip.source.clone(),
                    reason,
                });
            } else if !resolver.source_exists(&clip.source) {
                errors.push(SourceNotFound {
                    track: t,
                    clip: c,
                    source: clip.source.clone(),
                });
            } else if track.is_audio() && clip.is_image() {
                errors.push(ImageOnAudioTrack {
                    track: t,
                    clip: c,
                    source: clip.source.clone(),
                });
            } else if track.is_audio() && !resolver.source_has_audio(&clip.source) {
                errors.push(SourceHasNoAudio {
                    track: t,
                    clip: c,
                    source: clip.source.clone(),
                });
            }
            if clip.placement.is_some_and(|placement| !placement.is_valid()) {
                errors.push(InvalidPlacement { track: t, clip: c });
            }
            motion_errors(clip, t, c, track.is_audio(), &mut errors);
            if !speed_in_range(clip.speed) {
                errors.push(InvalidSpeed {
                    track: t,
                    clip: c,
                    speed: clip.speed,
                });
            }
            if !clip.gain_db.is_finite() {
                errors.push(NonFiniteGain { track: t, clip: c });
            } else if clip.gain_db > 60.0 {
                errors.push(GainTooLarge {
                    track: t,
                    clip: c,
                    gain_db: clip.gain_db,
                });
            }
            let mut fades_ok = true;
            for (field, value) in [("fade_in", clip.fade_in), ("fade_out", clip.fade_out)] {
                if value < Time::ZERO {
                    errors.push(NegativeFade {
                        track: t,
                        clip: c,
                        field,
                        value,
                    });
                    fades_ok = false;
                }
            }
            // A duration that is not positive has its own error already.
            if fades_ok
                && clip.duration > Time::ZERO
                && clip
                    .fade_in
                    .checked_add(clip.fade_out)
                    .is_none_or(|total| total > clip.duration)
            {
                errors.push(FadesExceedClip {
                    track: t,
                    clip: c,
                    fade_in: clip.fade_in,
                    fade_out: clip.fade_out,
                    duration: clip.duration,
                });
            }
            for (e, effect) in clip.effects.iter().enumerate() {
                if track.is_audio() {
                    errors.push(AudioTrackEffect {
                        track: t,
                        clip: c,
                        effect: e,
                    });
                }
                if !track.hidden && !resolver.package_exists(&effect.package) {
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
            if track.is_audio() {
                errors.push(AudioTrackTransition {
                    track: t,
                    transition: i,
                });
            }
            let mut ok = true;
            if tr.duration <= Time::ZERO {
                errors.push(NonPositiveTransitionDuration {
                    track: t,
                    transition: i,
                    duration: tr.duration,
                });
                ok = false;
            }
            if !track.hidden && !resolver.package_exists(&tr.package) {
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

        // A fade beside a transition would silence part of its window.
        for tr in &track.transitions {
            let Some(next) = tr.after_clip.checked_add(1) else {
                continue;
            };
            if next >= track.clips.len() {
                continue;
            }
            if track.clips[tr.after_clip].fade_out > Time::ZERO {
                errors.push(FadeBesideTransition {
                    track: t,
                    clip: tr.after_clip,
                    field: "fade_out",
                });
            }
            if track.clips[next].fade_in > Time::ZERO {
                errors.push(FadeBesideTransition {
                    track: t,
                    clip: next,
                    field: "fade_in",
                });
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
