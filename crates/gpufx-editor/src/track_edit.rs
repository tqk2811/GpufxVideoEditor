//! What the editor does with motion tracks for a person: picking an object, boxes placed by hand,
//! asking the window for tracker runs, overlays that follow, censor regions that follow, effect
//! regions and baking a follow into keyframes. Every change is one undo step, a slider drag one
//! live edit.

use crate::editor::EditorState;
use crate::i18n::Message;
use crate::tracking::Direction;
use engine::{ClipKey, EditError, EffectRegion, Follow, FrameMapping, MotionTrack, Project, Time, TrackLink, TrackLock, TrackSample};

/// A tracker run the inspector asked for; the window starts it with the file the preview decodes
/// (the proxy when there is one), see [`EditorState::run_track_ask`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackRunAsk {
    pub track_id: u32,
    /// The source time of the sample to start from; `None` for the default of
    /// [`crate::tracking::RunRequest::for_track`].
    pub from: Option<Time>,
    pub direction: Direction,
    /// At most this many frames (1 for "Track 1 frame").
    pub limit: Option<usize>,
}

/// A box below this size, in parts of the picture, is a click rather than a box.
pub const MIN_BOX_SIDE: f64 = 0.01;
/// Below this confidence a sample counts as lost (red on the timeline).
pub const LOW_CONFIDENCE: f64 = 0.5;

/// The clip that shows the tracked file for an overlay at timeline time `at`: of the clips of
/// `media` that follow nothing, other than `overlay`, the one playing at `at`, else the nearest.
pub fn reference_clip(project: &Project, media: &str, at: Time, overlay: Option<ClipKey>) -> Option<ClipKey> {
    let mut best: Option<(i64, ClipKey)> = None;
    for (t, track) in project.tracks.iter().enumerate() {
        for (c, clip) in track.clips.iter().enumerate() {
            let key = ClipKey { track: t, clip: c };
            if clip.source != media || clip.follow.is_some() || Some(key) == overlay {
                continue;
            }
            let end = clip.start.saturating_add(clip.duration);
            let distance = if at < clip.start { clip.start.ticks() - at.ticks() } else if at >= end { at.ticks() - end.ticks() + 1 } else { 0 };
            if best.is_none_or(|(known, _)| distance < known) {
                best = Some((distance, key));
            }
        }
    }
    best.map(|(_, key)| key)
}

/// The follow of an overlay through reference clip `reference` of `project`, anchored at the
/// moment of the reference nearest timeline time `at`: there the overlay stands where it is
/// placed, so attaching never moves it at that moment (no offset needed).
pub fn follow_at(project: &Project, track_id: u32, reference: ClipKey, at: Time, follow_scale: bool, follow_rotation: bool) -> Option<Follow> {
    let clip = project.tracks.get(reference.track)?.clips.get(reference.clip)?;
    let last = clip.start.saturating_add(clip.duration).saturating_sub(Time::from_ticks(1)).max(clip.start);
    let time = at.clamp(clip.start, last);
    let anchor_source_time = clip.source_time_at(time.saturating_sub(clip.start)).max(Time::ZERO);
    project.motion_track(track_id)?;
    // A new follower takes the smoothing the others of the track share.
    let smooth = track_smooth(project, track_id);
    let link = TrackLink { track_id, anchor_source_time, follow_scale, follow_rotation, smooth };
    Some(Follow { link, ref_track: reference.track, ref_clip: reference.clip, offset: [0.0, 0.0], perspective: false })
}

/// The smoothing the followers of motion track `track_id` share (the first one found), 0 without any.
pub fn track_smooth(project: &Project, track_id: u32) -> f64 {
    project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .find_map(|clip| {
            clip.follow.filter(|follow| follow.link.track_id == track_id).map(|follow| follow.link.smooth).or_else(|| clip.effects.iter().find_map(|effect| effect.region.and_then(|region| region.link).filter(|link| link.track_id == track_id).map(|link| link.smooth)))
        })
        .unwrap_or(0.0)
}

/// The sample of `track` within `tolerance` of source time `at`, the nearest.
pub fn sample_near(track: &MotionTrack, at: Time, tolerance: Time) -> Option<TrackSample> {
    let next = track.samples.partition_point(|s| s.source_time < at);
    [next.checked_sub(1), Some(next)]
        .into_iter()
        .flatten()
        .filter_map(|i| track.samples.get(i))
        .filter(|s| (s.source_time.ticks() - at.ticks()).abs() <= tolerance.ticks())
        .min_by_key(|s| (s.source_time.ticks() - at.ticks()).abs())
        .copied()
}

/// Source time `at` moved to the start of the nearest frame of a source of `rate` frames per
/// second `(num, den)`, counted from time zero.
pub fn snap_to_frame(at: Time, (num, den): (i32, i32)) -> Time {
    let unit = i128::from(den) * i128::from(engine::TICKS_PER_SECOND);
    let num = i128::from(num.max(1));
    let index = (i128::from(at.ticks()) * num + unit / 2).div_euclid(unit);
    Time::from_ticks((index * unit / num) as i64)
}

/// The upright box in parts of the frame around the four corners of `sample` drawn with `mapping`:
/// left, top, width and height.
pub fn frame_box(mapping: &FrameMapping, sample: &TrackSample) -> [f64; 4] {
    let corners = box_corners(sample).map(|corner| mapping.to_frame(corner));
    let (mut low, mut high) = ([f64::MAX; 2], [f64::MIN; 2]);
    for [x, y] in corners {
        low = [low[0].min(x), low[1].min(y)];
        high = [high[0].max(x), high[1].max(y)];
    }
    [low[0], low[1], high[0] - low[0], high[1] - low[1]]
}

