//! The automatic face censor: a face detector (YuNet, [`gpufx_core::nn::FaceDetector`], the model
//! [`TrackingModel::FaceDetector`] of [`crate::tracking_models`]) runs in the background over the
//! part of the file a video clip shows (its proxy when it has one), frames read as the tracker
//! reads them ([`crate::tracking::FrameSource`]). When it ends, the faces are joined into face
//! tracks ([`engine::faces::face_tracks`]), and each track becomes a motion track of the file and a
//! censor region (blur, ellipse, soft edge by default) that follows it, all as one undo step
//! ([`EditorState::poll_face_censor`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use engine::faces::{face_tracks, FaceDetection, FaceFrame, FaceTrackOptions};
use engine::{CensorContent, CensorMode, ClipKey, EditError, MotionTrack, Project, RegionShape, Time, TrackSample, TICKS_PER_SECOND};

use crate::editor::EditorState;
use crate::i18n::Message;
use crate::tracking::{FrameSource, TrackFrame};
use crate::tracking_models::TrackingModel;

/// The most detections per second of source: frames between them take the motion track's
/// interpolation (provisional).
pub const DETECTIONS_PER_SECOND: f64 = 10.0;
/// A censor region is this many times the face box, around its middle, so it hides the hair line
/// and the chin as well (provisional).
pub const CENSOR_MARGIN: f64 = 1.3;
/// The smoothing of the face tracks the censor regions follow, in seconds (provisional).
pub const CENSOR_SMOOTH: f64 = 0.15;
/// The frame rate assumed for a file that states none.
const FALLBACK_RATE: (i32, i32) = (30, 1);
/// The most detections of one job.
pub const MAX_DETECTIONS: usize = 50_000;

/// What the censor regions look like.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceCensorStyle {
    pub shape: RegionShape,
    pub feather: f64,
    pub mode: CensorMode,
}

impl Default for FaceCensorStyle {
    fn default() -> Self {
        FaceCensorStyle { shape: RegionShape::Ellipse, feather: 0.3, mode: CensorMode::default() }
    }
}

/// What a face censor job is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceRequest {
    /// The video clip whose faces are hidden; checked again before anything is written.
    pub clip: ClipKey,
    pub media_path: String,
    /// The clip as it was when the job started: a job whose clip differs from it when it ends
    /// writes nothing.
    pub shown: ClipShown,
    /// The source span the clip shows: from, and up to (not included).
    pub from: Time,
    pub to: Time,
    pub style: FaceCensorStyle,
    pub tracks: FaceTrackOptions,
    /// The face detector model.
    pub model: PathBuf,
}

/// What of a clip decides where its faces land on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipShown {
    pub source: String,
    pub source_in: Time,
    pub source_span: Time,
    pub start: Time,
    pub duration: Time,
    pub speed: f64,
    pub reverse: bool,
}

impl ClipShown {
    pub fn of(clip: &engine::Clip) -> Self {
        ClipShown { source: clip.source.clone(), source_in: clip.source_in, source_span: clip.source_span(), start: clip.start, duration: clip.duration, speed: clip.speed, reverse: clip.reverse }
    }
}

/// Finds the faces of a frame.
pub trait FaceFinder {
    /// The faces of `frame`, in parts of its width and height.
    fn find(&mut self, frame: &TrackFrame) -> Result<Vec<FaceDetection>, String>;
}

/// YuNet on the tracker's GPU.
pub struct YuNetFinder {
    detector: gpufx_core::nn::FaceDetector,
}

impl YuNetFinder {
    pub fn open(model: &Path) -> Result<Self, String> {
        let detector = gpufx_core::nn::FaceDetector::open(crate::tracking::tracking_gpu()?, model).map_err(|error| {
            crate::tracking::forget_tracking_gpu(&error);
            format!("the face detector model failed to load: {error}")
        })?;
        Ok(Self { detector })
    }
}

impl FaceFinder for YuNetFinder {
    fn find(&mut self, frame: &TrackFrame) -> Result<Vec<FaceDetection>, String> {
        let (width, height) = (frame.gray.width, frame.gray.height);
        let rgba = frame.rgba();
        let found = self.detector.detect(&crate::tracking::rgba_image(&rgba, width, height)).map_err(|error| {
            crate::tracking::forget_tracking_gpu(&error);
            format!("the face detector failed: {error}")
        })?;
        let (w, h) = (width.max(1) as f64, height.max(1) as f64);
        Ok(found
            .into_iter()
            .map(|b| FaceDetection { cx: (f64::from(b.x) + f64::from(b.w) / 2.0) / w, cy: (f64::from(b.y) + f64::from(b.h) / 2.0) / h, w: f64::from(b.w) / w, h: f64::from(b.h) / h, score: f64::from(b.score) })
            .collect())
    }
}

