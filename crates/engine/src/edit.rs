//! Timeline edit commands: pure functions from a project to a new project or a typed error.
//!
//! No command touches its input. Each result has passed [`crate::validate`] with
//! [`crate::AcceptAll`], and a command that cannot produce a valid project returns an error.
//!
//! # Rules
//!
//! * **Clip order.** A command that changes a track leaves its clips sorted by `start`, with
//!   [`Transition::after_clip`] renumbered. A [`ClipKey`] names a clip of the project passed in;
//!   [`clip_at`] finds a clip again in the result.
//! * **Moving.** A clip never overlaps another and never pushes one: it lands at the valid start
//!   nearest the one asked for, the earlier one on a tie, see [`nearest_free_start`]. A clip that
//!   really moves loses the transitions touching it, because they require touching clips.
//! * **Trimming.** An edge holding a transition cannot be trimmed, because the cut would leave the
//!   neighbouring clip. Remove the transition first.
//! * **Splitting.** The two halves play exactly what the clip played, frame for frame and sample
//!   for sample. Audio reads source samples relative to rounded positions, so a cut at an arbitrary
//!   tick can shift the second half by one sample; the cut moves to the nearest tick where it does
//!   not (at most 14 ticks away, under 0.2 ms; the picture does not depend on the cut). A fade
//!   cannot be divided between two clips, so a cut inside a fade is refused, as is a cut inside a
//!   transition window.
//! * **Speed.** A clip at another speed keeps every source position exactly across a split or a
//!   start trim: both land on a multiple of [`Clip::speed_grid`] (at most 11 ms away, and never
//!   before the requested position on a trim). [`set_speed`] keeps the source range constant and
//!   refuses what cannot follow it. Sound positions at a cut can differ by a sample at another
//!   speed; the time stretcher starts a new grain chain there.
//! * **Locked tracks.** Every command that changes the clips, effects or transitions of a locked
//!   track is refused with [`EditError::TrackLocked`], so that every caller gets the same rule.
//!   A move is refused when either its source track or its target track is locked. Locking and
//!   hiding themselves stay possible, as does adding a track.

use crate::audio_schedule::sample_position;
use crate::project::{speed_in_range, speed_milli, Clip, Effect, Project, Track, TrackKind, Transition, SPEED_SCALE};
use crate::render::ClipKey;
use crate::time::Time;
use crate::validate::{validate, AcceptAll, ValidationError};
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::fmt;

