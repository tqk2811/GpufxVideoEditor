//! Object tracking data: the motion tracks of source files and what follows them.
//!
//! A [`MotionTrack`] is the path of one object through one source file, in the time and the
//! picture of that source, so a split, a trim, a new speed, playing backwards or a proxy keep every
//! clip of the file on it. An overlay ([`Follow`]) or the region of an effect ([`TrackLink`])
//! moves with it: the track is read through the clip that shows the object at each moment.
//!
//! Smoothing is applied when the track is read, never written into the samples: a Gaussian over
//! source time. It was chosen over One-Euro because it needs no state, so a frame looks the same
//! whatever was played or sought before it (preview and export agree).

use crate::keyframe::ClipTransform;
use crate::project::{Clip, Project};
use crate::time::{Time, TICKS_PER_SECOND};
use serde::{Deserialize, Serialize};

/// The most samples one track may have (an hour at 120 fps).
pub const MAX_TRACK_SAMPLES: usize = 432_000;
/// The largest smoothing of a follow, in seconds of source time.
pub const MAX_FOLLOW_SMOOTH: f64 = 2.0;
/// The largest offset of a follow, in parts of the frame.
pub const MAX_FOLLOW_OFFSET: f64 = 8.0;

/// The path of one object through one source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionTrack {
    /// Unique among the tracks of the project; what a [`TrackLink`] names.
    pub id: u32,
    /// The `source` of the clips that show the file.
    pub media_path: String,
    /// Width over height of the source picture as displayed (its rotation applied), the shape the
    /// sample coordinates are parts of.
    pub aspect: f64,
    /// Samples sorted by `source_time`, no two at the same time.
    #[serde(default)]
    pub samples: Vec<TrackSample>,
}

/// Where the object is at one moment of the source.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackSample {
    /// Time in the source file.
    pub source_time: Time,
    /// Middle of the object, in parts of the source picture width and height.
    pub cx: f64,
    pub cy: f64,
    /// Size of the object, in parts of the source picture width and height.
    pub w: f64,
    pub h: f64,
    /// Clockwise degrees.
    #[serde(default)]
    pub angle: f64,
    /// How sure the tracker was, 0 to 1. 1 when absent.
    #[serde(default = "full_confidence")]
    pub confidence: f64,
    /// Placed by the user rather than the tracker.
    #[serde(default, skip_serializing_if = "is_false")]
    pub manual: bool,
}

fn full_confidence() -> f64 {
    1.0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Which track something follows and how.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackLink {
    /// The [`MotionTrack::id`] followed.
    pub track_id: u32,
    /// The source time at which the follower stands where it was placed: its own place is where it
    /// is at this moment, and it moves by how the object moved since.
    pub anchor_source_time: Time,
    /// Also grow and shrink with the object.
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_scale: bool,
    /// Also turn with the object.
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_rotation: bool,
    /// Gaussian smoothing of the track, in seconds of source time; 0 is off.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub smooth: f64,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

fn is_zero_pair(value: &[f64; 2]) -> bool {
    *value == [0.0, 0.0]
}

/// An overlay clip (sticker, text, image, censor region) that moves with a motion track seen
/// through another clip, the reference clip that shows the tracked file.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Follow {
    #[serde(flatten)]
    pub link: TrackLink,
    /// Track index of the reference clip.
    pub ref_track: usize,
    /// Clip index of the reference clip in its track.
    pub ref_clip: usize,
    /// Extra shift of the overlay, in parts of the frame width and height.
    #[serde(default, skip_serializing_if = "is_zero_pair")]
    pub offset: [f64; 2],
}

/// How a following picture moves at one moment, applied by [`ClipTransform::rect`]: its middle
/// keeps its place relative to the object, which went from `from` to `to` (parts of the frame),
/// grew `scale` times and turned `rotation` clockwise degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowPose {
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub scale: f64,
    pub rotation: f64,
}

impl FollowPose {
    /// Where point `point` of the frame goes, for a frame of `aspect` (width over height).
    pub fn map(&self, point: [f64; 2], aspect: f64) -> [f64; 2] {
        let vx = (point[0] - self.from[0]) * aspect;
        let vy = point[1] - self.from[1];
        let (sine, cosine) = self.rotation.to_radians().sin_cos();
        let rx = (cosine * vx - sine * vy) * self.scale;
        let ry = (sine * vx + cosine * vy) * self.scale;
        [self.to[0] + rx / aspect, self.to[1] + ry]
    }
}