/// The source times a job looks at: frames of `rate` from `from` up to `to`, at most
/// [`DETECTIONS_PER_SECOND`] a second, and the step between them. A span longer than
/// [`MAX_DETECTIONS`] such times takes a wider step, so the times still cover all of it.
pub fn detection_times(from: Time, to: Time, rate: Option<(i32, i32)>) -> (Vec<Time>, Time) {
    let (num, den) = rate.filter(|(num, den)| *num > 0 && *den > 0).unwrap_or(FALLBACK_RATE);
    let fps = f64::from(num) / f64::from(den);
    let mut every = (fps / DETECTIONS_PER_SECOND).ceil().max(1.0) as i64;
    let unit = i128::from(den) * i128::from(TICKS_PER_SECOND);
    let frame_time = |index: i64| Time::from_ticks(((i128::from(index) * unit) / i128::from(num)) as i64);
    // the first frame starting at or after `from`, and the first at or after `to`
    let first_at = |at: Time| ((i128::from(at.ticks().max(0)) * i128::from(num) + unit - 1) / unit) as i64;
    let first = first_at(from);
    let frames = (first_at(to) - first).max(0);
    every = every.max((frames + MAX_DETECTIONS as i64 - 1) / MAX_DETECTIONS as i64);
    let mut times = Vec::new();
    let mut index = first;
    while times.len() < MAX_DETECTIONS {
        let at = frame_time(index);
        if at >= to {
            break;
        }
        times.push(at);
        index += every;
    }
    (times, frame_time(every))
}

/// What a finished job found.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceResult {
    pub request: FaceRequest,
    pub frames: Vec<FaceFrame>,
    /// Width over height of the frames read.
    pub aspect: f64,
    /// The source time between two detections.
    pub step: Time,
    pub cancelled: bool,
}

/// What a finished job did, kept for an agent that asks later.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceReport {
    pub clip: ClipKey,
    /// `done`, `cancelled` (nothing written), `stale` (the clip changed while it ran, nothing
    /// written) or `failed`.
    pub outcome: &'static str,
    /// The face tracks found and written.
    pub faces: usize,
    /// The motion tracks made, one per face.
    pub track_ids: Vec<u32>,
    pub message: Option<String>,
}

/// Where a job stands.
#[derive(Debug, Clone, PartialEq)]
pub enum FaceState {
    Running { done: usize, total: usize },
    Finished(FaceResult),
    Failed(String),
}

#[derive(Default)]
struct Shared {
    done: AtomicUsize,
    total: AtomicUsize,
    cancel: AtomicBool,
    result: Mutex<Option<Result<FaceResult, String>>>,
    finished: Condvar,
}