/// The corners of the box of `sample` in parts of its picture, clockwise from the top left, turned
/// by its angle (the picture taken as square, as the tracker turns boxes in pixels only roughly).
pub fn box_corners(sample: &TrackSample) -> [[f64; 2]; 4] {
    let (sine, cosine) = sample.angle.to_radians().sin_cos();
    let (hw, hh) = (sample.w / 2.0, sample.h / 2.0);
    [[-hw, -hh], [hw, -hh], [hw, hh], [-hw, hh]].map(|[x, y]| [sample.cx + cosine * x - sine * y, sample.cy + sine * x + cosine * y])
}

impl EditorState {
    /// The motion tracks of source file `media`.
    pub fn motion_tracks_of(&self, media: &str) -> Vec<&MotionTrack> {
        self.project().motion_tracks.iter().filter(|track| track.media_path == media).collect()
    }

    /// The motion track the inspector works on for clip `key`: the chosen one when it is of the
    /// clip's file, else the first track of that file.
    pub fn motion_track_for(&self, key: ClipKey) -> Option<&MotionTrack> {
        let clip = self.project().tracks.get(key.track)?.clips.get(key.clip)?;
        let tracks = self.motion_tracks_of(&clip.source);
        tracks.iter().find(|track| Some(track.id) == self.ui.motion_track).or(tracks.first()).copied()
    }

    /// Chooses motion track `id` for the inspector, the preview and the timeline.
    pub fn choose_motion_track(&mut self, id: Option<u32>) {
        self.ui.motion_track = id;
    }

    /// How clip `key`, a picture of the shape of its source, lies in the frame at the playhead,
    /// and the source time it shows there; `None` when the playhead is off the clip.
    pub fn clip_frame_now(&self, key: ClipKey) -> Option<(FrameMapping, Time)> {
        self.clip_frame_now_for(key, None)
    }

    /// [`Self::clip_frame_now`] for drawing or editing the box and path of motion track
    /// `motion_track`: without the clip's lock when it locks to that track (see
    /// [`engine::Project::clip_frame_mapping_for`]).
    pub fn clip_frame_now_for(&self, key: ClipKey, motion_track: Option<u32>) -> Option<(FrameMapping, Time)> {
        let clip = self.project().tracks.get(key.track)?.clips.get(key.clip)?;
        let at = self.playhead();
        if at < clip.start || at >= clip.start.saturating_add(clip.duration) {
            return None;
        }
        let mapping = self.project().clip_frame_mapping_for(key.track, key.clip, at, self.clip_aspect(clip), motion_track)?;
        Some((mapping, clip.source_time_at(at.saturating_sub(clip.start)).max(Time::ZERO)))
    }

