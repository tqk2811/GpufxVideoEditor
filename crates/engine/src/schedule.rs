//! Frame scheduler: a pure function from a project and a frame index to the layers to composite.
//!
//! All math is integer. The functions assume a project that passed [`crate::validate`];
//! on an invalid project they do not panic but the result is unspecified.
//!
//! # Rounding rules (defined once, here)
//!
//! * **Frame time.** Output frame `n` starts at `floor(n * den * 90000 / num)` ticks for the rate
//!   `num / den` ([`crate::FrameRate::frame_start`]). Each start is rounded on its own, so the
//!   24000/1001 cadence (3753.75 ticks a frame) never drifts.
//! * **Audio tracks.** Tracks of kind `audio` give no layer, but they still count in the
//!   [`Layer::track`] numbering and in the frame count.
//! * **Hidden tracks.** A hidden track gives no layer, transitions included, but still counts in the
//!   frame count, so hiding a track does not change the length of the project.
//! * **Frame count.** The timeline end is the largest clip end over all tracks, audio tracks
//!   included, so music longer than the picture is not cut and a project of only audio has a
//!   length. Frames past the last visual clip have no layer and show black. The count is the
//!   number of frames that start before `end`, so a final partial frame is kept. An empty project
//!   has 0 frames.
//! * **Clip coverage.** A clip covers the half-open range `[start, start + duration)`. A frame is
//!   inside the clip when its start time is in that range. The source time is exactly
//!   `source_in + floor((frame_time - start) * speed)` ticks (see [`Clip::source_offset`]); the
//!   decoder picks the source frame for that time, dropping frames when fast and repeating them
//!   when slow. A clip with holds (see [`Clip::hold_start`]) counts that time from the end of its
//!   start hold, and shows its first or last frame inside the holds ([`Clip::source_time_at`]).
//! * **Transition window.** For a transition of `duration` after clip `i`, with the cut at the
//!   start `s` of clip `i + 1`, the window is `[s - duration / 2, s - duration / 2 + duration)` where
//!   `duration / 2` rounds down. The first clip supplies `duration / 2` ticks and the second the rest.
//!   Inside the window both clips map time with their own formula above, so the first clip's
//!   source time may run past its out point and the second clip's may fall before its in point.
//!   A source time below zero is clamped to zero; the decoder holds the first or last frame at
//!   the ends of a file.
//! * **Progress.** `elapsed = frame_time - window_start` out of `total = duration`, kept as an exact
//!   ratio. [`Progress::parts_per_million`] floors `elapsed * 1_000_000 / total`.
//! * **Clip clock.** `elapsed = frame_time - visible_start` out of `duration = visible_end -
//!   visible_start`, in timeline ticks: the time the viewer sees, so `speed` does not scale it (a 2x
//!   clip of 5 s on the timeline counts 0 to 5). The clip is visible from the start of the window of
//!   the transition into it (else its `start`) to the end of the window of the transition out of it
//!   (else `start + duration`), so a clip that fades in starts counting when it first appears, not
//!   at the cut. Each clip counts from its own first visible moment, also after a split. `elapsed`
//!   is clamped to `0..=duration`.

use crate::keyframe::ClipTransform;
use crate::project::{Clip, Effect, Project, Track, Transition};
use crate::clip_animation::animation_pose;
use crate::time::Time;
use serde_json::{Map, Value};

/// Scale of [`Progress::parts_per_million`].
pub const PROGRESS_PPM_SCALE: i64 = 1_000_000;

/// Exact transition progress: `elapsed / total`, with `0 <= elapsed < total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Ticks since the window started.
    pub elapsed: Time,
    /// Window length in ticks.
    pub total: Time,
}

impl Progress {
    /// `floor(elapsed * 1_000_000 / total)`, in `0..1_000_000`.
    pub fn parts_per_million(self) -> i64 {
        let value = i128::from(self.elapsed.ticks()) * i128::from(PROGRESS_PPM_SCALE)
            / i128::from(self.total.ticks().max(1));
        value as i64
    }
}

/// How far a clip has played, in timeline time (see the module rounding rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipClock {
    /// Time since the clip first became visible (the start of the transition window into it, else
    /// its start), in `0..=duration`.
    pub elapsed: Time,
    /// Length of the time the clip is visible: from the start of the window of the transition into
    /// it (else its start) to the end of the window of the transition out of it (else its end).
    pub duration: Time,
}