/// Why an edit command was refused. The input project is unchanged in every case.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// No track has this index.
    TrackNotFound { track: usize },
    /// The track has no clip with this index.
    ClipNotFound { key: ClipKey },
    /// The clip has no effect with this index.
    EffectNotFound { key: ClipKey, effect: usize },
    /// The track has no transition after this clip.
    TransitionNotFound { track: usize, after_clip: usize },
    /// A clip can only move between tracks of the same kind.
    TrackKindMismatch { from: TrackKind, to: TrackKind },
    /// The target track has no valid position for the clip.
    NoRoom { track: usize },
    /// The clip would overlap another clip of the track.
    Overlap { track: usize, start: Time, end: Time },
    /// Time arithmetic overflowed.
    TimeOverflow { key: ClipKey },
    /// The edge to trim holds a transition; `edge` is `start` or `end`.
    TransitionOnEdge { key: ClipKey, edge: &'static str },
    /// Extending the start would read before the beginning of the source.
    BeforeSourceStart { key: ClipKey, source_in: Time },
    /// The cut is not strictly inside the clip, or no cut near it keeps the audio aligned.
    SplitOutsideClip { key: ClipKey, at: Time },
    /// The cut falls inside a fade of the clip.
    SplitInsideFade { key: ClipKey, at: Time },
    /// The cut falls inside the window of a transition of the clip.
    SplitInsideTransition { key: ClipKey, at: Time },
    /// The track is locked: nothing on it can be changed.
    TrackLocked { track: usize },
    /// At this speed the clip would need to last past the start of the next clip.
    SpeedOverlap { key: ClipKey, end: Time, next_start: Time },
    /// The result would not pass validation.
    Invalid(Vec<ValidationError>),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EditError::*;
        match self {
            TrackNotFound { track } => write!(f, "track {track} does not exist"),
            ClipNotFound { key } => write!(f, "track {}: clip {} does not exist", key.track, key.clip),
            EffectNotFound { key, effect } => {
                write!(f, "track {}, clip {}: effect {effect} does not exist", key.track, key.clip)
            }
            TransitionNotFound { track, after_clip } => {
                write!(f, "track {track}: no transition after clip {after_clip}")
            }
            TrackKindMismatch { from, to } => {
                write!(f, "a clip cannot move from a {from:?} track to a {to:?} track")
            }
            NoRoom { track } => write!(f, "track {track}: no room for the clip"),
            Overlap { track, start, end } => {
                write!(f, "track {track}: {start} to {end} overlaps another clip")
            }
            TimeOverflow { key } => {
                write!(f, "track {}, clip {}: time overflow", key.track, key.clip)
            }
            TransitionOnEdge { key, edge } => write!(
                f,
                "track {}, clip {}: the {edge} edge holds a transition",
                key.track, key.clip
            ),
            BeforeSourceStart { key, source_in } => write!(
                f,
                "track {}, clip {}: source_in {source_in} would be negative",
                key.track, key.clip
            ),
            SplitOutsideClip { key, at } => {
                write!(f, "track {}, clip {}: cannot split at {at}", key.track, key.clip)
            }
            SplitInsideFade { key, at } => {
                write!(f, "track {}, clip {}: {at} is inside a fade", key.track, key.clip)
            }
            SplitInsideTransition { key, at } => write!(
                f,
                "track {}, clip {}: {at} is inside a transition window",
                key.track, key.clip
            ),
            TrackLocked { track } => write!(f, "track {track} is locked"),
            SpeedOverlap { key, end, next_start } => write!(
                f,
                "track {}, clip {}: at this speed the clip ends at {end}, past the next clip at {next_start}",
                key.track, key.clip
            ),
            Invalid(errors) => {
                write!(f, "the result is not valid:")?;
                for error in errors {
                    write!(f, " {error};")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for EditError {}

fn finish(project: Project) -> Result<Project, EditError> {
    let errors = validate(&project, &AcceptAll);
    if errors.is_empty() {
        Ok(project)
    } else {
        Err(EditError::Invalid(errors))
    }
}

fn track_of(project: &Project, track: usize) -> Result<&Track, EditError> {
    project.tracks.get(track).ok_or(EditError::TrackNotFound { track })
}

fn track_mut(project: &mut Project, track: usize) -> Result<&mut Track, EditError> {
    project.tracks.get_mut(track).ok_or(EditError::TrackNotFound { track })
}

/// Refuses a command that would change `track` while it is locked.
fn unlocked(project: &Project, track: usize) -> Result<(), EditError> {
    if track_of(project, track)?.locked {
        Err(EditError::TrackLocked { track })
    } else {
        Ok(())
    }
}

fn clip_of(project: &Project, key: ClipKey) -> Result<&Clip, EditError> {
    track_of(project, key.track)?
        .clips
        .get(key.clip)
        .ok_or(EditError::ClipNotFound { key })
}

fn clip_mut(project: &mut Project, key: ClipKey) -> Result<&mut Clip, EditError> {
    track_mut(project, key.track)?
        .clips
        .get_mut(key.clip)
        .ok_or(EditError::ClipNotFound { key })
}

/// Sorts the clips by start and renumbers the transitions. Clips joined by a transition touch, so
/// they stay neighbours and `after_clip + 1` still names the later one.
fn sort_track(track: &mut Track) {
    let mut order: Vec<usize> = (0..track.clips.len()).collect();
    order.sort_by_key(|&i| (track.clips[i].start, i));
    let mut new_index = vec![0; order.len()];
    for (new, &old) in order.iter().enumerate() {
        new_index[old] = new;
    }
    let mut old_clips: Vec<Option<Clip>> = std::mem::take(&mut track.clips).into_iter().map(Some).collect();
    track.clips = order.iter().filter_map(|&old| old_clips[old].take()).collect();
    for transition in &mut track.transitions {
        if let Some(&new) = new_index.get(transition.after_clip) {
            transition.after_clip = new;
        }
    }
    track.transitions.sort_by_key(|transition| transition.after_clip);
}

/// Removes clip `index`, the transitions touching it, and renumbers the rest.
fn remove_clip_at(track: &mut Track, index: usize) -> Clip {
    track
        .transitions
        .retain(|tr| tr.after_clip != index && tr.after_clip.checked_add(1) != Some(index));
    for transition in &mut track.transitions {
        if transition.after_clip > index {
            transition.after_clip -= 1;
        }
    }
    track.clips.remove(index)
}

fn overlaps_other(track: &Track, ignore: Option<usize>, start: Time, end: Time) -> bool {
    track.clips.iter().enumerate().any(|(i, other)| {
        Some(i) != ignore && other.start < end && other.start.saturating_add(other.duration) > start
    })
}

/// The valid start for a clip of `duration` nearest `desired` on `track`, not counting clip
/// `ignore`. The earlier position wins a tie. `None` when the duration is not positive or no
/// position fits.
pub fn nearest_free_start(
    track: &Track,
    ignore: Option<usize>,
    duration: Time,
    desired: Time,
) -> Option<Time> {
    if duration <= Time::ZERO {
        return None;
    }
    let mut others: Vec<(Time, Time)> = track
        .clips
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != ignore)
        .map(|(_, clip)| (clip.start, clip.start.saturating_add(clip.duration)))
        .collect();
    others.sort();

    let mut best: Option<(i128, Time)> = None;
    let mut consider = |gap_start: Time, gap_end: Time| {
        let Some(last) = gap_end.checked_sub(duration) else {
            return;
        };
        if last < gap_start {
            return;
        }
        let candidate = desired.clamp(gap_start, last);
        let distance = (i128::from(candidate.ticks()) - i128::from(desired.ticks())).abs();
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    };
    let mut cursor = Time::ZERO;
    for (start, end) in others {
        consider(cursor, start);
        cursor = cursor.max(end);
    }
    consider(cursor, Time::MAX);
    best.map(|(_, start)| start)
}

/// The clip of `track` that starts exactly at `start`.
pub fn clip_at(project: &Project, track: usize, start: Time) -> Option<ClipKey> {
    let clip = project.tracks.get(track)?.clips.iter().position(|c| c.start == start)?;
    Some(ClipKey { track, clip })
}

/// Appends an empty track above the others.
pub fn add_track(project: &Project, kind: TrackKind) -> Result<Project, EditError> {
    let mut next = project.clone();
    next.tracks.push(Track {
        kind,
        clips: Vec::new(),
        transitions: Vec::new(),
        locked: false,
        hidden: false,
    });
    finish(next)
}

/// Sets whether every edit of `track` is refused.
pub fn set_track_locked(project: &Project, track: usize, locked: bool) -> Result<Project, EditError> {
    let mut next = project.clone();
    track_mut(&mut next, track)?.locked = locked;
    finish(next)
}

/// Sets whether `track` is left out of the picture and the sound.
pub fn set_track_hidden(project: &Project, track: usize, hidden: bool) -> Result<Project, EditError> {
    let mut next = project.clone();
    track_mut(&mut next, track)?.hidden = hidden;
    finish(next)
}

/// Adds a clip to `track` at its own `start`. Refuses a position taken by another clip; use
/// [`nearest_free_start`] to pick a free one.
pub fn add_clip(project: &Project, track: usize, clip: Clip) -> Result<Project, EditError> {
    unlocked(project, track)?;
    let mut next = project.clone();
    let target = track_mut(&mut next, track)?;
    if let Some(end) = clip.end() {
        if overlaps_other(target, None, clip.start, end) {
            return Err(EditError::Overlap {
                track,
                start: clip.start,
                end,
            });
        }
    }
    target.clips.push(clip);
    sort_track(target);
    finish(next)
}

/// Moves a clip to `target_track`, as close to `desired_start` as the other clips allow. A move
/// that ends where the clip already is returns an equal project and keeps its transitions.
pub fn move_clip(
    project: &Project,
    key: ClipKey,
    target_track: usize,
    desired_start: Time,
) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    unlocked(project, target_track)?;
    let clip = clip_of(project, key)?;
    let from = project.tracks[key.track].kind;
    let target = track_of(project, target_track)?;
    if from != target.kind {
        return Err(EditError::TrackKindMismatch { from, to: target.kind });
    }
    let same_track = target_track == key.track;
    let ignore = same_track.then_some(key.clip);
    let start = nearest_free_start(target, ignore, clip.duration, desired_start)
        .ok_or(EditError::NoRoom { track: target_track })?;
    if same_track && start == clip.start {
        return finish(project.clone());
    }
    let mut next = project.clone();
    let mut moved = remove_clip_at(&mut next.tracks[key.track], key.clip);
    moved.start = start;
    let destination = &mut next.tracks[target_track];
    destination.clips.push(moved);
    sort_track(destination);
    sort_track(&mut next.tracks[key.track]);
    finish(next)
}

fn has_transition_after(track: &Track, clip: usize) -> bool {
    track.transitions.iter().any(|tr| tr.after_clip == clip)
}

fn has_transition_before(track: &Track, clip: usize) -> bool {
    clip.checked_sub(1).is_some_and(|previous| has_transition_after(track, previous))
}

/// Moves the start edge of a clip to `new_start`, keeping the source frames under the end edge.
pub fn trim_start(project: &Project, key: ClipKey, new_start: Time) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let clip = clip_of(project, key)?;
    if new_start == clip.start {
        return finish(project.clone());
    }
    let track = &project.tracks[key.track];
    if has_transition_before(track, key.clip) {
        return Err(EditError::TransitionOnEdge { key, edge: "start" });
    }
    let overflow = EditError::TimeOverflow { key };
    let end = clip.end().ok_or(overflow.clone())?;
    // The edge lands on the grid of the speed, toward the end of the clip, so the frames under the
    // end edge stay exactly where they are.
    let delta = new_start.checked_sub(clip.start).ok_or(overflow.clone())?;
    let delta = Time::from_ticks(delta.ticks().div_euclid(clip.speed_grid()).saturating_add(i64::from(delta.ticks().rem_euclid(clip.speed_grid()) != 0)).saturating_mul(clip.speed_grid()));
    let new_start = clip.start.checked_add(delta).ok_or(overflow.clone())?;
    if new_start == clip.start {
        return finish(project.clone());
    }
    let source_in = clip.source_in.checked_add(clip.source_offset(delta)).ok_or(overflow.clone())?;
    if source_in < Time::ZERO {
        return Err(EditError::BeforeSourceStart { key, source_in });
    }
    if overlaps_other(track, Some(key.clip), new_start, end) {
        return Err(EditError::Overlap {
            track: key.track,
            start: new_start,
            end,
        });
    }
    let duration = end.checked_sub(new_start).ok_or(overflow)?;
    let mut next = project.clone();
    let target = &mut next.tracks[key.track];
    let changed = &mut target.clips[key.clip];
    changed.start = new_start;
    changed.source_in = source_in;
    changed.duration = duration;
    sort_track(target);
    finish(next)
}

/// Moves the end edge of a clip to `new_end`. Whether the source has that much media is for the
/// caller to know; the project does not record source lengths.
pub fn trim_end(project: &Project, key: ClipKey, new_end: Time) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let clip = clip_of(project, key)?;
    if clip.end() == Some(new_end) {
        return finish(project.clone());
    }
    let track = &project.tracks[key.track];
    if has_transition_after(track, key.clip) {
        return Err(EditError::TransitionOnEdge { key, edge: "end" });
    }
    if overlaps_other(track, Some(key.clip), clip.start, new_end) {
        return Err(EditError::Overlap {
            track: key.track,
            start: clip.start,
            end: new_end,
        });
    }
    let duration = new_end
        .checked_sub(clip.start)
        .ok_or(EditError::TimeOverflow { key })?;
    let mut next = project.clone();
    next.tracks[key.track].clips[key.clip].duration = duration;
    finish(next)
}

