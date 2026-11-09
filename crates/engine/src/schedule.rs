//! Frame scheduler: a pure function from a project and a frame index to the layers to composite.
//!
//! All math is integer. The functions assume a project that passed [`crate::validate`];
//! on an invalid project they do not panic but the result is unspecified.
//!
//! # Rounding rules (defined once, here)
//!
//! * **Frame time.** Output frame `n` starts at `n * frame_duration` ticks. This is exact, so
//!   the 30000/1001 cadence (`frame_duration = 3003`) never drifts.
//! * **Audio tracks.** Everything in this module ignores tracks of kind `audio`; they still
//!   count in the [`Layer::track`] numbering.
//! * **Frame count.** The timeline end is the largest clip end over all video tracks. The count is
//!   `ceil(end / frame_duration)`, so a final partial frame is kept. An empty project has 0 frames.
//! * **Clip coverage.** A clip covers the half-open range `[start, start + duration)`. A frame is
//!   inside the clip when its start time is in that range. The source time is exactly
//!   `source_in + (frame_time - start)` ticks; the decoder picks the source frame for that time.
//! * **Transition window.** For a transition of `duration` after clip `i`, with the cut at the
//!   start `s` of clip `i + 1`, the window is `[s - duration / 2, s - duration / 2 + duration)` where
//!   `duration / 2` rounds down. The first clip supplies `duration / 2` ticks and the second the rest.
//!   Inside the window both clips map time with their own formula above, so the first clip's
//!   source time may run past its out point and the second clip's may fall before its in point.
//!   A source time below zero is clamped to zero; the decoder holds the first or last frame at
//!   the ends of a file.
//! * **Progress.** `elapsed = frame_time - window_start` out of `total = duration`, kept as an exact
//!   ratio. [`Progress::parts_per_million`] floors `elapsed * 1_000_000 / total`.

use crate::project::{Effect, Project, Track};
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

/// One decoded-source request: which file, at which source time, with which effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceSample<'a> {
    /// Index of the clip in its track.
    pub clip: usize,
    /// Source media path.
    pub source: &'a str,
    /// Time inside the source to show.
    pub source_time: Time,
    /// Effects to apply to this source before compositing.
    pub effects: &'a [Effect],
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
/// project or a non-positive `frame_duration`.
pub fn frame_count(project: &Project) -> i64 {
    let end = project
        .tracks
        .iter()
        .filter(|t| !t.is_audio())
        .flat_map(|t| t.clips.iter())
        .map(|c| c.start.saturating_add(c.duration))
        .max()
        .unwrap_or(Time::ZERO);
    end.div_ceil(project.output.frame_duration)
        .unwrap_or(0)
        .max(0)
}

/// Start time of output frame `frame` (`frame * frame_duration`), or `None` on overflow.
pub fn frame_time(project: &Project, frame: i64) -> Option<Time> {
    project.output.frame_duration.checked_mul(frame)
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
        .filter(|(_, track)| !track.is_audio())
        .filter_map(|(i, track)| content_at(track, t).map(|content| Layer { track: i, content }))
        .collect()
}

fn sample_at(track: &Track, clip: usize, t: Time) -> SourceSample<'_> {
    let c = &track.clips[clip];
    let offset = t.saturating_sub(c.start);
    SourceSample {
        clip,
        source: &c.source,
        source_time: c.source_in.saturating_add(offset).max(Time::ZERO),
        effects: &c.effects,
    }
}

fn content_at(track: &Track, t: Time) -> Option<LayerContent<'_>> {
    for tr in &track.transitions {
        let Some(b) = track.clips.get(tr.after_clip + 1) else {
            continue;
        };
        let window_start = b.start.saturating_sub(tr.duration.half_floor());
        let window_end = window_start.saturating_add(tr.duration);
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