/// One decoded-source request: which file, at which source time, with which effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceSample<'a> {
    /// Index of the clip in its track.
    pub clip: usize,
    /// The clip itself, for what the source path alone does not say, such as the text of a
    /// text clip.
    pub model: &'a Clip,
    /// Source media path.
    pub source: &'a str,
    /// Time inside the source to show.
    pub source_time: Time,
    /// Effects to apply to this source before compositing.
    pub effects: &'a [Effect],
    /// Where the picture sits in the frame and how it is drawn, now.
    pub transform: ClipTransform,
    /// Timeline ticks since the start of the clip; negative inside the transition into it. The
    /// time of the keyframes of its effects.
    pub offset: Time,
    /// Where the clip is in its own playing time.
    pub clock: ClipClock,
}

/// What one track contributes to one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayerContent<'a> {
    /// A single clip.
    Clip(SourceSample<'a>),
    /// A transition from `from` (the earlier clip) to `to` (the later clip).
    Transition {
        /// Transition package name.
        package: &'a str,
        /// Static transition parameters.
        params: &'a Map<String, Value>,
        /// Earlier clip.
        from: SourceSample<'a>,
        /// Later clip.
        to: SourceSample<'a>,
        /// Position inside the transition window.
        progress: Progress,
    },
}

/// A layer of one frame. Layers are returned bottom track first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer<'a> {
    /// Index of the track.
    pub track: usize,
    /// What the track shows.
    pub content: LayerContent<'a>,
}

/// Total number of output frames (see the module rounding rules). 0 for an empty
/// project or an invalid frame rate.
pub fn frame_count(project: &Project) -> i64 {
    let end = project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.start.saturating_add(c.duration))
        .max()
        .unwrap_or(Time::ZERO);
    project.output.frame_rate.frames_before(end)
}

/// Start time of output frame `frame` (see [`FrameRate::frame_start`]), or `None` on overflow.
pub fn frame_time(project: &Project, frame: i64) -> Option<Time> {
    project.output.frame_rate.frame_start(frame)
}

/// Layers of output frame `frame`, bottom track first. Tracks with nothing at that time
/// contribute no layer. Frames outside `0..frame_count` give an empty list.
pub fn layers_at_frame(project: &Project, frame: i64) -> Vec<Layer<'_>> {
    if frame < 0 || frame >= frame_count(project) {
        return Vec::new();
    }
    let Some(t) = frame_time(project, frame) else {
        return Vec::new();
    };
    project
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| !track.is_audio() && !track.hidden)
        .filter_map(|(i, track)| content_at(track, t).map(|content| Layer { track: i, content: with_follow(project, i, content, t) }))
        .collect()
}

/// `content` of track `track` with the clips that follow a motion track moved where the track puts
/// them at `t`, and the clips locked to one moved against it.
fn with_follow<'a>(project: &'a Project, track: usize, content: LayerContent<'a>, t: Time) -> LayerContent<'a> {
    let follow = |mut sample: SourceSample<'a>| {
        if let Some(follow) = &sample.model.follow {
            follow_transform(project, follow, &mut sample.transform, t);
        } else if sample.model.lock.is_some() {
            lock_transform(project, track, sample.clip, &mut sample.transform, t);
        }
        sample
    };
    match content {
        LayerContent::Clip(sample) => LayerContent::Clip(follow(sample)),
        LayerContent::Transition { package, params, from, to, progress } => LayerContent::Transition { package, params, from: follow(from), to: follow(to), progress },
    }
}

/// Moves `transform`, that of a clip following `follow`, where the motion track puts it at timeline
/// time `t`: the track is read at the source time the reference clip shows at `t` (its trim, speed
/// and direction applied), mapped through the place and transform of the reference clip at `t` to
/// the output frame, and the clip moves by how far the object moved since the anchor. Its own place
/// and keyframes are where it stands at the anchor, so they add to the motion. Leaves it as it is
/// when the track cannot be read.
pub fn follow_transform(project: &Project, follow: &crate::tracking::Follow, transform: &mut ClipTransform, t: Time) {
    if let Some(pose) = project.follow_pose(follow, t) {
        transform.rotation += pose.rotation;
        transform.rotates |= follow.link.follow_rotation || follow.perspective;
        transform.follow = Some(pose);
        transform.pin = project.follow_pin(follow, t);
    }
}