impl Shared {
    fn result(&self) -> Option<Result<FaceResult, String>> {
        self.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

struct Job {
    clip: ClipKey,
    shared: Arc<Shared>,
}

/// The face censor jobs of this session, by job id, and the reports of finished ones (the last
/// [`MAX_REPORTS`]).
#[derive(Default)]
pub struct FaceCensorJobs {
    jobs: BTreeMap<u64, Job>,
    next_id: u64,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    pub reports: BTreeMap<u64, FaceReport>,
    /// The job started last.
    pub last_job: Option<u64>,
}

/// How many reports are kept.
pub const MAX_REPORTS: usize = 32;

impl FaceCensorJobs {
    /// Called from a job's thread when it has finished, so a window repaints.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    /// The job running on clip `clip`, if one does.
    pub fn running_on(&self, clip: ClipKey) -> Option<u64> {
        self.jobs.iter().find(|(_, job)| job.clip == clip && job.shared.result().is_none()).map(|(id, _)| *id)
    }

    /// The share done of the job running on `clip`, 0 to 1.
    pub fn progress_on(&self, clip: ClipKey) -> Option<f32> {
        let id = self.running_on(clip)?;
        let job = &self.jobs[&id];
        let total = job.shared.total.load(Ordering::Relaxed);
        Some(if total == 0 { 0.0 } else { job.shared.done.load(Ordering::Relaxed) as f32 / total as f32 })
    }

    /// Starts `request` on a new thread, reading frames from what `open` gives and finding faces
    /// with what `finder` gives. Refused while another job works on the same clip.
    pub fn start(
        &mut self,
        request: FaceRequest,
        open: impl FnOnce() -> Result<Box<dyn FrameSource>, String> + Send + 'static,
        finder: impl FnOnce() -> Result<Box<dyn FaceFinder>, String> + Send + 'static,
    ) -> Result<u64, String> {
        if let Some(job) = self.running_on(request.clip) {
            return Err(format!("the faces of track {}, clip {} are being found by job {job}", request.clip.track, request.clip.clip));
        }
        self.next_id += 1;
        let id = self.next_id;
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let waker = self.waker.clone();
        let clip = request.clip;
        std::thread::Builder::new()
            .name("face censor".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut source = open()?;
                    let mut finder = finder()?;
                    run(request, source.as_mut(), finder.as_mut(), &thread_shared)
                }))
                .unwrap_or_else(|_| Err("the face detector stopped on an internal error".to_owned()));
                *thread_shared.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
                thread_shared.finished.notify_all();
                if let Some(waker) = waker {
                    waker();
                }
            })
            .map_err(|error| error.to_string())?;
        self.jobs.insert(id, Job { clip, shared });
        self.last_job = Some(id);
        Ok(id)
    }

    /// Where job `id` stands; `None` for a job this runner no longer holds.
    pub fn status(&self, id: u64) -> Option<FaceState> {
        let job = self.jobs.get(&id)?;
        Some(match job.shared.result() {
            None => FaceState::Running { done: job.shared.done.load(Ordering::Relaxed), total: job.shared.total.load(Ordering::Relaxed) },
            Some(Ok(result)) => FaceState::Finished(result),
            Some(Err(message)) => FaceState::Failed(message),
        })
    }

    /// The detections done and to do of job `id`, also once it has finished.
    pub fn counts(&self, id: u64) -> Option<(usize, usize)> {
        let job = self.jobs.get(&id)?;
        Some((job.shared.done.load(Ordering::Relaxed), job.shared.total.load(Ordering::Relaxed)))
    }

    /// Asks job `id` to stop; it then writes nothing.
    pub fn cancel(&self, id: u64) {
        if let Some(job) = self.jobs.get(&id) {
            job.shared.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Waits until job `id` has finished.
    pub fn wait(&self, id: u64) {
        let Some(job) = self.jobs.get(&id) else { return };
        let mut result = job.shared.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while result.is_none() {
            result = job.shared.finished.wait(result).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Whether a job is still going.
    pub fn running(&self) -> bool {
        self.jobs.values().any(|job| job.shared.result().is_none())
    }

    /// The finished jobs and their outcomes, which this runner forgets.
    pub fn take_finished(&mut self) -> Vec<(u64, ClipKey, Result<FaceResult, String>)> {
        let ids: Vec<u64> = self.jobs.iter().filter(|(_, job)| job.shared.result().is_some()).map(|(id, _)| *id).collect();
        ids.into_iter().filter_map(|id| self.jobs.remove(&id).and_then(|job| job.shared.result().map(|result| (id, job.clip, result)))).collect()
    }

    fn keep_report(&mut self, id: u64, report: FaceReport) {
        self.reports.insert(id, report);
        while self.reports.len() > MAX_REPORTS {
            self.reports.pop_first();
        }
    }
}

impl Drop for FaceCensorJobs {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.shared.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Runs `request`: the faces of every detection time, counted into `shared`.
fn run(request: FaceRequest, source: &mut dyn FrameSource, finder: &mut dyn FaceFinder, shared: &Shared) -> Result<FaceResult, String> {
    let (rate, _) = source.timing();
    let (times, step) = detection_times(request.from, request.to, rate);
    shared.total.store(times.len(), Ordering::Relaxed);
    let cancelled = || shared.cancel.load(Ordering::Relaxed);
    let mut frames: Vec<FaceFrame> = Vec::with_capacity(times.len());
    let mut aspect = None;
    for (index, at) in times.iter().enumerate() {
        if cancelled() {
            break;
        }
        let (frame, shown) = match source.frame_at(*at, &cancelled) {
            Ok(frame) => frame,
            Err(_) if cancelled() => break,
            Err(error) => return Err(error),
        };
        // a frame shown again (held longer than the stated rate) or one past the end of the file
        if frames.last().is_some_and(|last| shown <= last.source_time) || shown >= request.to {
            shared.done.store(index + 1, Ordering::Relaxed);
            continue;
        }
        aspect.get_or_insert(frame.gray.width as f64 / frame.gray.height.max(1) as f64);
        frames.push(FaceFrame { source_time: shown, faces: finder.find(&frame)? });
        shared.done.store(index + 1, Ordering::Relaxed);
    }
    Ok(FaceResult { request, frames, aspect: aspect.unwrap_or(16.0 / 9.0), step, cancelled: cancelled() })
}

/// The timeline span of clip `clip` that shows source times `first` to `last` plus `step`, inside
/// the clip; `None` when it shows none of it.
fn timeline_span(clip: &engine::Clip, first: Time, last: Time, step: Time) -> Option<(Time, Time)> {
    let ends = [clip.timeline_time_of(first), clip.timeline_time_of(last.saturating_add(step))];
    let (low, high) = (ends[0].min(ends[1]), ends[0].max(ends[1]));
    let clip_end = clip.start.saturating_add(clip.duration);
    let (start, end) = (low.max(clip.start), high.min(clip_end));
    (end > start).then_some((start, end))
}

/// What [`add_face_censors`] made.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceCensorsAdded {
    pub project: Project,
    /// The ids of the motion tracks made, one per face written.
    pub ids: Vec<u32>,
    /// The face tracks not written: outside the clip's span, or with no box where it starts.
    pub skipped: usize,
}

/// Writes the face tracks `tracks` of `result` to `project`: a motion track of the file for each
/// and a censor region following it over reference clip `reference`, on sticker tracks with room or
/// new ones (not locked or hidden). Output frames are `width` x `height` pixels. A face track that
/// cannot be placed writes nothing, not even its motion track, and is counted as skipped.
pub fn add_face_censors(project: &Project, reference: ClipKey, result: &FaceResult, tracks: &[Vec<TrackSample>], (width, height): (u32, u32)) -> Result<FaceCensorsAdded, EditError> {
    let clip = project.tracks.get(reference.track).and_then(|lane| lane.clips.get(reference.clip)).ok_or(EditError::ClipNotFound { key: reference })?.clone();
    let mut next = project.clone();
    let mut ids = Vec::new();
    let mut skipped = 0;
    for samples in tracks {
        let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
            skipped += 1;
            continue;
        };
        let Some((start, end)) = timeline_span(&clip, first.source_time, last.source_time, result.step) else {
            skipped += 1;
            continue;
        };
        let id = next.next_motion_track_id();
        let track = MotionTrack { id, media_path: result.request.media_path.clone(), aspect: result.aspect, samples: samples.clone() };
        let source_time = clip.source_time_at(start.saturating_sub(clip.start)).max(Time::ZERO);
        // the box where the region starts, found before anything of this face is written
        let (Some(point), Some(mapping)) = (track.point_at(source_time, 0.0), next.clip_frame_mapping(reference.track, reference.clip, start, track.aspect)) else {
            skipped += 1;
            continue;
        };
        next = engine::set_motion_track(&next, track.clone())?;
        let sample = TrackSample { source_time, cx: point.cx, cy: point.cy, w: point.w, h: point.h, angle: point.angle, confidence: 1.0, manual: false };
        let [x, y, w, h] = crate::track_edit::frame_box(&mapping, &sample);
        let (cx, cy, w, h) = (x + w / 2.0, y + h / 2.0, w * CENSOR_MARGIN, h * CENSOR_MARGIN);
        let range = engine::CENSOR_ASPECT_RANGE;
        let aspect = (w * f64::from(width) / (h * f64::from(height)).max(1.0e-9)).clamp(*range.start(), *range.end());
        let censor = CensorContent { shape: result.request.style.shape, aspect, feather: result.request.style.feather, mode: result.request.style.mode };
        let placement = engine::ClipPlacement { x: cx - w / 2.0, y: cy - h / 2.0, width: w.clamp(engine::MIN_PLACEMENT_WIDTH, engine::MAX_PLACEMENT_WIDTH) };
        let duration = end.saturating_sub(start);
        let lane = match crate::media::choose_track(&next, engine::TrackKind::Sticker, duration, start) {
            crate::media::TrackChoice::Existing(index) => index,
            crate::media::TrackChoice::New => {
                next = engine::add_track(&next, engine::TrackKind::Sticker)?;
                next.tracks.len() - 1
            }
        };
        next = engine::add_censor_clip(&next, lane, start, duration, censor, Some(placement))?;
        let key = engine::clip_at(&next, lane, start).ok_or(EditError::TrackNotFound { track: lane })?;
        let mut follow = crate::track_edit::follow_at(&next, id, reference, start, true, false).ok_or(EditError::MotionTrackNotFound { id })?;
        follow.link.smooth = CENSOR_SMOOTH;
        next = engine::set_follow(&next, key, Some(follow))?;
        ids.push(id);
    }
    Ok(FaceCensorsAdded { project: next, ids, skipped })
}

impl EditorState {
    /// The face censor job for video clip `key` with `style`: refused for a clip that is not a
    /// video, and when the face detector model is not installed.
    pub fn face_request(&self, key: ClipKey, style: FaceCensorStyle) -> Result<FaceRequest, String> {
        let clip = self.project().tracks.get(key.track).and_then(|lane| lane.clips.get(key.clip)).ok_or_else(|| EditError::ClipNotFound { key }.to_string())?;
        if clip.is_still() {
            return Err(format!("track {}, clip {} is not a video", key.track, key.clip));
        }
        if self.is_track_locked(key.track) {
            return Err(EditError::TrackLocked { track: key.track }.to_string());
        }
        let model = crate::tracking_models::installed(TrackingModel::FaceDetector, &crate::project_file::package_roots())
            .ok_or_else(|| "the face censor needs the face detector model: download it or import its zip in the Tracking section of the inspector".to_owned())?;
        let from = clip.source_in.max(Time::ZERO);
        let to = from.saturating_add(clip.source_span());
        Ok(FaceRequest { clip: key, media_path: clip.source.clone(), shown: ClipShown::of(clip), from, to, style, tracks: FaceTrackOptions::default(), model })
    }

    /// Starts `request`, reading `decode_path` (the file of the clip, or its proxy). Answers the job id.
    pub fn start_face_censor(&mut self, decode_path: PathBuf, request: FaceRequest) -> Result<u64, String> {
        use crate::tracking::FileFrames;
        let model = request.model.clone();
        self.faces.start(request, move || FileFrames::open(&decode_path).map(|frames| Box::new(frames) as Box<dyn FrameSource>), move || YuNetFinder::open(&model).map(|finder| Box::new(finder) as Box<dyn FaceFinder>))
    }

    /// Writes the censor regions of finished face jobs, each job as one undo step, says so on the
    /// status line and keeps a report; says whether a job still runs. While edits wait (a drag or
    /// an export) finished jobs wait too. A cancelled job writes nothing. A job whose clip changed
    /// under it (another file there, moved, cut, slowed, reversed, or gone) writes nothing either.
    pub fn poll_face_censor(&mut self) -> bool {
        if self.agent_busy_reason().is_some() {
            return self.faces.running();
        }
        for (id, clip, result) in self.faces.take_finished() {
            let report = match result {
                Err(message) => {
                    self.report_error(Message::FaceCensorFailed(message.clone()));
                    FaceReport { clip, outcome: "failed", faces: 0, track_ids: Vec::new(), message: Some(message) }
                }
                Ok(result) if result.cancelled => {
                    self.report_info(Message::FaceCensorCancelled);
                    FaceReport { clip, outcome: "cancelled", faces: 0, track_ids: Vec::new(), message: None }
                }
                Ok(result) => self.write_face_censors(clip, &result),
            };
            self.faces.keep_report(id, report);
        }
        self.faces.running()
    }

    fn write_face_censors(&mut self, clip: ClipKey, result: &FaceResult) -> FaceReport {
        let current = self.project().tracks.get(clip.track).and_then(|lane| lane.clips.get(clip.clip)).is_some_and(|found| ClipShown::of(found) == result.request.shown);
        if !current {
            self.report_error(Message::FaceCensorStale);
            return FaceReport { clip, outcome: "stale", faces: 0, track_ids: Vec::new(), message: Some("the clip changed while its faces were found".to_owned()) };
        }
        let tracks = face_tracks(&result.frames, result.request.tracks);
        if tracks.is_empty() {
            self.report_info(Message::FaceCensorNone);
            return FaceReport { clip, outcome: "done", faces: 0, track_ids: Vec::new(), message: None };
        }
        let pixels = self.output_pixels();
        let mut made = Vec::new();
        let mut skipped = 0;
        let written = self.run(|project| {
            let added = add_face_censors(project, clip, result, &tracks, pixels)?;
            made = added.ids;
            skipped = added.skipped;
            Ok(added.project)
        });
        if !written {
            return FaceReport { clip, outcome: "failed", faces: 0, track_ids: Vec::new(), message: Some("the censor regions could not be added".to_owned()) };
        }
        self.report_info(Message::FaceCensorDone { count: made.len() });
        let message = (skipped > 0).then(|| format!("{skipped} of {} faces found could not be placed on the clip and were not written", tracks.len()));
        FaceReport { clip, outcome: "done", faces: made.len(), track_ids: made, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::tracker::GrayImage;

    /// Frames of 160 x 90 at 30 per second with one "face" moving right: a finder that is told
    /// where the face is in each frame (by the gray level of the top left pixel).
    struct Moving;

    fn face_at(frame: i64) -> (f64, f64) {
        (0.2 + 0.005 * frame as f64, 0.5)
    }

    impl FrameSource for Moving {
        fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
            (Some((30, 1)), Some(Time::from_ticks(TICKS_PER_SECOND * 4)))
        }

        fn frame_at(&mut self, at: Time, _: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String> {
            let index = (at.ticks() * 30 / TICKS_PER_SECOND) as usize;
            let mut data = vec![0.0; 160 * 90];
            data[0] = index as f32 / 1000.0;
            Ok((TrackFrame::gray(GrayImage::new(160, 90, data)), Time::from_ticks(index as i64 * TICKS_PER_SECOND / 30)))
        }
    }

    struct Told;

    impl FaceFinder for Told {
        fn find(&mut self, frame: &TrackFrame) -> Result<Vec<FaceDetection>, String> {
            let index = (frame.gray.data[0] * 1000.0).round() as i64;
            let (cx, cy) = face_at(index);
            Ok(vec![FaceDetection { cx, cy, w: 0.1, h: 0.2, score: 0.9 }])
        }
    }

    #[test]
    fn detection_times_are_frames_at_most_ten_a_second() {
        let (times, step) = detection_times(Time::ZERO, Time::from_ticks(TICKS_PER_SECOND), Some((30, 1)));
        assert_eq!(times.len(), 10);
        assert_eq!(step, Time::from_ticks(TICKS_PER_SECOND / 10));
        let (times, _) = detection_times(Time::from_ticks(TICKS_PER_SECOND / 2 + 1), Time::from_ticks(TICKS_PER_SECOND), Some((5, 1)));
        assert_eq!(times, vec![Time::from_ticks(TICKS_PER_SECOND * 3 / 5), Time::from_ticks(TICKS_PER_SECOND * 4 / 5)], "a slow file: every frame, from the first after from");
    }

    fn request(clip: ClipKey) -> FaceRequest {
        let shown = ClipShown { source: "a.mp4".into(), source_in: Time::ZERO, source_span: Time::from_ticks(4 * TICKS_PER_SECOND), start: Time::ZERO, duration: Time::from_ticks(4 * TICKS_PER_SECOND), speed: 1.0, reverse: false };
        FaceRequest { clip, media_path: "a.mp4".into(), shown, from: Time::ZERO, to: Time::from_ticks(TICKS_PER_SECOND * 2), style: FaceCensorStyle::default(), tracks: FaceTrackOptions::default(), model: PathBuf::new() }
    }

    /// A video clip of `a.mp4` over 4 s on track 0.
    fn rigged() -> (EditorState, ClipKey) {
        let mut state = EditorState::default();
        state.add_track(engine::TrackKind::Video);
        state.add_clip_from_file(0, std::path::Path::new("a.mp4"));
        let mut project = state.project().clone();
        project.tracks[0].clips[0].duration = Time::from_ticks(4 * TICKS_PER_SECOND);
        state.replace_project(project, None);
        (state, ClipKey { track: 0, clip: 0 })
    }

    #[test]
    fn a_job_finds_faces_and_writes_one_censor_per_face_as_one_step() {
        let (mut state, key) = rigged();
        let id = state.faces.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Told) as Box<dyn FaceFinder>)).unwrap();
        state.faces.wait(id);
        let Some(FaceState::Finished(result)) = state.faces.status(id) else { panic!("{:?}", state.faces.status(id)) };
        assert_eq!(result.frames.len(), 20, "two seconds at ten a second");
        assert!(!result.cancelled);
        let before = state.project().clone();
        assert!(!state.poll_face_censor());
        let report = state.faces.reports[&id].clone();
        assert_eq!((report.outcome, report.faces, report.track_ids.clone()), ("done", 1, vec![1]));
        let next = state.project().clone();
        let track = next.motion_track(1).unwrap();
        assert_eq!(track.samples.len(), 20);
        assert!((track.samples[10].cx - face_at(30).0).abs() < 1e-9);
        let censor: Vec<&engine::Clip> = next.tracks.iter().flat_map(|lane| lane.clips.iter()).filter(|clip| clip.is_censor()).collect();
        assert_eq!(censor.len(), 1);
        let clip = censor[0];
        let follow = clip.follow.unwrap();
        assert_eq!((follow.link.track_id, follow.link.follow_scale, follow.link.follow_rotation, follow.link.smooth), (1, true, false, CENSOR_SMOOTH));
        assert_eq!(clip.censor.unwrap().shape, RegionShape::Ellipse);
        // the region covers the first face, a little bigger, from its first sample to its last plus a step
        let placement = clip.placement.unwrap();
        assert!(((placement.x + placement.width / 2.0) - face_at(0).0).abs() < 1e-6, "{placement:?}");
        assert!((placement.width - 0.1 * CENSOR_MARGIN).abs() < 1e-6);
        assert_eq!(clip.start, Time::ZERO);
        assert_eq!(clip.duration, Time::from_ticks(TICKS_PER_SECOND * 2));
        state.undo();
        assert_eq!(*state.project(), before, "one undo step");
    }

    #[test]
    fn two_faces_get_two_regions_and_a_changed_clip_gets_none() {
        struct Two;
        impl FaceFinder for Two {
            fn find(&mut self, frame: &TrackFrame) -> Result<Vec<FaceDetection>, String> {
                let index = (frame.gray.data[0] * 1000.0).round() as i64;
                let (cx, cy) = face_at(index);
                Ok(vec![FaceDetection { cx, cy, w: 0.1, h: 0.2, score: 0.9 }, FaceDetection { cx: 0.8, cy: 0.3, w: 0.08, h: 0.12, score: 0.95 }])
            }
        }
        let (mut state, key) = rigged();
        let id = state.faces.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Two) as Box<dyn FaceFinder>)).unwrap();
        state.faces.wait(id);
        state.poll_face_censor();
        assert_eq!(state.faces.reports[&id].faces, 2);
        assert_eq!(state.project().tracks.iter().flat_map(|lane| lane.clips.iter()).filter(|clip| clip.is_censor()).count(), 2);
        // the clip shows another file by the time the job ends: nothing is written
        let (mut state, key) = rigged();
        let changed = request(key);
        let id = state.faces.start(FaceRequest { media_path: "b.mp4".into(), shown: ClipShown { source: "b.mp4".into(), ..changed.shown.clone() }, ..changed }, || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Two) as Box<dyn FaceFinder>)).unwrap();
        state.faces.wait(id);
        let before = state.project().clone();
        state.poll_face_censor();
        assert_eq!(state.faces.reports[&id].outcome, "stale");
        assert_eq!(*state.project(), before);
    }

    #[test]
    fn the_request_is_the_clip_as_it_is() {
        let (state, key) = rigged();
        let clip = &state.project().tracks[0].clips[0];
        assert_eq!(ClipShown::of(clip), request(key).shown, "the test request matches the rigged clip");
    }

    /// The clip is moved, cut or slowed while its faces are found: the faces would land in the
    /// wrong place, so nothing is written.
    #[test]
    fn a_clip_moved_or_cut_during_the_job_gets_nothing() {
        let edits: [(&str, fn(&mut engine::Clip)); 4] = [
            ("moved by a clip put before it", |clip| clip.start = Time::from_ticks(TICKS_PER_SECOND)),
            ("cut in two", |clip| clip.duration = Time::from_ticks(TICKS_PER_SECOND)),
            ("trimmed at its head", |clip| clip.source_in = Time::from_ticks(TICKS_PER_SECOND)),
            ("slowed", |clip| clip.speed = 0.5),
        ];
        for (what, edit) in edits {
            let (mut state, key) = rigged();
            let id = state.faces.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Told) as Box<dyn FaceFinder>)).unwrap();
            state.faces.wait(id);
            let mut project = state.project().clone();
            edit(&mut project.tracks[0].clips[0]);
            state.replace_project(project, None);
            let before = state.project().clone();
            state.poll_face_censor();
            assert_eq!(state.faces.reports[&id].outcome, "stale", "{what}");
            assert_eq!(*state.project(), before, "{what}");
        }
    }

    /// A span longer than [`MAX_DETECTIONS`] detections takes a wider step instead of stopping short.
    #[test]
    fn a_long_span_is_covered_to_its_end() {
        let to = Time::from_ticks(TICKS_PER_SECOND * (MAX_DETECTIONS as i64 / 10) * 3);
        let (times, step) = detection_times(Time::ZERO, to, Some((30, 1)));
        assert!(times.len() <= MAX_DETECTIONS);
        let last = *times.last().unwrap();
        assert!(last < to && last.saturating_add(step) >= to, "last {last:?}, step {step:?}, to {to:?}");
        assert_eq!(step, Time::from_ticks(TICKS_PER_SECOND * 3 / 10));
    }

    #[test]
    fn a_locked_track_is_refused() {
        let (mut state, key) = rigged();
        let mut project = state.project().clone();
        project.tracks[0].locked = true;
        state.replace_project(project, None);
        let error = state.face_request(key, FaceCensorStyle::default()).unwrap_err();
        assert_eq!(error, EditError::TrackLocked { track: 0 }.to_string());
    }

    /// The regions go on a sticker track that is not locked or hidden, even when that one has room.
    #[test]
    fn regions_skip_locked_and_hidden_sticker_tracks() {
        for hide in [false, true] {
            let (mut state, key) = rigged();
            state.add_track(engine::TrackKind::Sticker);
            let mut project = state.project().clone();
            if hide {
                project.tracks[1].hidden = true;
            } else {
                project.tracks[1].locked = true;
            }
            state.replace_project(project, None);
            let id = state.faces.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Told) as Box<dyn FaceFinder>)).unwrap();
            state.faces.wait(id);
            state.poll_face_censor();
            assert_eq!(state.faces.reports[&id].faces, 1);
            assert!(state.project().tracks[1].clips.is_empty(), "hidden {hide}");
            assert_eq!(state.project().tracks.len(), 3);
        }
    }

    #[test]
    fn a_cancelled_job_ends_early() {
        let (_, key) = rigged();
        let mut jobs = FaceCensorJobs::default();
        let id = jobs.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Told) as Box<dyn FaceFinder>)).unwrap();
        assert!(jobs.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Told) as Box<dyn FaceFinder>)).is_err() || !jobs.running(), "one job per clip");
        jobs.cancel(id);
        jobs.wait(id);
        let finished = jobs.take_finished();
        assert_eq!(finished.len(), 1);
        let (_, clip, result) = &finished[0];
        assert_eq!(*clip, key);
        let result = result.as_ref().unwrap();
        assert!(result.cancelled || result.frames.len() == 20);
        assert!(!jobs.running());
    }

    /// The golden frame of the YuNet conversion (`scripts/convert-tracking-models.py`), shifted
    /// right one pixel a frame.
    struct Shifting {
        rgba: Vec<u8>,
    }

    impl FrameSource for Shifting {
        fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
            (Some((10, 1)), Some(Time::from_ticks(TICKS_PER_SECOND * 2)))
        }

        fn frame_at(&mut self, at: Time, _: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String> {
            let index = (at.ticks() * 10 / TICKS_PER_SECOND) as usize;
            let (w, h) = (640usize, 352usize);
            let mut rgba = vec![0u8; w * h * 4];
            for y in 0..h {
                let row = &self.rgba[y * w * 4..][..(w - index) * 4];
                rgba[(y * w + index) * 4..][..row.len()].copy_from_slice(row);
            }
            let gray = GrayImage::from_rgba(w, h, &rgba);
            Ok((TrackFrame { gray, rgba: Some(rgba) }, Time::from_ticks(index as i64 * TICKS_PER_SECOND / 10)))
        }
    }

    /// YuNet on the GPU finds the faces of a moving picture and each becomes a censor region.
    #[test]
    fn the_face_detector_censors_the_faces_of_a_moving_picture() {
        let Some(model) = crate::tracking_models::tests::built_zip_of(TrackingModel::FaceDetector) else {
            eprintln!("skipping: yunet-2023mar.zip is not built (scripts/convert-tracking-models.py)");
            return;
        };
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/FFmpegGpuFilter/build/tracking-golden/yunet-2023mar/frame.rgba");
        let Ok(rgba) = std::fs::read(&golden) else {
            eprintln!("skipping: no golden frame");
            return;
        };
        if let Err(error) = crate::tracking::tracking_gpu() {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {error}");
            eprintln!("skipping: {error}");
            return;
        }
        let (mut state, key) = rigged();
        let request = FaceRequest { model: model.clone(), ..request(key) };
        let id = state
            .faces
            .start(request, move || Ok(Box::new(Shifting { rgba }) as Box<dyn FrameSource>), move || YuNetFinder::open(&model).map(|finder| Box::new(finder) as Box<dyn FaceFinder>))
            .unwrap();
        state.faces.wait(id);
        let Some(FaceState::Finished(result)) = state.faces.status(id) else { panic!("{:?}", state.faces.status(id)) };
        assert_eq!(result.frames.len(), 20);
        assert!(result.frames.iter().all(|frame| frame.faces.len() >= 10), "{:?}", result.frames.iter().map(|f| f.faces.len()).collect::<Vec<_>>());
        state.poll_face_censor();
        let report = state.faces.reports[&id].clone();
        assert_eq!(report.outcome, "done");
        assert!(report.faces >= 10, "{report:?}");
        // the large face near the left of the frame moves right with the picture
        let biggest = report.track_ids.iter().filter_map(|id| state.project().motion_track(*id)).max_by(|a, b| a.samples[0].w.total_cmp(&b.samples[0].w)).unwrap();
        let (first, last) = (biggest.samples.first().unwrap(), biggest.samples.last().unwrap());
        let moved = (last.cx - first.cx) * 640.0;
        let frames = (last.source_time.ticks() - first.source_time.ticks()) as f64 * 10.0 / TICKS_PER_SECOND as f64;
        assert!((moved - frames).abs() < 4.0, "moved {moved} px in {frames} frames");
    }

    #[test]
    fn a_failing_finder_fails_the_job() {
        struct Broken;
        impl FaceFinder for Broken {
            fn find(&mut self, _: &TrackFrame) -> Result<Vec<FaceDetection>, String> {
                Err("no GPU".into())
            }
        }
        let (_, key) = rigged();
        let mut jobs = FaceCensorJobs::default();
        let id = jobs.start(request(key), || Ok(Box::new(Moving) as Box<dyn FrameSource>), || Ok(Box::new(Broken) as Box<dyn FaceFinder>)).unwrap();
        jobs.wait(id);
        assert_eq!(jobs.status(id), Some(FaceState::Failed("no GPU".into())));
    }
}