/// Whether a cut `offset` ticks into `clip` keeps the second half on the source positions it had.
/// At another speed the source position of the cut must be a whole tick, see [`Clip::speed_grid`].
fn cut_keeps_samples(clip: &Clip, offset: i64) -> bool {
    if clip.has_speed() {
        return offset.rem_euclid(clip.speed_grid()) == 0;
    }
    let offset = Time::from_ticks(offset);
    let in_source = sample_position(clip.source_in.saturating_add(offset)) - sample_position(clip.source_in);
    let on_timeline = sample_position(clip.start.saturating_add(offset)) - sample_position(clip.start);
    in_source == on_timeline
}

/// The offset of the cut nearest `wanted` within `earliest..=latest` that is inside the clip and
/// keeps the source positions, the earlier one on a tie. The sample rounding repeats every 15
/// ticks, so looking 14 away is enough at normal speed; at another speed a grid step is.
fn aligned_cut(clip: &Clip, wanted: i64, earliest: i64, latest: i64) -> Option<i64> {
    let allowed = |offset: i64| {
        offset > 0 && offset < clip.duration.ticks() && offset >= earliest && offset <= latest
    };
    (0..15.max(clip.speed_grid())).find_map(|distance| {
        [wanted.saturating_sub(distance), wanted.saturating_add(distance)]
            .into_iter()
            .find(|&offset| allowed(offset) && cut_keeps_samples(clip, offset))
    })
}