/// The object at one moment: middle, size and angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackPoint {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub angle: f64,
}

impl MotionTrack {
    /// The object at source time `at`: interpolated between the samples around it (held at the ends),
    /// then smoothed over `smooth` seconds when that is above 0. `None` without samples.
    pub fn point_at(&self, at: Time, smooth: f64) -> Option<TrackPoint> {
        let samples = &self.samples;
        if samples.is_empty() {
            return None;
        }
        if smooth > 0.0 && smooth.is_finite() {
            let sigma = smooth * TICKS_PER_SECOND as f64;
            let reach = Time::from_ticks((sigma * 3.0).ceil() as i64);
            let low = samples.partition_point(|s| s.source_time < at.saturating_sub(reach));
            let high = samples.partition_point(|s| s.source_time <= at.saturating_add(reach));
            if high > low {
                let base = samples[low].angle;
                let mut sum = [0.0; 5];
                let mut total = 0.0;
                for sample in &samples[low..high] {
                    let d = (sample.source_time.ticks() - at.ticks()) as f64 / sigma;
                    let weight = (-0.5 * d * d).exp();
                    let angle = base + wrap_degrees(sample.angle - base);
                    for (slot, value) in sum.iter_mut().zip([sample.cx, sample.cy, sample.w, sample.h, angle]) {
                        *slot += weight * value;
                    }
                    total += weight;
                }
                if total > 0.0 {
                    let [cx, cy, w, h, angle] = sum.map(|value| value / total);
                    return Some(TrackPoint { cx, cy, w, h, angle });
                }
            }
        }
        let next = samples.partition_point(|s| s.source_time <= at);
        let point = |s: &TrackSample| TrackPoint { cx: s.cx, cy: s.cy, w: s.w, h: s.h, angle: s.angle };
        if next == 0 {
            return Some(point(&samples[0]));
        }
        if next == samples.len() {
            return Some(point(&samples[next - 1]));
        }
        let (a, b) = (&samples[next - 1], &samples[next]);
        let span = (b.source_time.ticks() - a.source_time.ticks()) as f64;
        let k = (at.ticks() - a.source_time.ticks()) as f64 / span;
        let lerp = |x: f64, y: f64| x + (y - x) * k;
        Some(TrackPoint { cx: lerp(a.cx, b.cx), cy: lerp(a.cy, b.cy), w: lerp(a.w, b.w), h: lerp(a.h, b.h), angle: a.angle + wrap_degrees(b.angle - a.angle) * k })
    }

    /// Adds `sample`, or replaces the one at the same source time, keeping the order.
    pub fn upsert(&mut self, sample: TrackSample) {
        match self.samples.binary_search_by_key(&sample.source_time, |s| s.source_time) {
            Ok(index) => self.samples[index] = sample,
            Err(index) => self.samples.insert(index, sample),
        }
    }

    /// Why the track cannot be used, if it cannot.
    pub fn problem(&self) -> Option<String> {
        if self.media_path.is_empty() {
            return Some("media_path is empty".to_owned());
        }
        if !(self.aspect.is_finite() && self.aspect > 0.0) {
            return Some(format!("aspect {} must be a positive number", self.aspect));
        }
        if self.samples.len() > MAX_TRACK_SAMPLES {
            return Some(format!("{} samples, at most {MAX_TRACK_SAMPLES}", self.samples.len()));
        }
        for (i, s) in self.samples.iter().enumerate() {
            if s.source_time < Time::ZERO {
                return Some(format!("sample {i}: negative source_time"));
            }
            if i > 0 && s.source_time <= self.samples[i - 1].source_time {
                return Some(format!("sample {i}: source_time not after the sample before"));
            }
            let finite = [s.cx, s.cy, s.w, s.h, s.angle].iter().all(|v| v.is_finite() && v.abs() <= 1.0e6);
            if !finite || s.w < 0.0 || s.h < 0.0 {
                return Some(format!("sample {i}: coordinates must be finite and the size not negative"));
            }
            if !(0.0..=1.0).contains(&s.confidence) {
                return Some(format!("sample {i}: confidence {} not from 0 to 1", s.confidence));
            }
        }
        None
    }
}