/// Moves `transform`, that of clip `clip` of track `track` which locks to a motion track, against
/// the track at timeline time `t` (see [`Project::lock_pose`]). Leaves it as it is without a lock
/// or when the track cannot be read.
pub fn lock_transform(project: &Project, track: usize, clip: usize, transform: &mut ClipTransform, t: Time) {
    let Some(lock) = project.tracks.get(track).and_then(|lane| lane.clips.get(clip)).and_then(|c| c.lock) else { return };
    transform.rotates |= lock.link.follow_rotation;
    if let Some(pose) = project.lock_pose(track, clip, t) {
        transform.rotation += pose.rotation;
        transform.follow = Some(pose);
    }
}

/// The transform of clip `clip` of track `track` at timeline time `t` as it is drawn: its preset
/// animations and its lock included, not a follow.
pub(crate) fn placed_transform_at(project: &Project, track: usize, clip: usize, t: Time) -> ClipTransform {
    let mut transform = clip_transform_at(&project.tracks[track], clip, t);
    lock_transform(project, track, clip, &mut transform, t);
    transform
}

/// The transform of clip `clip` of `track` at timeline time `t`, its preset animations included.
pub(crate) fn clip_transform_at(track: &Track, clip: usize, t: Time) -> ClipTransform {
    sample_at(track, clip, t).transform
}

fn sample_at(track: &Track, clip: usize, t: Time) -> SourceSample<'_> {
    let c = &track.clips[clip];
    let offset = t.saturating_sub(c.start);
    let (visible_start, visible_end) = visible_span(track, clip);
    let clock = ClipClock {
        elapsed: t.saturating_sub(visible_start).max(Time::ZERO).min(visible_end.saturating_sub(visible_start)),
        duration: visible_end.saturating_sub(visible_start),
    };
    let mut transform = c.transform_at(offset);
    // The preset animations of a clip move its picture after its own place and keyframes, on the
    // clock of the time it is visible.
    if !c.animation.is_empty() {
        transform = transform.with_pose(animation_pose(&c.animation, clock));
    }
    SourceSample {
        clip,
        model: c,
        source: &c.source,
        source_time: c.source_time_at(offset).max(Time::ZERO),
        effects: &c.effects,
        transform,
        offset,
        clock,
    }
}

/// The time clip `clip` is visible: from the start of the window of the transition into it (else its
/// start) to the end of the window of the transition out of it (else its end).
fn visible_span(track: &Track, clip: usize) -> (Time, Time) {
    let c = &track.clips[clip];
    let start = track
        .transitions
        .iter()
        .find(|tr| tr.after_clip + 1 == clip)
        .map_or(c.start, |tr| transition_window(tr, c).0);
    let end = track
        .transitions
        .iter()
        .find(|tr| tr.after_clip == clip)
        .and_then(|tr| track.clips.get(clip + 1).map(|next| transition_window(tr, next).1))
        .unwrap_or_else(|| c.start.saturating_add(c.duration));
    (start, end)
}

/// The start and end ticks of the window of `transition`, whose later clip is `next`. The window
/// is centred on the cut, which is the start of `next`.
pub(crate) fn transition_window(transition: &Transition, next: &Clip) -> (Time, Time) {
    let start = next.start.saturating_sub(transition.duration.half_floor());
    (start, start.saturating_add(transition.duration))
}

fn content_at(track: &Track, t: Time) -> Option<LayerContent<'_>> {
    for tr in &track.transitions {
        let Some(b) = track.clips.get(tr.after_clip + 1) else {
            continue;
        };
        let (window_start, window_end) = transition_window(tr, b);
        if t >= window_start && t < window_end {
            return Some(LayerContent::Transition {
                package: &tr.package,
                params: &tr.params,
                from: sample_at(track, tr.after_clip, t),
                to: sample_at(track, tr.after_clip + 1, t),
                progress: Progress {
                    elapsed: t.saturating_sub(window_start),
                    total: tr.duration,
                },
            });
        }
    }
    track
        .clips
        .iter()
        .position(|c| t >= c.start && t < c.start.saturating_add(c.duration))
        .map(|i| LayerContent::Clip(sample_at(track, i, t)))
}