/// Splits a clip into two at `at`, or at the nearest tick that keeps the audio aligned (see the
/// module rules). A transition stays with the half that touches it.
pub fn split_clip(project: &Project, key: ClipKey, at: Time) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let clip = clip_of(project, key)?;
    let outside = EditError::SplitOutsideClip { key, at };
    let wanted = at.checked_sub(clip.start).ok_or(outside.clone())?;
    if wanted <= Time::ZERO || wanted >= clip.duration {
        return Err(outside);
    }
    let wanted_rest = clip.duration.saturating_sub(wanted);
    if wanted < clip.fade_in || wanted_rest < clip.fade_out {
        return Err(EditError::SplitInsideFade { key, at });
    }

    // The window gives half (rounded down) to the earlier clip and the rest to the later one.
    let track = &project.tracks[key.track];
    let mut head = Time::ZERO;
    let mut tail = Time::ZERO;
    for tr in &track.transitions {
        let before = tr.duration.half_floor();
        if tr.after_clip == key.clip {
            tail = before;
        } else if tr.after_clip.checked_add(1) == Some(key.clip) {
            head = tr.duration.saturating_sub(before);
        }
    }
    if wanted < head || wanted_rest < tail {
        return Err(EditError::SplitInsideTransition { key, at });
    }

    let earliest = clip.fade_in.max(head).ticks();
    let latest = clip.duration.saturating_sub(clip.fade_out.max(tail)).ticks();
    let offset = Time::from_ticks(aligned_cut(clip, wanted.ticks(), earliest, latest).ok_or(outside)?);
    let rest = clip.duration.saturating_sub(offset);

    let mut second = clip.clone();
    second.start = clip.start.saturating_add(offset);
    second.source_in = clip.source_in.saturating_add(clip.source_offset(offset));
    second.duration = rest;
    second.fade_in = Time::ZERO;

    let mut next = project.clone();
    let target = &mut next.tracks[key.track];
    let first = &mut target.clips[key.clip];
    first.duration = offset;
    first.fade_out = Time::ZERO;
    target.clips.insert(key.clip + 1, second);
    // A transition after the clip now follows its second half.
    for transition in &mut target.transitions {
        if transition.after_clip >= key.clip {
            transition.after_clip += 1;
        }
    }
    sort_track(target);
    finish(next)
}