/// `degrees` brought into -180..180.
fn wrap_degrees(degrees: f64) -> f64 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

impl TrackLink {
    /// Why the link cannot be used, apart from the track it names.
    pub fn problem(&self) -> Option<String> {
        if self.anchor_source_time < Time::ZERO {
            return Some("negative anchor_source_time".to_owned());
        }
        if !(self.smooth.is_finite() && (0.0..=MAX_FOLLOW_SMOOTH).contains(&self.smooth)) {
            return Some(format!("smooth {} not from 0 to {MAX_FOLLOW_SMOOTH}", self.smooth));
        }
        None
    }
}

impl Project {
    /// The motion track with `id`.
    pub fn motion_track(&self, id: u32) -> Option<&MotionTrack> {
        self.motion_tracks.iter().find(|track| track.id == id)
    }

    /// The smallest id no motion track has.
    pub fn next_motion_track_id(&self) -> u32 {
        self.motion_tracks.iter().map(|track| track.id.saturating_add(1)).max().unwrap_or(1)
    }

    /// Why `follow` on the clip at (`track`, `clip`) cannot be used, if it cannot.
    pub fn follow_problem(&self, follow: &Follow, track: usize, clip: usize) -> Option<String> {
        if let Some(problem) = follow.link.problem() {
            return Some(problem);
        }
        if !follow.offset.iter().all(|v| v.is_finite() && v.abs() <= MAX_FOLLOW_OFFSET) {
            return Some(format!("offset must be finite and within {MAX_FOLLOW_OFFSET}"));
        }
        let Some(motion) = self.motion_track(follow.link.track_id) else {
            return Some(format!("no motion track {}", follow.link.track_id));
        };
        if (follow.ref_track, follow.ref_clip) == (track, clip) {
            return Some("a clip cannot follow itself".to_owned());
        }
        let Some(reference) = self.tracks.get(follow.ref_track).and_then(|t| t.clips.get(follow.ref_clip)) else {
            return Some(format!("no reference clip {} on track {}", follow.ref_clip, follow.ref_track));
        };
        if reference.source != motion.media_path {
            return Some(format!("the reference clip shows {}, not the tracked {}", reference.source, motion.media_path));
        }
        if reference.follow.is_some() {
            return Some("the reference clip follows a track itself".to_owned());
        }
        None
    }

    /// Whether `follow` still holds its anchor through its reference clip.
    fn follow_holds(&self, follow: &Follow) -> Option<bool> {
        let motion = self.motion_track(follow.link.track_id)?;
        let reference = self.tracks.get(follow.ref_track)?.clips.get(follow.ref_clip)?;
        Some(reference.source == motion.media_path && reference.plays_source(follow.link.anchor_source_time))
    }

    /// After an edit: points every follow at the clip of its reference track that shows its
    /// anchor (the piece of a split, the clip renumbered by a move or a delete), and drops the
    /// follows whose anchor no clip of that track shows any more, or whose track is gone. Also drops
    /// the tracks of effect regions that no longer exist or no longer match their clip.
    pub(crate) fn repair_follows(&mut self) {
        for t in 0..self.tracks.len() {
            for c in 0..self.tracks[t].clips.len() {
                let clip = &self.tracks[t].clips[c];
                if let Some(follow) = clip.follow {
                    let repaired = if self.follow_holds(&follow) == Some(true) {
                        Some(follow)
                    } else {
                        self.motion_track(follow.link.track_id).and_then(|motion| {
                            let candidates = self.tracks.get(follow.ref_track)?;
                            let found = candidates.clips.iter().enumerate().position(|(i, other)| (follow.ref_track, i) != (t, c) && other.source == motion.media_path && other.plays_source(follow.link.anchor_source_time))?;
                            Some(Follow { ref_clip: found, ..follow })
                        })
                    };
                    self.tracks[t].clips[c].follow = repaired;
                }
                let source = self.tracks[t].clips[c].source.clone();
                let regions: Vec<bool> = self.tracks[t].clips[c].effects.iter().map(|effect| effect.region.and_then(|region| region.link).is_some_and(|link| self.motion_track(link.track_id).is_none_or(|motion| motion.media_path != source))).collect();
                for (effect, drop) in self.tracks[t].clips[c].effects.iter_mut().zip(regions) {
                    if drop {
                        if let Some(region) = effect.region.as_mut() {
                            region.link = None;
                        }
                    }
                }
            }
        }
    }