    /// How far, in source time, a sample may be from the frame clip `key` shows to count as on it:
    /// half a frame of the source, as the tracker writes one sample per source frame; half an
    /// output frame at the clip's speed until a probe says the source's rate.
    pub fn sample_tolerance(&self, key: ClipKey) -> Time {
        let Some(clip) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return Time::ZERO };
        let frame = match self.source_rate(&clip.source) {
            Some((num, den)) => f64::from(den) * engine::TICKS_PER_SECOND as f64 / f64::from(num),
            None => self.project().output.frame_rate.frame_duration().ticks() as f64 * clip.speed.abs(),
        };
        Time::from_ticks((frame / 2.0).ceil() as i64)
    }

    /// The box of `track` the player shows for clip `key` at the playhead, which a drag turns
    /// into a sample placed by hand, and the sample on that frame when there is one. On a sample's
    /// frame the box is that sample, at its source time, so an edit replaces it rather than adding
    /// one beside it; between samples it is the path at the playhead, put on the nearest frame of
    /// the source when its rate is known.
    pub fn box_at_playhead(&self, key: ClipKey, track: &MotionTrack) -> Option<(TrackSample, Option<TrackSample>)> {
        let (_, now) = self.clip_frame_now(key)?;
        let on_frame = sample_near(track, now, self.sample_tolerance(key));
        if let Some(sample) = on_frame {
            return Some((sample, on_frame));
        }
        let clip = self.project().tracks.get(key.track)?.clips.get(key.clip)?;
        let time = self.source_rate(&clip.source).map_or(now, |rate| snap_to_frame(now, rate));
        let point = track.point_at(time, 0.0)?;
        let quad = if track.is_planar() { track.quad_at(time, 0.0) } else { None };
        Some((TrackSample { source_time: time, cx: point.cx, cy: point.cy, w: point.w, h: point.h, angle: point.angle, confidence: 1.0, manual: false, quad }, None))
    }

    /// Starts or ends drawing a box for a new motion track on the preview, for video clip `key`.
    pub fn toggle_track_pick(&mut self, key: ClipKey) {
        let source = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).map(|clip| clip.source.clone());
        self.ui.tracking_pick = match source {
            Some(source) if self.track_pick() != Some(key) => Some((key, source)),
            _ => None,
        };
    }

    /// Ends drawing a box for a new motion track, with the drag of a box going, if any (Escape).
    /// Whether a pick was going.
    pub fn cancel_track_pick(&mut self) -> bool {
        if self.ui.tracking_pick.take().is_none() {
            return false;
        }
        self.ui.tracking_pick_round = self.ui.tracking_pick_round.wrapping_add(1);
        true
    }

    /// The clip whose preview takes a box for a new motion track: only while it is the selected
    /// clip and still shows the file it showed when the drawing started, so a delete, an undo or
    /// another selection that left the key on another clip does not draw on that one.
    pub fn track_pick(&self) -> Option<ClipKey> {
        let (key, source) = self.ui.tracking_pick.as_ref()?;
        let clip = self.project().tracks.get(key.track)?.clips.get(key.clip)?;
        (self.selection() == Some(*key) && clip.source == *source).then_some(*key)
    }

    /// Starts a motion track of the file of clip `key` with the box drawn on the preview: `rect`
    /// is left, top, width and height in parts of the frame, at the playhead. The sample is the
    /// upright box of the picture around all four corners of `rect`, so a turned clip still has
    /// the whole drawn box inside it. One undo step; the new track is chosen and the drawing ends.
    pub fn create_motion_track(&mut self, key: ClipKey, rect: [f64; 4]) -> Option<u32> {
        self.ui.tracking_pick = None;
        let (mapping, source_time) = self.clip_frame_now(key)?;
        let clip = self.project().tracks.get(key.track)?.clips.get(key.clip)?.clone();
        if rect[2].abs() < MIN_BOX_SIDE || rect[3].abs() < MIN_BOX_SIDE {
            return None;
        }
        let (right, bottom) = (rect[0] + rect[2], rect[1] + rect[3]);
        let corners = [[rect[0], rect[1]], [right, rect[1]], [right, bottom], [rect[0], bottom]].map(|corner| mapping.to_source(corner));
        let (mut low, mut high) = ([f64::MAX; 2], [f64::MIN; 2]);
        for [x, y] in corners {
            low = [low[0].min(x), low[1].min(y)];
            high = [high[0].max(x), high[1].max(y)];
        }
        let (a, b) = (low, high);
        let (w, h) = (b[0] - a[0], b[1] - a[1]);
        if w < MIN_BOX_SIDE || h < MIN_BOX_SIDE {
            return None;
        }
        let id = self.project().next_motion_track_id();
        let sample = TrackSample { source_time, cx: (a[0] + b[0]) / 2.0, cy: (a[1] + b[1]) / 2.0, w, h, angle: 0.0, confidence: 1.0, manual: true, quad: None };
        let track = MotionTrack { id, media_path: clip.source.clone(), aspect: self.clip_aspect(&clip), samples: vec![sample] };
        if self.run(|project| engine::set_motion_track(project, track)) {
            self.ui.motion_track = Some(id);
            self.status_info(Message::TrackCreated);
            Some(id)
        } else {
            None
        }
    }

    /// Moves or resizes the box of motion track `id` while it is dragged: the box becomes a sample
    /// placed by hand at its source time, which the tracker starts from and stops before. The
    /// release makes it one undo step.
    pub fn live_track_sample(&mut self, id: u32, sample: TrackSample) {
        let sample = TrackSample { confidence: 1.0, manual: true, ..sample };
        self.live_edit(|project| engine::set_track_sample(project, id, sample));
    }

    /// Makes motion track `id` a planar (four corner) track, or a track of boxes again, for clip
    /// `key`. Planar: the box at the playhead becomes a sample placed by hand with its corners, so
    /// a run from it tracks in perspective. Boxes: every sample loses its corners. One undo step.
    pub fn set_track_planar(&mut self, key: ClipKey, id: u32, planar: bool) {
        let Some(track) = self.project().motion_track(id).cloned() else { return };
        if planar {
            let Some((shown, _)) = self.box_at_playhead(key, &track) else { return };
            let quad = shown.corners(track.aspect);
            let sample = TrackSample::from_quad(shown.source_time, quad, track.aspect, 1.0, true);
            self.run(|project| engine::set_track_sample(project, id, sample));
        } else {
            let mut boxes = track;
            boxes.samples.iter_mut().for_each(|sample| sample.quad = None);
            self.run(|project| engine::set_motion_track(project, boxes));
        }
    }

    /// Shows `quad` (parts of the source picture) as the corners of motion track `id` at
    /// `source_time`, a sample placed by hand, while a drag goes on (see [`Self::commit_live`]).
    pub fn live_track_quad(&mut self, id: u32, source_time: Time, quad: engine::Quad) {
        let Some(aspect) = self.project().motion_track(id).map(|track| track.aspect) else { return };
        self.live_track_sample(id, TrackSample::from_quad(source_time, quad, aspect, 1.0, true));
    }

    /// Removes motion track `id`; what followed it stays where it was placed. One undo step.
    pub fn delete_motion_track(&mut self, id: u32) {
        if self.run(|project| engine::remove_motion_track(project, id)) && self.ui.motion_track == Some(id) {
            self.ui.motion_track = None;
        }
    }

    /// Asks the window for a tracker run of motion track `id` for clip `key`: from the sample on
    /// the frame at the playhead when there is one, else from the default sample.
    pub fn ask_track_run(&mut self, key: ClipKey, id: u32, direction: Direction, limit: Option<usize>) {
        let tolerance = self.sample_tolerance(key);
        let from = self.clip_frame_now(key).and_then(|(_, at)| self.project().motion_track(id).and_then(|track| sample_near(track, at, tolerance))).map(|sample| sample.source_time);
        self.ui.track_run_request = Some(TrackRunAsk { track_id: id, from, direction, limit });
    }

    /// Starts the run `ask` reading `decode_path`; a refusal goes to the status line.
    pub fn run_track_ask(&mut self, decode_path: std::path::PathBuf, ask: TrackRunAsk) {
        let started = self.track_request(ask.track_id, ask.from, ask.direction, true, self.ui.track_mode).and_then(|request| self.start_track_request(decode_path, crate::tracking::RunRequest { limit: ask.limit, ..request }));
        if let Err(message) = started {
            self.report_error(Message::TrackingFailed(message));
        }
    }

    /// The progress of the run on motion track `id`, 0 to 1, while one goes.
    pub fn track_progress(&self, id: u32) -> Option<f32> {
        let job = self.tracking.running_on(id)?;
        match self.tracking.status(job) {
            Some(crate::tracking::RunState::Running { done, total }) => Some(if total == 0 { 0.0 } else { done as f32 / total as f32 }),
            _ => Some(1.0),
        }
    }

    /// Stops the run on motion track `id`; it writes what it made.
    pub fn stop_track_run(&mut self, id: u32) {
        if let Some(job) = self.tracking.running_on(id) {
            self.tracking.cancel(job);
        }
    }

    /// Moves the smoothing slider of motion track `id`, for everything that follows it.
    pub fn live_track_smooth(&mut self, id: u32, smooth: f64) {
        self.live_edit(|project| engine::set_track_smooth(project, id, smooth));
    }

    /// Makes overlay `key` follow motion track `track_id`, seen through the clip of its file at
    /// the playhead (or the nearest), anchored at the playhead: position only by default. One undo
    /// step.
    pub fn follow_motion_track(&mut self, key: ClipKey, track_id: u32) {
        let Some(media) = self.project().motion_track(track_id).map(|track| track.media_path.clone()) else { return };
        let at = self.playhead();
        let Some(reference) = reference_clip(self.project(), &media, at, Some(key)) else {
            self.status_info(Message::FollowNoReference);
            return;
        };
        let Some(follow) = follow_at(self.project(), track_id, reference, at, false, false) else { return };
        self.run(|project| engine::set_follow(project, key, Some(follow)));
    }

    /// Turns following the scale or the turn of the object on or off for overlay `key`.
    pub fn set_follow_options(&mut self, key: ClipKey, follow_scale: bool, follow_rotation: bool) {
        let Some(follow) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).and_then(|clip| clip.follow) else { return };
        let link = TrackLink { follow_scale, follow_rotation, ..follow.link };
        self.run(|project| engine::set_follow(project, key, Some(Follow { link, ..follow })));
    }

    /// Pins overlay `key` to its track's four corners in perspective, or not. One undo step.
    pub fn set_follow_perspective(&mut self, key: ClipKey, perspective: bool) {
        let Some(follow) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).and_then(|clip| clip.follow) else { return };
        self.run(|project| engine::set_follow(project, key, Some(Follow { perspective, ..follow })));
    }

    /// Stops overlay `key` following its track.
    pub fn clear_follow(&mut self, key: ClipKey) {
        self.run(|project| engine::set_follow(project, key, None));
    }

    /// Locks the frame of video clip `key` to motion track `track_id`, anchored at the playhead
    /// (the clip's first frame when the playhead is off it): position only, with the smoothing the
    /// track's followers have. One undo step.
    pub fn lock_to_track(&mut self, key: ClipKey, track_id: u32) {
        let Some(clip) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return };
        let last = clip.start.saturating_add(clip.duration).saturating_sub(Time::from_ticks(1)).max(clip.start);
        let time = self.playhead().clamp(clip.start, last);
        let mut lock = TrackLock::new(track_id, clip.source_time_at(time.saturating_sub(clip.start)).max(Time::ZERO));
        lock.link.smooth = track_smooth(self.project(), track_id);
        self.run(|project| engine::set_lock(project, key, Some(lock)));
    }

    /// Changes the lock of video clip `key` with `change`, as one undo step.
    pub fn set_lock_options(&mut self, key: ClipKey, change: impl FnOnce(&mut TrackLock)) {
        let Some(mut lock) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).and_then(|clip| clip.lock) else { return };
        change(&mut lock);
        self.run(|project| engine::set_lock(project, key, Some(lock)));
    }

    /// Moves the extra zoom of the lock of video clip `key` while a slider is dragged.
    pub fn live_lock_zoom(&mut self, key: ClipKey, zoom: f64) {
        let Some(mut lock) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).and_then(|clip| clip.lock) else { return };
        lock.zoom = zoom.clamp(engine::MIN_LOCK_ZOOM, engine::MAX_LOCK_ZOOM);
        self.live_edit(|project| engine::set_lock(project, key, Some(lock)));
    }

    /// Ends the lock of video clip `key`.
    pub fn clear_lock(&mut self, key: ClipKey) {
        self.run(|project| engine::set_lock(project, key, None));
    }

    /// Turns the lock of video clip `key` into keyframes, see [`engine::bake_lock`].
    pub fn bake_lock(&mut self, key: ClipKey) {
        let Some(aspect) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).map(|clip| self.clip_aspect(clip)) else { return };
        if self.run(|project| engine::bake_lock(project, key, aspect)) {
            self.status_info(Message::LockBaked);
        }
    }

    /// Turns the follow of overlay `key` into keyframes, see [`engine::bake_follow`].

    pub fn bake_follow(&mut self, key: ClipKey) {
        let Some(aspect) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).map(|clip| self.clip_aspect(clip)) else { return };
        if self.run(|project| engine::bake_follow(project, key, aspect)) {
            self.status_info(Message::FollowBaked);
        }
    }

    /// Adds a censor region over the box of motion track `track_id` at the playhead, following it,
    /// for as long as the clip of the track's file that shows it, as one undo step, and selects it.
    pub fn add_censor_following(&mut self, track_id: u32) {
        if self.edits_blocked() {
            return;
        }
        let Some(track) = self.project().motion_track(track_id).cloned() else { return };
        let at = self.playhead();
        let Some(reference) = reference_clip(self.project(), &track.media_path, at, None) else {
            self.status_info(Message::FollowNoReference);
            return;
        };
        let Some(clip) = self.project().tracks.get(reference.track).and_then(|lane| lane.clips.get(reference.clip)).cloned() else { return };
        let last = clip.start.saturating_add(clip.duration).saturating_sub(Time::from_ticks(1)).max(clip.start);
        let time = at.clamp(clip.start, last);
        let source_time = clip.source_time_at(time.saturating_sub(clip.start)).max(Time::ZERO);
        let (Some(point), Some(mapping)) = (track.point_at(source_time, 0.0), self.project().clip_frame_mapping(reference.track, reference.clip, time, track.aspect)) else { return };
        let sample = TrackSample { source_time, cx: point.cx, cy: point.cy, w: point.w, h: point.h, angle: point.angle, confidence: 1.0, manual: false, quad: None };
        let [x, y, w, h] = frame_box(&mapping, &sample);
        let (width, height) = self.output_pixels();
        let range = engine::CENSOR_ASPECT_RANGE;
        let aspect = (w * f64::from(width) / (h * f64::from(height)).max(1.0e-9)).clamp(*range.start(), *range.end());
        let censor = engine::CensorContent { aspect, ..engine::CensorContent::default() };
        let placement = engine::ClipPlacement { x, y, width: w.clamp(engine::MIN_PLACEMENT_WIDTH, engine::MAX_PLACEMENT_WIDTH) };
        let (start, duration) = (clip.start, clip.duration);
        let choice = crate::media::choose_track(self.project(), engine::TrackKind::Sticker, duration, start);
        let lane = match choice {
            crate::media::TrackChoice::Existing(index) => index,
            crate::media::TrackChoice::New => self.project().tracks.len(),
        };
        let added = self.run(|project| {
            let with_track = if choice == crate::media::TrackChoice::New { engine::add_track(project, engine::TrackKind::Sticker)? } else { project.clone() };
            let next = engine::add_censor_clip(&with_track, lane, start, duration, censor, Some(placement))?;
            let key = engine::clip_at(&next, lane, start).ok_or(EditError::TrackNotFound { track: lane })?;
            let follow = follow_at(&next, track_id, reference, time, false, false).ok_or(EditError::MotionTrackNotFound { id: track_id })?;
            engine::set_follow(&next, key, Some(follow))
        });
        if added {
            self.select(engine::clip_at(self.project(), lane, start));
            self.status_info(Message::CensorAdded);
        }
    }

    /// Limits effect `effect` of the selected clip to `region`, or gives it the whole picture.
    pub fn set_effect_region(&mut self, effect: usize, region: Option<EffectRegion>) {
        let Some(key) = self.selection() else { return };
        self.run(|project| engine::set_effect_region(project, key, effect, region));
    }

    /// Moves a slider of the region of effect `effect` of the selected clip.
    pub fn live_effect_region(&mut self, effect: usize, region: EffectRegion) {
        let Some(key) = self.selection() else { return };
        self.live_edit(|project| engine::set_effect_region(project, key, effect, Some(region)));
    }

    /// Makes the region of effect `effect` of the selected clip move with motion track `track_id`
    /// from the frame at the playhead (the clip's first frame when the playhead is off it), or
    /// stay put with `None`.
    pub fn link_effect_region(&mut self, effect: usize, track_id: Option<u32>) {
        let Some(key) = self.selection() else { return };
        let Some(clip) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return };
        let Some(mut region) = clip.effects.get(effect).and_then(|effect| effect.region) else { return };
        region.link = track_id.map(|track_id| {
            let last = clip.start.saturating_add(clip.duration).saturating_sub(Time::from_ticks(1)).max(clip.start);
            let time = self.playhead().clamp(clip.start, last);
            let anchor_source_time = clip.source_time_at(time.saturating_sub(clip.start)).max(Time::ZERO);
            TrackLink { track_id, anchor_source_time, follow_scale: false, follow_rotation: false, smooth: track_smooth(self.project(), track_id) }
        });
        self.run(|project| engine::set_effect_region(project, key, effect, Some(region)));
    }

    fn status_info(&mut self, message: Message) {
        self.report_info(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{ClipPlacement, TrackKind, TICKS_PER_SECOND};

    const SECOND: i64 = TICKS_PER_SECOND;

    fn sample(seconds: i64, cx: f64) -> TrackSample {
        TrackSample { source_time: Time::from_ticks(seconds * SECOND), cx, cy: 0.5, w: 0.1, h: 0.1, angle: 0.0, confidence: 1.0, manual: true, quad: None }
    }

    /// A video clip of `a.mp4` over 4 s on track 0, a motion track of it going from the left
    /// quarter to the right quarter over 2 s, and a sticker on track 1 over the same 4 s.
    fn rigged() -> EditorState {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, std::path::Path::new("a.mp4"));
        let mut project = state.project().clone();
        project.tracks[0].clips[0].duration = Time::from_ticks(4 * SECOND);
        project.motion_tracks.push(MotionTrack { id: 1, media_path: "a.mp4".into(), aspect: 16.0 / 9.0, samples: vec![sample(0, 0.25), sample(2, 0.75)] });
        let mut sticker = engine::censor_clip(Time::ZERO, Time::from_ticks(4 * SECOND), engine::CensorContent::default(), Some(ClipPlacement { x: 0.1, y: 0.1, width: 0.2 }));
        sticker.censor = Some(engine::CensorContent::default());
        let mut stickers = engine::Track { kind: TrackKind::Sticker, ..project.tracks[0].clone() };
        stickers.clips = vec![sticker];
        stickers.transitions.clear();
        project.tracks.push(stickers);
        state.replace_project(project, None);
        state
    }

    fn overlay_x(state: &EditorState, at: Time) -> f64 {
        let project = state.project();
        let clip = &project.tracks[1].clips[0];
        let mut transform = clip.transform_at(at.saturating_sub(clip.start));
        if let Some(follow) = &clip.follow {
            engine::follow_transform(project, follow, &mut transform, at);
        }
        transform.rect(16.0 / 9.0, clip.censor.unwrap().aspect).x
    }

    #[test]
    fn a_track_turns_planar_at_the_playhead_and_back_and_a_follow_pins_in_perspective() {
        let mut state = rigged();
        let video = ClipKey { track: 0, clip: 0 };
        state.set_playhead(Time::from_ticks(SECOND));
        state.set_track_planar(video, 1, true);
        let track = state.project().motion_track(1).unwrap().clone();
        assert!(track.is_planar());
        let planar = track.samples.iter().find(|s| s.quad.is_some()).expect("a sample with corners");
        assert!(planar.manual && (planar.cx - 0.5).abs() < 1e-9, "the box at the playhead, half way: {planar:?}");
        let (shown, _) = state.box_at_playhead(video, &track).unwrap();
        assert_eq!(shown.quad, planar.quad);
        // Between a box and a quad the corners are interpolated.
        state.set_playhead(Time::from_ticks(SECOND / 2));
        assert!(state.box_at_playhead(video, &track).unwrap().0.quad.is_some());
        let moved = planar.quad.unwrap().map(|[x, y]| [x + 0.01, y]);
        state.live_track_quad(1, planar.source_time, moved);
        state.commit_live();
        assert_eq!(state.project().motion_track(1).unwrap().samples.iter().find(|s| s.source_time == planar.source_time).unwrap().quad, Some(moved));
        let overlay = ClipKey { track: 1, clip: 0 };
        state.follow_motion_track(overlay, 1);
        state.set_follow_perspective(overlay, true);
        assert!(state.project().tracks[1].clips[0].follow.unwrap().perspective);
        state.set_track_planar(video, 1, false);
        assert!(!state.project().motion_track(1).unwrap().is_planar());
        state.undo();
        assert!(state.project().motion_track(1).unwrap().is_planar(), "one undo step");
    }

    #[test]
    fn locking_a_clip_anchors_at_the_playhead_and_its_options_bake_and_clear() {
        let mut state = rigged();
        let key = ClipKey { track: 0, clip: 0 };
        state.set_playhead(Time::from_ticks(SECOND));
        state.lock_to_track(key, 1);
        let lock = state.project().tracks[0].clips[0].lock.expect("locked");
        assert_eq!((lock.link.track_id, lock.link.anchor_source_time, lock.link.follow_scale, lock.link.follow_rotation, lock.zoom), (1, Time::from_ticks(SECOND), false, false, 1.0));
        state.set_lock_options(key, |lock| lock.link.follow_rotation = true);
        assert!(state.project().tracks[0].clips[0].lock.unwrap().link.follow_rotation);
        state.live_lock_zoom(key, 20.0);
        state.commit_live();
        assert_eq!(state.project().tracks[0].clips[0].lock.unwrap().zoom, engine::MAX_LOCK_ZOOM, "clamped");
        state.bake_lock(key);
        let clip = &state.project().tracks[0].clips[0];
        assert!(clip.lock.is_none() && clip.motion.has_keyframes(engine::ClipProperty::X));
        state.undo();
        assert!(state.project().tracks[0].clips[0].lock.is_some(), "baking is one undo step");
        state.clear_lock(key);
        assert!(state.project().tracks[0].clips[0].lock.is_none());
    }

    #[test]
    fn attaching_at_the_playhead_keeps_the_overlay_there_then_moves_it_with_the_object() {

        let mut state = rigged();
        let key = ClipKey { track: 1, clip: 0 };
        let at = Time::from_ticks(SECOND);
        state.set_playhead(at);
        let before = overlay_x(&state, at);
        state.follow_motion_track(key, 1);
        let follow = state.project().tracks[1].clips[0].follow.expect("follows");
        assert_eq!((follow.ref_track, follow.ref_clip, follow.offset), (0, 0, [0.0, 0.0]));
        assert_eq!(follow.link.anchor_source_time, at, "anchored at the playhead");
        assert!(!follow.link.follow_scale && !follow.link.follow_rotation, "position only by default");
        assert!((overlay_x(&state, at) - before).abs() < 1e-9, "no jump at the playhead");
        // Half a second later the object went an eighth of the frame right.
        assert!((overlay_x(&state, Time::from_ticks(3 * SECOND / 2)) - before - 0.125).abs() < 1e-6);
        state.set_follow_options(key, true, true);
        let follow = state.project().tracks[1].clips[0].follow.unwrap();
        assert!(follow.link.follow_scale && follow.link.follow_rotation);
        state.undo();
        state.undo();
        assert!(state.project().tracks[1].clips[0].follow.is_none(), "each step undoes");
    }

    #[test]
    fn baking_keeps_where_the_overlay_shows_and_undoes_in_one_step() {
        let mut state = rigged();
        let key = ClipKey { track: 1, clip: 0 };
        state.set_playhead(Time::ZERO);
        state.follow_motion_track(key, 1);
        let at = Time::from_ticks(SECOND);
        let shown = overlay_x(&state, at);
        state.bake_follow(key);
        let clip = &state.project().tracks[1].clips[0];
        assert!(clip.follow.is_none() && clip.motion.has_keyframes(engine::ClipProperty::X));
        assert!((overlay_x(&state, at) - shown).abs() < 1e-6);
        state.undo();
        assert!(state.project().tracks[1].clips[0].follow.is_some());
    }

    #[test]
    fn a_censor_following_the_object_covers_its_box() {
        let mut state = rigged();
        state.set_playhead(Time::ZERO);
        state.add_censor_following(1);
        let key = state.selection().expect("the new region is selected");
        let clip = &state.project().tracks[key.track].clips[key.clip];
        assert!(clip.is_censor());
        assert_eq!(clip.follow.expect("follows").link.track_id, 1);
        let placement = clip.placement.expect("placed");
        // The clip fills the frame, so the box is 0.1 wide around 0.25.
        assert!((placement.x - 0.2).abs() < 1e-6 && (placement.width - 0.1).abs() < 1e-6, "{placement:?}");
        state.undo();
        assert_eq!(state.project().tracks.iter().map(|track| track.clips.len()).sum::<usize>(), 2, "one undo step");
    }

    #[test]
    fn a_drawn_box_starts_a_track_with_a_sample_placed_by_hand() {
        let mut state = rigged();
        let key = ClipKey { track: 0, clip: 0 };
        state.set_playhead(Time::from_ticks(SECOND / 2));
        state.select(Some(key));
        state.toggle_track_pick(key);
        assert_eq!(state.track_pick(), Some(key));
        assert!(state.create_motion_track(key, [0.4, 0.4, 0.005, 0.2]).is_none(), "too thin to be a box");
        state.toggle_track_pick(key);
        let id = state.create_motion_track(key, [0.4, 0.4, 0.2, 0.2]).expect("a new track");
        assert_eq!(state.track_pick(), None);
        assert_eq!(state.motion_track_for(key).map(|track| track.id), Some(id));
        let track = state.project().motion_track(id).unwrap();
        let first = track.samples[0];
        assert!(first.manual && first.source_time == Time::from_ticks(SECOND / 2));
        assert!((first.cx - 0.5).abs() < 1e-9 && (first.w - 0.2).abs() < 1e-9, "{first:?}");
        // Editing the box on a tracked frame places a sample by hand there.
        state.live_track_sample(1, TrackSample { cx: 0.3, manual: false, confidence: 0.2, ..sample(2, 0.75) });
        state.commit_live();
        let fixed = state.project().motion_track(1).unwrap().samples[1];
        assert!(fixed.manual && fixed.confidence == 1.0 && fixed.cx == 0.3);
        state.delete_motion_track(id);
        assert!(state.project().motion_track(id).is_none());
    }

    /// [`rigged`] with a 24 fps source played on a 60 fps output.
    fn rigged_24_on_60() -> EditorState {
        let mut state = rigged();
        state.visuals.borrow_mut().set_video_for_test("a.mp4", 16.0 / 9.0, (24, 1));
        let mut project = state.project().clone();
        project.output.frame_rate = engine::FrameRate::whole(60);
        state.replace_project(project, None);
        state
    }

    #[test]
    fn the_box_on_a_tracked_frame_is_that_sample_so_an_edit_replaces_it() {
        let mut state = rigged_24_on_60();
        let key = ClipKey { track: 0, clip: 0 };
        assert_eq!(state.sample_tolerance(key), Time::from_ticks((SECOND + 47) / 48), "half a frame of the source");
        let track = state.project().motion_track(1).unwrap().clone();
        // A few ticks past the sample, and a whole output frame past it: still its source frame.
        for off in [3, SECOND / 60] {
            state.set_playhead(Time::from_ticks(2 * SECOND + off));
            let (shown, on_frame) = state.box_at_playhead(key, &track).expect("a box");
            assert_eq!(on_frame, Some(track.samples[1]), "{off} ticks off");
            assert_eq!(shown.source_time, Time::from_ticks(2 * SECOND), "{off} ticks off");
        }
        let (shown, _) = state.box_at_playhead(key, &track).unwrap();
        state.live_track_sample(1, TrackSample { cx: 0.6, ..shown });
        state.commit_live();
        let samples = &state.project().motion_track(1).unwrap().samples;
        assert_eq!(samples.len(), 2, "replaced, not added beside it");
        assert!(samples[1].manual && samples[1].cx == 0.6);
        // Between samples the box goes on the nearest frame of the source.
        state.set_playhead(Time::from_ticks(SECOND + 5));
        let (shown, on_frame) = state.box_at_playhead(key, &track).unwrap();
        assert_eq!((shown.source_time, on_frame), (Time::from_ticks(SECOND), None));
        assert_eq!(snap_to_frame(Time::from_ticks(SECOND / 24 * 3 + 7), (24, 1)), Time::from_ticks(SECOND / 8));
    }

    #[test]
    fn the_box_of_the_track_a_clip_locks_to_is_drawn_without_the_lock() {
        let mut state = rigged();
        let key = ClipKey { track: 0, clip: 0 };
        state.set_playhead(Time::from_ticks(2 * SECOND));
        let (plain, _) = state.clip_frame_now(key).unwrap();
        let mut project = state.project().clone();
        let link = engine::TrackLink { track_id: 1, anchor_source_time: Time::ZERO, follow_scale: true, follow_rotation: true, smooth: 0.0 };
        project.tracks[0].clips[0].lock = Some(engine::TrackLock { link, zoom: 1.5, perspective: false });
        state.replace_project(project, None);
        let (locked, _) = state.clip_frame_now(key).unwrap();
        assert_ne!(locked, plain, "the picture itself is drawn locked");
        // The box of track 1 drives the lock, so it is drawn and dragged in the clip's own place:
        // a drag then moves the sample by just as much, not through the lock it changes.
        let (for_box, _) = state.clip_frame_now_for(key, Some(1)).unwrap();
        assert_eq!(for_box, plain);
        assert_eq!(state.clip_frame_now_for(key, Some(2)).unwrap().0, locked, "another track keeps the lock");
    }

    #[test]
    fn a_box_drawn_on_a_turned_clip_covers_all_it_was_drawn_around() {
        let mut state = rigged();
        let key = ClipKey { track: 0, clip: 0 };
        let mut project = state.project().clone();
        project.tracks[0].clips[0].motion.rotation = 30.0;
        state.replace_project(project, None);
        state.set_playhead(Time::from_ticks(SECOND / 2));
        let drawn = [0.4, 0.4, 0.2, 0.1];
        let id = state.create_motion_track(key, drawn).expect("a new track");
        let sample = state.project().motion_track(id).unwrap().samples[0];
        let (mapping, _) = state.clip_frame_now(key).unwrap();
        let (right, bottom) = (drawn[0] + drawn[2], drawn[1] + drawn[3]);
        for corner in [[drawn[0], drawn[1]], [right, drawn[1]], [right, bottom], [drawn[0], bottom]] {
            let [x, y] = mapping.to_source(corner);
            let inside = (x - sample.cx).abs() <= sample.w / 2.0 + 1e-9 && (y - sample.cy).abs() <= sample.h / 2.0 + 1e-9;
            assert!(inside, "corner {corner:?} of the drawn box is outside {sample:?}");
        }
        let centre = mapping.to_source([0.5, 0.45]);
        assert!((sample.cx - centre[0]).abs() < 1e-9 && (sample.cy - centre[1]).abs() < 1e-9, "{sample:?}");
    }

    #[test]
    fn picking_ends_when_the_clip_is_no_longer_the_one_selected() {
        let mut state = rigged();
        let key = ClipKey { track: 0, clip: 0 };
        let other = ClipKey { track: 1, clip: 0 };
        state.select(Some(key));
        state.toggle_track_pick(key);
        assert_eq!(state.track_pick(), Some(key));
        state.select(Some(other));
        state.select(Some(key));
        assert_eq!(state.track_pick(), None, "another selection ends it, and coming back does not revive it");
        state.toggle_track_pick(key);
        state.delete_selected();
        assert_eq!(state.track_pick(), None, "the clip went");
        state.undo();
        state.select(Some(key));
        assert_eq!(state.track_pick(), None, "undo does not bring it back");
        state.toggle_track_pick(key);
        let mut project = state.project().clone();
        project.tracks[0].clips[0].source = "b.mp4".into();
        state.replace_project(project, None);
        state.select(Some(key));
        assert_eq!(state.track_pick(), None, "another project");
        // A clip of another file landing on the key does not take the drawing.
        state.toggle_track_pick(key);
        state.ui.tracking_pick = Some((key, "a.mp4".into()));
        assert_eq!(state.track_pick(), None);
    }

    #[test]
    fn a_run_starts_from_the_sample_on_the_frame_at_the_playhead() {
        let mut state = rigged();
        let key = ClipKey { track: 0, clip: 0 };
        state.set_playhead(Time::from_ticks(2 * SECOND));
        state.ask_track_run(key, 1, Direction::Forward, Some(1));
        assert_eq!(state.ui.track_run_request, Some(TrackRunAsk { track_id: 1, from: Some(Time::from_ticks(2 * SECOND)), direction: Direction::Forward, limit: Some(1) }));
        state.set_playhead(Time::from_ticks(SECOND));
        state.ask_track_run(key, 1, Direction::Backward, None);
        assert_eq!(state.ui.track_run_request.unwrap().from, None, "no sample there: the default one");
    }

    #[test]
    fn smoothing_and_a_region_link_reach_the_project() {
        let mut state = rigged();
        let key = ClipKey { track: 1, clip: 0 };
        state.follow_motion_track(key, 1);
        state.live_track_smooth(1, 0.5);
        state.commit_live();
        assert_eq!(track_smooth(state.project(), 1), 0.5);
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        let mut project = state.project().clone();
        project.tracks[0].clips[0].effects.push(engine::Effect { package: "pixelate".into(), params: Default::default(), keyframes: Default::default(), region: None });
        state.replace_project(project, None);
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        state.set_effect_region(0, Some(EffectRegion { shape: engine::RegionShape::Ellipse, cx: 0.25, cy: 0.5, w: 0.2, h: 0.2, angle: 0.0, feather: 0.2, link: None, perspective: false, corners: None }));
        state.link_effect_region(0, Some(1));
        let region = state.project().tracks[0].clips[0].effects[0].region.expect("a region");
        assert_eq!(region.link.map(|link| link.track_id), Some(1));
        state.link_effect_region(0, None);
        assert!(state.project().tracks[0].clips[0].effects[0].region.unwrap().link.is_none());
    }
}