/// Deletes a clip and the transitions touching it.
pub fn delete_clip(project: &Project, key: ClipKey) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    clip_of(project, key)?;
    let mut next = project.clone();
    remove_clip_at(&mut next.tracks[key.track], key.clip);
    finish(next)
}

/// Appends an effect to a clip.
pub fn add_effect(project: &Project, key: ClipKey, effect: Effect) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let mut next = project.clone();
    clip_mut(&mut next, key)?.effects.push(effect);
    finish(next)
}

/// Removes effect `effect` of a clip.
pub fn remove_effect(project: &Project, key: ClipKey, effect: usize) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let mut next = project.clone();
    let clip = clip_mut(&mut next, key)?;
    if effect >= clip.effects.len() {
        return Err(EditError::EffectNotFound { key, effect });
    }
    clip.effects.remove(effect);
    finish(next)
}

/// Adds a transition. Its clips must touch, and validation decides everything else about it,
/// including [`ValidationError::FadeBesideTransition`].
pub fn add_transition(
    project: &Project,
    track: usize,
    transition: Transition,
) -> Result<Project, EditError> {
    unlocked(project, track)?;
    let mut next = project.clone();
    let target = track_mut(&mut next, track)?;
    target.transitions.push(transition);
    target.transitions.sort_by_key(|tr| tr.after_clip);
    finish(next)
}