    /// The pose of a clip following `follow` at timeline time `t`, or `None` when it cannot be
    /// worked out (no samples, or a reference that is gone).
    pub fn follow_pose(&self, follow: &Follow, t: Time) -> Option<FollowPose> {
        let motion = self.motion_track(follow.link.track_id)?;
        let track = self.tracks.get(follow.ref_track)?;
        let reference = track.clips.get(follow.ref_clip)?;
        let output_aspect = f64::from(self.output.width) / f64::from(self.output.height.max(1));
        let anchor_time = reference.timeline_time_of(follow.link.anchor_source_time);
        let place = |time: Time| -> Option<(TrackPoint, ClipTransform)> {
            let source = reference.source_time_at(time.saturating_sub(reference.start)).max(Time::ZERO);
            let point = motion.point_at(source, follow.link.smooth)?;
            Some((point, crate::schedule::clip_transform_at(track, follow.ref_clip, time)))
        };
        let (anchor_point, anchor_transform) = place(anchor_time)?;
        let (point, transform) = place(t)?;
        let (from, from_size, from_angle) = to_output(anchor_point, anchor_transform, output_aspect, motion.aspect);
        let (to, to_size, to_angle) = to_output(point, transform, output_aspect, motion.aspect);
        let scale = if follow.link.follow_scale && from_size > 0.0 { to_size / from_size } else { 1.0 };
        let rotation = if follow.link.follow_rotation { to_angle - from_angle } else { 0.0 };
        Some(FollowPose { from, to: [to[0] + follow.offset[0], to[1] + follow.offset[1]], scale, rotation })
    }
}

/// A tracked point of a source of `source_aspect`, shown by a clip drawn with `transform`, in the
/// output frame: its middle (parts of the frame), its size (geometric mean of width and height, in
/// parts of the frame height) and its angle (clockwise degrees).
fn to_output(point: TrackPoint, transform: ClipTransform, output_aspect: f64, source_aspect: f64) -> ([f64; 2], f64, f64) {
    let mut plain = transform;
    plain.follow = None;
    let rect = plain.rect(output_aspect, source_aspect);
    let (x, y) = (rect.x + point.cx * rect.width, rect.y + point.cy * rect.height);
    let (mx, my) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let (sine, cosine) = transform.rotation.to_radians().sin_cos();
    let (dx, dy) = ((x - mx) * output_aspect, y - my);
    let middle = [mx + (cosine * dx - sine * dy) / output_aspect, my + sine * dx + cosine * dy];
    let size = (point.w * rect.width * output_aspect * point.h * rect.height).max(0.0).sqrt();
    (middle, size, point.angle + transform.rotation)
}

/// A rectangle or an ellipse inside a picture, in parts of its width and height, before turning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionBox {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub angle: f64,
}

/// Moves `region` (parts of the picture of `clip`) with the track `link` names, whose samples are
/// in the same picture, at timeline time `t`.
pub fn follow_region(project: &Project, clip: &Clip, link: &TrackLink, region: RegionBox, t: Time) -> RegionBox {
    let Some(motion) = project.motion_track(link.track_id) else { return region };
    let now = clip.source_time_at(t.saturating_sub(clip.start)).max(Time::ZERO);
    let (Some(anchor), Some(point)) = (motion.point_at(link.anchor_source_time, link.smooth), motion.point_at(now, link.smooth)) else {
        return region;
    };
    let aspect = motion.aspect;
    let scale = if link.follow_scale {
        let (a, b) = ((anchor.w * anchor.h).max(0.0).sqrt(), (point.w * point.h).max(0.0).sqrt());
        if a > 0.0 { b / a } else { 1.0 }
    } else {
        1.0
    };
    let rotation = if link.follow_rotation { point.angle - anchor.angle } else { 0.0 };
    let pose = FollowPose { from: [anchor.cx, anchor.cy], to: [point.cx, point.cy], scale, rotation };
    let [cx, cy] = pose.map([region.cx, region.cy], aspect);
    RegionBox { cx, cy, w: region.w * scale, h: region.h * scale, angle: region.angle + rotation }
}