/// Removes the transition after clip `after_clip`.
pub fn remove_transition(
    project: &Project,
    track: usize,
    after_clip: usize,
) -> Result<Project, EditError> {
    unlocked(project, track)?;
    let mut next = project.clone();
    let target = track_mut(&mut next, track)?;
    let index = target
        .transitions
        .iter()
        .position(|tr| tr.after_clip == after_clip)
        .ok_or(EditError::TransitionNotFound { track, after_clip })?;
    target.transitions.remove(index);
    finish(next)
}

/// Sets the audio gain of a clip, in decibels.
pub fn set_gain(project: &Project, key: ClipKey, gain_db: f64) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let mut next = project.clone();
    clip_mut(&mut next, key)?.gain_db = gain_db;
    finish(next)
}

/// Sets whether a clip is left out of the mix.
pub fn set_muted(project: &Project, key: ClipKey, muted: bool) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let mut next = project.clone();
    clip_mut(&mut next, key)?.muted = muted;
    finish(next)
}

/// Sets the audio fades of a clip.
pub fn set_fades(
    project: &Project,
    key: ClipKey,
    fade_in: Time,
    fade_out: Time,
) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let mut next = project.clone();
    let clip = clip_mut(&mut next, key)?;
    clip.fade_in = fade_in;
    clip.fade_out = fade_out;
    finish(next)
}

/// Sets the playback speed of a clip, keeping the part of the source it plays: the timeline length
/// becomes `source range / speed`, from the same start. Refused with [`EditError::SpeedOverlap`]
/// when the clip would run into the next one, because nothing else moves; with
/// [`EditError::TransitionOnEdge`] when its end holds a transition, because the end would leave the
/// next clip; and as invalid when the fades or the transition at its start no longer fit.
pub fn set_speed(project: &Project, key: ClipKey, speed: f64) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let clip = clip_of(project, key)?;
    let mut next = project.clone();
    if !speed_in_range(speed) {
        clip_mut(&mut next, key)?.speed = speed;
        return finish(next);
    }
    let milli = speed_milli(speed);
    let track = &project.tracks[key.track];
    let scaled = i128::from(clip.duration.ticks()) * i128::from(clip.speed_milli());
    let rounded = (scaled + i128::from(milli) / 2) / i128::from(milli);
    let duration = Time::from_ticks(i64::try_from(rounded.max(1)).map_err(|_| EditError::TimeOverflow { key })?);
    if duration != clip.duration && has_transition_after(track, key.clip) {
        return Err(EditError::TransitionOnEdge { key, edge: "end" });
    }
    let end = clip.start.checked_add(duration).ok_or(EditError::TimeOverflow { key })?;
    if overlaps_other(track, Some(key.clip), clip.start, end) {
        let next_start = track.clips.iter().enumerate().filter(|(i, other)| *i != key.clip && other.start >= clip.start).map(|(_, other)| other.start).min().unwrap_or(end);
        return Err(EditError::SpeedOverlap { key, end, next_start });
    }
    let changed = clip_mut(&mut next, key)?;
    changed.speed = milli as f64 / SPEED_SCALE as f64;
    changed.duration = duration;
    sort_track(&mut next.tracks[key.track]);
    finish(next)
}

/// Replaces the parameters of effect `effect` of a clip.
pub fn set_effect_params(
    project: &Project,
    key: ClipKey,
    effect: usize,
    params: Map<String, Value>,
) -> Result<Project, EditError> {
    unlocked(project, key.track)?;
    let mut next = project.clone();
    let clip = clip_mut(&mut next, key)?;
    clip.effects
        .get_mut(effect)
        .ok_or(EditError::EffectNotFound { key, effect })?
        .params = params;
    finish(next)
}

/// Replaces the parameters and the duration of the transition after clip `after_clip`.
/// Validation refuses a duration the two clips cannot hold.
pub fn set_transition_params(
    project: &Project,
    track: usize,
    after_clip: usize,
    params: Map<String, Value>,
    duration: Time,
) -> Result<Project, EditError> {
    unlocked(project, track)?;
    let mut next = project.clone();
    let transition = track_mut(&mut next, track)?
        .transitions
        .iter_mut()
        .find(|tr| tr.after_clip == after_clip)
        .ok_or(EditError::TransitionNotFound { track, after_clip })?;
    transition.params = params;
    transition.duration = duration;
    finish(next)
}

/// Snapshot undo and redo over a project, keeping at most `depth` undo steps.
#[derive(Debug, Clone)]
pub struct EditHistory {
    current: Project,
    undo: VecDeque<Project>,
    redo: Vec<Project>,
    depth: usize,
}

impl EditHistory {
    /// A history starting at `project`. A `depth` of 0 keeps no undo steps.
    pub fn new(project: Project, depth: usize) -> Self {
        Self {
            current: project,
            undo: VecDeque::new(),
            redo: Vec::new(),
            depth,
        }
    }

    /// The project after the edits applied so far.
    pub fn current(&self) -> &Project {
        &self.current
    }

    /// Whether [`EditHistory::undo`] has a step to go back to.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether [`EditHistory::redo`] has a step to go forward to.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Runs `command` on the current project and records the result. A refused command changes
    /// nothing, and neither does one that returns an equal project, so it adds no undo step.
    pub fn apply<F>(&mut self, command: F) -> Result<&Project, EditError>
    where
        F: FnOnce(&Project) -> Result<Project, EditError>,
    {
        let next = command(&self.current)?;
        if next != self.current {
            let previous = std::mem::replace(&mut self.current, next);
            self.push_undo(previous);
            self.redo.clear();
        }
        Ok(&self.current)
    }

    /// Locks or unlocks `track` without an undo step: the redo stack stays, and no later undo or
    /// redo changes the flag. Tracks are only ever appended, so an index names the same track in
    /// every snapshot; one that predates the track simply does not have it. Undo still restores
    /// the content of a locked track, because the lock refuses new edits and says nothing about
    /// the past.
    pub fn set_track_locked(&mut self, track: usize, locked: bool) -> Result<(), EditError> {
        track_mut(&mut self.current, track)?.locked = locked;
        for snapshot in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if let Some(target) = snapshot.tracks.get_mut(track) {
                target.locked = locked;
            }
        }
        Ok(())
    }

    /// Goes back one step. Returns whether there was a step.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.current, previous));
        true
    }

    /// Goes forward one undone step. Returns whether there was a step.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let previous = std::mem::replace(&mut self.current, next);
        self.push_undo(previous);
        true
    }

    fn push_undo(&mut self, project: Project) {
        if self.depth == 0 {
            return;
        }
        if self.undo.len() == self.depth {
            self.undo.pop_front();
        }
        self.undo.push_back(project);
    }
}
