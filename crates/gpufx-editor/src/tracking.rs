//! Tracker runs in the background: each follows the box of one sample of a motion track frame by
//! frame through the file (its proxy when it has one), forwards or backwards, until the next
//! sample placed by hand, the end of the file, a lost step, or a cancel. Frames are decoded in
//! software at [`ANALYSIS_HEIGHT`] pixels, upright, and HDR sources tone mapped to SDR (see
//! [`FrameSampler`]), then turned gray. Nothing here touches the project: the editor writes the
//! samples of a finished run as one undo step ([`engine::apply_track_run`]).
//!
//! Two trackers ([`TrackMode`]): the classical one of [`engine::tracker`] (fast, on the CPU, follows
//! turns) and NanoTrack, a Siamese network on the GPU ([`gpufx_core::nn::NanoTrack`], the model of
//! [`crate::tracking_models`]): slower, but it holds on through blur, deformation and short
//! occlusions. It keeps the box upright at the angle of the start sample.
//!
//! The frames asked for follow the frame rate the file states, but each sample takes the time of
//! the frame the decoder really showed, and a frame shown twice is stepped over, so a file of
//! variable frame rate is not given made-up times. Still a limit there: a stretch with more
//! frames than the stated rate skips the extra ones, as only one frame is asked for per step.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use d3d12_backend::{probe_media, probe_video_format, FrameSampler};
use engine::tracker::{template, track_frames, GrayImage, Pyramid, RunEnd, StepTracker, TrackBox, TrackStep, Tracker, TrackerOptions};
use engine::{MotionTrack, Time, TrackSample, TICKS_PER_SECOND};

/// The height of the frames the tracker sees.
pub const ANALYSIS_HEIGHT: u32 = 360;
/// The most frames one run steps through.
pub const MAX_RUN_FRAMES: usize = 100_000;
/// Frames read at once, in order, when tracking backwards, so the decoder does not seek back for
/// every frame.
const BACKWARD_CHUNK: usize = 16;
/// How long the decoder may look for one frame.
const DECODE_BUDGET: Duration = Duration::from_secs(20);
/// The frame rate assumed for a file that states none.
const FALLBACK_RATE: (i32, i32) = (30, 1);

/// Which tracker a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackMode {
    /// The classical tracker on the CPU.
    #[default]
    Fast,
    /// NanoTrack on the GPU; needs its model ([`crate::tracking_models`]).
    Robust,
}

impl TrackMode {
    pub fn code(self) -> &'static str {
        match self {
            TrackMode::Fast => "fast",
            TrackMode::Robust => "robust",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        [TrackMode::Fast, TrackMode::Robust].into_iter().find(|mode| mode.code() == code)
    }
}

/// A frame as the trackers read it: gray for the classical tracker and the look check, and the
/// RGBA picture it came from (when there is one) for NanoTrack.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackFrame {
    pub gray: GrayImage,
    /// `gray.width * gray.height * 4` bytes.
    pub rgba: Option<Vec<u8>>,
}

impl TrackFrame {
    /// A frame known only in gray.
    pub fn gray(gray: GrayImage) -> Self {
        Self { gray, rgba: None }
    }

    /// The RGBA picture, made from the gray one when there is none.
    fn rgba(&self) -> std::borrow::Cow<'_, [u8]> {
        match &self.rgba {
            Some(rgba) => std::borrow::Cow::Borrowed(rgba),
            None => std::borrow::Cow::Owned(
                self.gray
                    .data
                    .iter()
                    .flat_map(|v| {
                        let level = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                        [level, level, level, 255]
                    })
                    .collect(),
            ),
        }
    }
}

/// Which way a run goes through the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

impl Direction {
    pub fn code(self) -> &'static str {
        match self {
            Direction::Forward => "forward",
            Direction::Backward => "backward",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        [Direction::Forward, Direction::Backward].into_iter().find(|direction| direction.code() == code)
    }
}

/// The frames of a file as the tracker reads them.
/// Made on the run's own thread, so it need not be `Send`.
pub trait FrameSource {
    /// The frame rate (frames per second as `num / den`) and the length of the file, when stated.
    fn timing(&self) -> (Option<(i32, i32)>, Option<Time>);
    /// The frame shown at source time `at` and the source time of that frame as decoded: later
    /// than `at` when no frame starts there, earlier past the end of the file.
    fn frame_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String>;
}

/// A video file read with [`FrameSampler`] in exact mode.
pub struct FileFrames {
    sampler: FrameSampler,
    rate: Option<(i32, i32)>,
    length: Option<Time>,
}

impl FileFrames {
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let format = probe_video_format(path)?;
        let length = probe_media(path)?.video_duration;
        let mut sampler = FrameSampler::open(path, ANALYSIS_HEIGHT)?;
        sampler.set_exact(DECODE_BUDGET);
        Ok(Self { sampler, rate: format.rate, length })
    }
}

impl FrameSource for FileFrames {
    fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
        (self.rate, self.length)
    }

    fn frame_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String> {
        let (picture, shown) = self.sampler.timed_picture_at(at, cancelled).map_err(|error| format!("{error:?}"))?;
        let gray = GrayImage::from_rgba(picture.width as usize, picture.height as usize, &picture.rgba);
        Ok((TrackFrame { gray, rgba: Some(picture.rgba) }, shown))
    }
}

/// What a run is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRequest {
    pub track_id: u32,
    /// The file of the track when the run starts, checked again before its samples are written.
    pub media_path: String,
    /// The sample the run starts from.
    pub start: TrackSample,
    pub direction: Direction,
    /// The source time of the next sample placed by hand that way, where the run stops before.
    pub stop: Option<Time>,
    pub rotation: bool,
    /// The most frames the run steps through after its start ([`MAX_RUN_FRAMES`] when absent):
    /// 1 tracks a single frame.
    pub limit: Option<usize>,
    pub mode: TrackMode,
    /// The NanoTrack model for [`TrackMode::Robust`].
    pub model: Option<PathBuf>,
}

impl RunRequest {
    /// The run from the sample of `track` at `from` (the last sample placed by hand, else the
    /// last sample, when `None`; the first ones when going backwards), up to the next sample placed
    /// by hand that way.
    pub fn for_track(track: &MotionTrack, from: Option<Time>, direction: Direction, rotation: bool) -> Result<Self, String> {
        let samples = &track.samples;
        let start = match from {
            Some(at) => samples.iter().find(|s| s.source_time == at).copied().ok_or_else(|| format!("motion track {} has no sample at source time {}", track.id, at.ticks()))?,
            None => {
                let chosen = match direction {
                    Direction::Forward => samples.iter().rev().find(|s| s.manual).or(samples.last()),
                    Direction::Backward => samples.iter().find(|s| s.manual).or(samples.first()),
                };
                *chosen.ok_or_else(|| format!("motion track {} has no sample to start from", track.id))?
            }
        };
        let stop = match direction {
            Direction::Forward => samples.iter().find(|s| s.manual && s.source_time > start.source_time).map(|s| s.source_time),
            Direction::Backward => samples.iter().rev().find(|s| s.manual && s.source_time < start.source_time).map(|s| s.source_time),
        };
        Ok(Self { track_id: track.id, media_path: track.media_path.clone(), start, direction, stop, rotation, limit: None, mode: TrackMode::Fast, model: None })
    }

    /// This run with `mode`; [`TrackMode::Robust`] needs the NanoTrack model installed (looked for
    /// with `package_roots`).
    pub fn with_mode(self, mode: TrackMode, package_roots: &[PathBuf]) -> Result<Self, String> {
        let model = match mode {
            TrackMode::Fast => None,
            TrackMode::Robust => Some(crate::tracking_models::installed(package_roots).ok_or_else(|| {
                "the robust tracker needs the NanoTrack model: download it or import its zip in the Tracking section of the inspector".to_owned()
            })?),
        };
        Ok(Self { mode, model, ..self })
    }
}

/// The frame of `rate` that shows source time `at`.
fn frame_index(at: Time, (num, den): (i32, i32)) -> i64 {
    ((i128::from(at.ticks()) * i128::from(num)) / (i128::from(den) * i128::from(TICKS_PER_SECOND))) as i64
}

/// The start of frame `index` of `rate`, rounded down to a tick, so the decoder's first frame at
/// or after it is that frame.
fn frame_time(index: i64, (num, den): (i32, i32)) -> Time {
    Time::from_ticks(((i128::from(index) * i128::from(den) * i128::from(TICKS_PER_SECOND)) / i128::from(num)) as i64)
}

/// The frames a run steps through after its start frame, in order, and the start frame's time.
pub fn run_times(request: &RunRequest, rate: Option<(i32, i32)>, length: Option<Time>) -> (Time, Vec<Time>) {
    let rate = rate.filter(|(num, den)| *num > 0 && *den > 0).unwrap_or(FALLBACK_RATE);
    let first = frame_index(request.start.source_time.max(Time::ZERO), rate);
    let last = length.map(|length| frame_index(length, rate) - 1);
    let stop = request.stop.map(|stop| frame_index(stop, rate));
    let mut times = Vec::new();
    let mut index = first;
    let most = request.limit.unwrap_or(MAX_RUN_FRAMES).min(MAX_RUN_FRAMES);
    while times.len() < most {
        index += match request.direction {
            Direction::Forward => 1,
            Direction::Backward => -1,
        };
        let beyond = match request.direction {
            Direction::Forward => stop.is_some_and(|stop| index >= stop) || last.is_some_and(|last| index > last),
            Direction::Backward => index < 0 || stop.is_some_and(|stop| index <= stop),
        };
        if beyond {
            break;
        }
        times.push(frame_time(index, rate));
    }
    (frame_time(first, rate), times)
}

/// How a run ended, with the samples it made, in the order made, and what it started from, to
/// check the track still fits before they are written.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    pub track_id: u32,
    pub media_path: String,
    pub start: TrackSample,
    pub direction: Direction,
    pub samples: Vec<TrackSample>,
    pub end: RunEnd,
}

impl RunResult {
    /// The run as [`engine::apply_track_run`] writes it.
    pub fn track_run(&self) -> engine::TrackRun {
        engine::TrackRun {
            track_id: self.track_id,
            media_path: self.media_path.clone(),
            start: self.start,
            forward: self.direction == Direction::Forward,
            samples: self.samples.clone(),
            lost: self.end == RunEnd::Lost,
        }
    }
}

/// What a finished run did, kept for an agent that asks later.
#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    pub track_id: u32,
    /// `done`, `lost`, `cancelled`, `stale` (the track changed while it ran, nothing written)
    /// or `failed`.
    pub outcome: &'static str,
    /// Samples written.
    pub count: usize,
    /// The last sample written: for a lost run, the one marked to fix.
    pub last: Option<TrackSample>,
    /// Why it failed.
    pub message: Option<String>,
}

/// Where a run stands.
#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    Running { done: usize, total: usize },
    Finished(RunResult),
    Failed(String),
}

#[derive(Default)]
struct Shared {
    done: AtomicUsize,
    total: AtomicUsize,
    cancel: AtomicBool,
    result: Mutex<Option<Result<RunResult, String>>>,
    finished: Condvar,
}

impl Shared {
    fn finish(&self, result: Result<RunResult, String>) {
        *self.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
        self.finished.notify_all();
    }

    fn result(&self) -> Option<Result<RunResult, String>> {
        self.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

struct Job {
    track_id: u32,
    shared: Arc<Shared>,
}

/// The runs of this session, by job id. A finished run stays until [`TrackRunner::take_finished`].
#[derive(Default)]
pub struct TrackRunner {
    jobs: BTreeMap<u64, Job>,
    next_id: u64,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl TrackRunner {
    /// Called from a run's thread when it has finished, so a window repaints.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    /// The job running on motion track `track_id`, if one does.
    pub fn running_on(&self, track_id: u32) -> Option<u64> {
        self.jobs.iter().find(|(_, job)| job.track_id == track_id && job.shared.result().is_none()).map(|(id, _)| *id)
    }

    /// Starts `request` on a new thread, reading frames from what `open` gives. Refused while
    /// another run works on the same track.
    pub fn start(&mut self, request: RunRequest, open: impl FnOnce() -> Result<Box<dyn FrameSource>, String> + Send + 'static) -> Result<u64, String> {
        if let Some(job) = self.running_on(request.track_id) {
            return Err(format!("motion track {} is being tracked by job {job}", request.track_id));
        }
        self.next_id += 1;
        let id = self.next_id;
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let waker = self.waker.clone();
        let track_id = request.track_id;
        std::thread::Builder::new()
            .name("object tracker".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open().and_then(|mut source| run(&request, source.as_mut(), &thread_shared))))
                    .unwrap_or_else(|_| Err("the tracker stopped on an internal error".to_owned()));
                thread_shared.finish(result);
                if let Some(waker) = waker {
                    waker();
                }
            })
            .map_err(|error| error.to_string())?;
        self.jobs.insert(id, Job { track_id, shared });
        Ok(id)
    }

    /// Where job `id` stands; `None` for a job this runner does not know (or no longer holds).
    pub fn status(&self, id: u64) -> Option<RunState> {
        let job = self.jobs.get(&id)?;
        Some(match job.shared.result() {
            None => RunState::Running { done: job.shared.done.load(Ordering::Relaxed), total: job.shared.total.load(Ordering::Relaxed) },
            Some(Ok(result)) => RunState::Finished(result),
            Some(Err(message)) => RunState::Failed(message),
        })
    }

    /// Asks job `id` to stop; it finishes with the samples made so far.
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

    /// Whether a run is still going.
    pub fn running(&self) -> bool {
        self.jobs.values().any(|job| job.shared.result().is_none())
    }

    /// The finished jobs, the track each ran on and their outcomes, which this runner forgets.
    pub fn take_finished(&mut self) -> Vec<(u64, u32, Result<RunResult, String>)> {
        let ids: Vec<u64> = self.jobs.iter().filter(|(_, job)| job.shared.result().is_some()).map(|(id, _)| *id).collect();
        ids.into_iter().filter_map(|id| self.jobs.remove(&id).and_then(|job| job.shared.result().map(|result| (id, job.track_id, result)))).collect()
    }
}

impl Drop for TrackRunner {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.shared.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Runs `request` on `source`, counting the frames into `shared`.
fn run(request: &RunRequest, source: &mut dyn FrameSource, shared: &Shared) -> Result<RunResult, String> {
    let (rate, length) = source.timing();
    let (start_time, times) = run_times(request, rate, length);
    shared.total.store(times.len(), Ordering::Relaxed);
    let cancelled = || shared.cancel.load(Ordering::Relaxed);
    let finished = |samples: Vec<TrackSample>, end: RunEnd| RunResult {
        track_id: request.track_id,
        media_path: request.media_path.clone(),
        start: request.start,
        direction: request.direction,
        samples,
        end,
    };
    let (first, first_shown) = match source.frame_at(start_time, &cancelled) {
        Ok(first) => first,
        // Cut short by the cancel: a cancelled run, with no samples, not a failed one.
        Err(_) if cancelled() => return Ok(finished(Vec::new(), RunEnd::Cancelled)),
        Err(error) => return Err(error),
    };
    let (width, height) = (first.gray.width, first.gray.height);
    let area = TrackBox::from_sample(&request.start, width, height);
    let mut tracker = match (request.mode, &request.model) {
        (TrackMode::Fast, _) => RunTracker::Fast(Box::new(Tracker::new(first.gray, area, TrackerOptions { rotation: request.rotation, ..TrackerOptions::default() }))),
        (TrackMode::Robust, Some(model)) => RunTracker::Robust(Box::new(RobustTracker::new(model, &first, area)?)),
        (TrackMode::Robust, None) => return Err("the robust tracker needs the NanoTrack model".to_owned()),
    };
    let mut shown = Vec::with_capacity(times.len());
    let frames = Frames {
        source,
        times: &times,
        next: 0,
        direction: request.direction,
        ahead: Vec::new(),
        cancel: &shared.cancel,
        last_shown: first_shown,
        stop: request.stop,
        shown: &mut shown,
    };
    let (steps, end) = track_frames(&mut tracker, frames, &shared.cancel, |index, _| shared.done.store(index + 1, Ordering::Relaxed))?;
    let samples = steps.iter().zip(&shown).map(|(step, at)| step.area.to_sample(width, height, *at, step.confidence)).collect();
    Ok(finished(samples, end))
}

/// The tracker of a run.
enum RunTracker {
    Fast(Box<Tracker>),
    Robust(Box<RobustTracker>),
}

impl StepTracker<TrackFrame> for RunTracker {
    fn step(&mut self, frame: TrackFrame) -> TrackStep {
        match self {
            RunTracker::Fast(tracker) => tracker.step(frame.gray),
            RunTracker::Robust(tracker) => tracker.step(frame),
        }
    }
}

/// A step of the robust tracker less sure than this is lost: the run stops there.
pub const ROBUST_LOST_CONFIDENCE: f64 = 0.25;

/// How much of each sure step's look goes into the look the robust tracker checks against.
const ROBUST_TEMPLATE_RATE: f32 = 0.1;

/// The GPU of the robust tracker: one device for every run of the session, made on first use.
fn tracking_gpu() -> Result<Arc<gpufx_core::GpuContext>, String> {
    static CONTEXT: Mutex<Option<Arc<gpufx_core::GpuContext>>> = Mutex::new(None);
    let mut context = CONTEXT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(context) = context.as_ref() {
        return Ok(context.clone());
    }
    let made = gpufx_core::GpuContext::new(gpufx_core::Backend::Auto).map_err(|error| format!("the robust tracker needs a GPU: {error}"))?;
    *context = Some(made.clone());
    Ok(made)
}

/// NanoTrack, with a check of the box's look: the network's score stays high on almost anything,
/// so how sure a step is also takes how well the box still looks like the object (NCC of the gray
/// patch against the remembered look, as the classical tracker does).
struct RobustTracker {
    nano: gpufx_core::nn::NanoTrack,
    /// The angle of the start sample, kept: NanoTrack finds upright boxes.
    angle: f64,
    area: TrackBox,
    template: Vec<f32>,
}

fn rgba_image(rgba: &[u8], width: usize, height: usize) -> gpufx_core::Image<'_> {
    gpufx_core::Image { data: rgba, width: width as u32, height: height as u32, stride: width as u32 * 4 }
}

impl RobustTracker {
    fn new(model: &Path, first: &TrackFrame, area: TrackBox) -> Result<Self, String> {
        let mut nano = gpufx_core::nn::NanoTrack::open(tracking_gpu()?, model).map_err(|error| format!("the NanoTrack model failed to load: {error}"))?;
        let rgba = first.rgba();
        let rect = gpufx_core::nn::TrackRect { cx: area.cx as f32, cy: area.cy as f32, w: area.w as f32, h: area.h as f32 };
        nano.init(&rgba_image(&rgba, first.gray.width, first.gray.height), rect).map_err(|error| error.to_string())?;
        let template = template::sample_patch(&Pyramid::new(first.gray.clone(), 3), &area);
        Ok(Self { nano, angle: area.angle, area, template })
    }

    fn step(&mut self, frame: TrackFrame) -> TrackStep {
        let lost = |area| TrackStep { area, confidence: 0.0, lost: true, recovered: false };
        let rgba = frame.rgba();
        let found = match self.nano.update(&rgba_image(&rgba, frame.gray.width, frame.gray.height)) {
            Ok(found) => found,
            Err(_) => return lost(self.area),
        };
        let rect = found.rect;
        let area = TrackBox { cx: f64::from(rect.cx), cy: f64::from(rect.cy), w: f64::from(rect.w), h: f64::from(rect.h), angle: self.angle };
        let patch = template::sample_patch(&Pyramid::new(frame.gray, 3), &area);
        let look = template::ncc(&self.template, &patch).clamp(0.0, 1.0);
        let confidence = (f64::from(found.score) * (0.5 + 0.5 * look)).clamp(0.0, 1.0);
        if confidence < ROBUST_LOST_CONFIDENCE {
            return TrackStep { confidence, ..lost(self.area) };
        }
        if confidence >= engine::tracker::LOST_CONFIDENCE {
            for (kept, seen) in self.template.iter_mut().zip(&patch) {
                *kept += (seen - *kept) * ROBUST_TEMPLATE_RATE;
            }
        }
        self.area = area;
        TrackStep { area, confidence, lost: false, recovered: false }
    }
}

/// The frames at `times`, in that order; backwards they are read a chunk at a time in file order.
/// A frame the decoder shows at the time of the frame before, or the wrong way from it, is stepped
/// over (a frame held longer than the stated rate); one before the time asked for going forwards
/// means the file ended. Each frame given has its source time pushed to `shown`.
struct Frames<'a> {
    source: &'a mut dyn FrameSource,
    times: &'a [Time],
    next: usize,
    direction: Direction,
    /// Frames read ahead with their source times, the next one last.
    ahead: Vec<(TrackFrame, Time)>,
    cancel: &'a AtomicBool,
    /// The source time of the frame given last (the start frame at first).
    last_shown: Time,
    /// The source time of the sample placed by hand that the run stops before.
    stop: Option<Time>,
    shown: &'a mut Vec<Time>,
}

impl Frames<'_> {
    /// The next frame read, with the time asked for and the time shown; `None` past `times`.
    fn read(&mut self) -> Option<Result<(TrackFrame, Time, Time), String>> {
        if self.next >= self.times.len() {
            return None;
        }
        let cancel = self.cancel;
        let cancelled = || cancel.load(Ordering::Relaxed);
        if self.direction == Direction::Backward && self.ahead.is_empty() {
            let end = (self.next + BACKWARD_CHUNK).min(self.times.len());
            // `times` go down, so the chunk read from its far end goes up the file; the frame
            // wanted next is read last and so popped first.
            for at in self.times[self.next..end].iter().rev() {
                match self.source.frame_at(*at, &cancelled) {
                    Ok(frame) => self.ahead.push(frame),
                    Err(error) => return Some(Err(error)),
                }
            }
        }
        let asked = self.times[self.next];
        let frame = match self.direction {
            Direction::Backward => self.ahead.pop().map(Ok),
            Direction::Forward => Some(self.source.frame_at(asked, &cancelled)),
        };
        self.next += 1;
        frame.map(|frame| frame.map(|(image, shown)| (image, asked, shown)))
    }
}

impl Iterator for Frames<'_> {
    type Item = Result<TrackFrame, String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (image, asked, shown) = match self.read()? {
                Ok(frame) => frame,
                Err(error) => return Some(Err(error)),
            };
            let (onwards, past_stop) = match self.direction {
                Direction::Forward => (shown > self.last_shown, self.stop.is_some_and(|stop| shown >= stop)),
                Direction::Backward => (shown < self.last_shown, self.stop.is_some_and(|stop| shown <= stop)),
            };
            if past_stop || (self.direction == Direction::Forward && !onwards && shown < asked) {
                // At the sample placed by hand, or the file ended before its stated length.
                self.next = self.times.len();
                return None;
            }
            if !onwards {
                continue;
            }
            self.last_shown = shown;
            self.shown.push(shown);
            return Some(Ok(image));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain bright square on gray, moving `speed` pixels a frame to the right from x = 40.
    struct Sliding {
        speed: f64,
        reads: Arc<AtomicUsize>,
    }

    fn texture(x: f64, y: f64) -> f32 {
        (0.5 + 0.25 * (x * 0.9).sin() * (y * 0.7).cos() + 0.15 * (x * 0.31 + y * 0.53).sin()) as f32
    }

    impl FrameSource for Sliding {
        fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
            (Some((30, 1)), Some(Time::from_ticks(TICKS_PER_SECOND * 2)))
        }

        fn frame_at(&mut self, at: Time, _: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let index = frame_index(at, (30, 1));
            Ok((TrackFrame::gray(self.picture(index as f64)), frame_time(index, (30, 1))))
        }
    }

    impl Sliding {
        fn picture(&self, frame: f64) -> GrayImage {
            let left = 40.0 + self.speed * frame;
            let (width, height) = (160, 90);
            let mut data = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let inside = (x as f64) >= left && (x as f64) < left + 30.0 && (30..60).contains(&y);
                    data.push(if inside { texture(x as f64 - left, y as f64) } else { 0.5 });
                }
            }
            GrayImage::new(width, height, data)
        }
    }

    fn start(at_frame: i64, left: f64, manual: bool) -> TrackSample {
        TrackSample { source_time: frame_time(at_frame, (30, 1)), cx: (left + 15.0) / 160.0, cy: 45.0 / 90.0, w: 30.0 / 160.0, h: 30.0 / 90.0, angle: 0.0, confidence: 1.0, manual }
    }

    fn track(samples: Vec<TrackSample>) -> MotionTrack {
        MotionTrack { id: 3, media_path: "a.mp4".into(), aspect: 16.0 / 9.0, samples }
    }

    #[test]
    fn a_run_goes_from_the_last_manual_sample_to_the_next_one_or_the_end() {
        let t = track(vec![start(0, 40.0, true), start(10, 60.0, false), start(20, 80.0, true)]);
        let forward = RunRequest::for_track(&t, None, Direction::Forward, true).unwrap();
        assert_eq!((forward.start.source_time, forward.stop), (frame_time(20, (30, 1)), None));
        let (first, times) = run_times(&forward, Some((30, 1)), Some(Time::from_ticks(TICKS_PER_SECOND)));
        assert_eq!(first, frame_time(20, (30, 1)));
        assert_eq!(times, (21..30).map(|i| frame_time(i, (30, 1))).collect::<Vec<_>>());
        let from_first = RunRequest::for_track(&t, Some(Time::ZERO), Direction::Forward, true).unwrap();
        assert_eq!(from_first.stop, Some(frame_time(20, (30, 1))));
        assert_eq!(run_times(&from_first, Some((30, 1)), None).1.len(), 19, "frames 1 to 19");
        let backward = RunRequest::for_track(&t, Some(frame_time(20, (30, 1))), Direction::Backward, true).unwrap();
        assert_eq!(backward.stop, Some(Time::ZERO));
        assert_eq!(run_times(&backward, Some((30, 1)), None).1, (1..20).rev().map(|i| frame_time(i, (30, 1))).collect::<Vec<_>>());
        assert!(RunRequest::for_track(&t, Some(Time::from_ticks(7)), Direction::Forward, true).is_err());
        let one = RunRequest { limit: Some(1), ..RunRequest::for_track(&t, Some(Time::ZERO), Direction::Forward, true).unwrap() };
        assert_eq!(run_times(&one, Some((30, 1)), None).1, vec![frame_time(1, (30, 1))], "a single frame");
    }

    #[test]
    fn the_times_of_a_fractional_rate_land_on_their_frames() {
        let rate = (30_000, 1001);
        for index in [0, 1, 7, 1000, 107_892] {
            assert_eq!(frame_index(frame_time(index, rate), rate), index);
        }
    }

    fn run_on(source: Sliding, request: RunRequest) -> (u64, TrackRunner) {
        run_on_source(Box::new(source), request)
    }

    fn run_on_source(source: Box<dyn FrameSource + Send>, request: RunRequest) -> (u64, TrackRunner) {
        let mut runner = TrackRunner::default();
        let id = runner.start(request, move || Ok(source as Box<dyn FrameSource>)).unwrap();
        (id, runner)
    }

    #[test]
    fn a_background_run_follows_the_object_forwards_and_backwards() {
        for (direction, from) in [(Direction::Forward, 0), (Direction::Backward, 59)] {
            let reads = Arc::new(AtomicUsize::new(0));
            let request = RunRequest::for_track(&track(vec![start(from, 40.0 + 1.5 * from as f64, true)]), None, direction, false).unwrap();
            let (id, runner) = run_on(Sliding { speed: 1.5, reads: reads.clone() }, request);
            runner.wait(id);
            let Some(RunState::Finished(result)) = runner.status(id) else { panic!("{:?}", runner.status(id)) };
            assert_eq!(result.end, RunEnd::Finished, "{direction:?}");
            assert_eq!(result.samples.len(), 59, "{direction:?}");
            for sample in &result.samples {
                let frame = frame_index(sample.source_time, (30, 1)) as f64;
                let expected = (40.0 + 1.5 * frame + 15.0) / 160.0;
                assert!(((sample.cx - expected) * 160.0).abs() < 1.0, "{direction:?} frame {frame}: {} vs {expected}", sample.cx);
                assert!(!sample.manual && sample.confidence > 0.7);
            }
            assert_eq!(reads.load(Ordering::Relaxed), 60, "{direction:?}: each frame read once");
        }
    }

    /// Frames that wait until let go, so a test can cancel a run in the middle.
    struct Gated {
        inner: Sliding,
        gate: Arc<(Mutex<usize>, Condvar)>,
        /// Fail a read the cancel cuts short, rather than finish it.
        fail_on_cancel: bool,
    }

    impl FrameSource for Gated {
        fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
            self.inner.timing()
        }

        fn frame_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String> {
            let (allowed, signal) = &*self.gate;
            let mut allowed = allowed.lock().unwrap();
            while *allowed == 0 && !cancelled() {
                allowed = signal.wait_timeout(allowed, Duration::from_millis(5)).unwrap().0;
            }
            if *allowed == 0 && self.fail_on_cancel {
                // As the decoder does when it sees the cancel in the middle of a search.
                return Err("cancelled".to_owned());
            }
            *allowed = allowed.saturating_sub(1);
            drop(allowed);
            self.inner.frame_at(at, cancelled)
        }
    }

    #[test]
    fn a_cancelled_run_ends_with_the_samples_made_so_far() {
        let gate = Arc::new((Mutex::new(4usize), Condvar::new()));
        let source = Gated { inner: Sliding { speed: 1.0, reads: Arc::default() }, gate: gate.clone(), fail_on_cancel: false };
        let request = RunRequest::for_track(&track(vec![start(0, 40.0, true)]), None, Direction::Forward, true).unwrap();
        let mut runner = TrackRunner::default();
        let id = runner.start(request, move || Ok(Box::new(source) as Box<dyn FrameSource>)).unwrap();
        while !matches!(runner.status(id), Some(RunState::Running { done: 3, .. })) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(runner.running_on(3), Some(id));
        assert!(runner.start(RunRequest::for_track(&track(vec![start(0, 40.0, true)]), None, Direction::Forward, true).unwrap(), || Err("unused".to_owned())).is_err(), "one run per track");
        runner.cancel(id);
        runner.wait(id);
        let finished = runner.take_finished();
        assert_eq!(finished.len(), 1);
        let result = finished[0].2.clone().unwrap();
        assert_eq!(finished[0].1, 3);
        assert_eq!(result.end, RunEnd::Cancelled);
        assert!((3..=4).contains(&result.samples.len()), "{}", result.samples.len());
        assert!(!runner.running() && runner.status(id).is_none());
    }

    #[test]
    fn a_source_that_cannot_open_fails_the_run() {
        let request = RunRequest::for_track(&track(vec![start(0, 40.0, true)]), None, Direction::Forward, true).unwrap();
        let mut runner = TrackRunner::default();
        let id = runner.start(request, || Err("no such file".to_owned())).unwrap();
        runner.wait(id);
        assert_eq!(runner.status(id), Some(RunState::Failed("no such file".to_owned())));
    }

    /// Cancels a run on a source that fails the read the cancel cuts short, once `allowed` reads
    /// have gone through and the run waits on the next.
    fn cancel_while_reading(allowed: usize) -> RunState {
        let gate = Arc::new((Mutex::new(allowed), Condvar::new()));
        let source = Gated { inner: Sliding { speed: 1.0, reads: Arc::default() }, gate: gate.clone(), fail_on_cancel: true };
        let request = RunRequest::for_track(&track(vec![start(0, 40.0, true)]), None, Direction::Forward, true).unwrap();
        let mut runner = TrackRunner::default();
        let id = runner.start(request, move || Ok(Box::new(source) as Box<dyn FrameSource>)).unwrap();
        while *gate.0.lock().unwrap() > 0 || !matches!(runner.status(id), Some(RunState::Running { done, .. }) if done + 1 >= allowed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(20));
        runner.cancel(id);
        runner.wait(id);
        runner.status(id).unwrap()
    }

    #[test]
    fn a_cancel_that_cuts_a_read_short_keeps_the_samples_made() {
        let RunState::Finished(result) = cancel_while_reading(4) else { panic!("a cancel is not a failure") };
        assert_eq!(result.end, RunEnd::Cancelled);
        assert_eq!(result.samples.len(), 3, "the start frame and three steps were read");
        let RunState::Finished(result) = cancel_while_reading(0) else { panic!("a cancel during the first frame is not a failure") };
        assert_eq!((result.end, result.samples.len()), (RunEnd::Cancelled, 0));
    }

    /// [`Sliding`] where every frame is held for two frames of the stated rate, and the file ends
    /// after `frames` frames although it states two seconds.
    struct Held {
        inner: Sliding,
        frames: i64,
    }

    impl FrameSource for Held {
        fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
            self.inner.timing()
        }

        fn frame_at(&mut self, at: Time, _: &dyn Fn() -> bool) -> Result<(TrackFrame, Time), String> {
            // Only even frames exist: the first one at or after `at`, or the last one.
            let last = (self.frames - 1) / 2 * 2;
            let wanted = frame_index(at, (30, 1));
            let index = (wanted + wanted % 2).min(last);
            Ok((TrackFrame::gray(self.inner.picture(index as f64)), frame_time(index, (30, 1))))
        }
    }

    #[test]
    fn samples_take_the_times_of_the_frames_really_shown() {
        for direction in [Direction::Forward, Direction::Backward] {
            let from = if direction == Direction::Forward { 0 } else { 40 };
            let source = Held { inner: Sliding { speed: 1.0, reads: Arc::default() }, frames: 41 };
            let request = RunRequest::for_track(&track(vec![start(from, 40.0 + from as f64, true)]), None, direction, false).unwrap();
            let (id, runner) = run_on_source(Box::new(source), request);
            runner.wait(id);
            let Some(RunState::Finished(result)) = runner.status(id) else { panic!("{:?}", runner.status(id)) };
            assert_eq!(result.end, RunEnd::Finished, "{direction:?}");
            let frames: Vec<i64> = result.samples.iter().map(|s| frame_index(s.source_time, (30, 1))).collect();
            let expected: Vec<i64> = match direction {
                Direction::Forward => (1..=20).map(|i| 2 * i).collect(),
                Direction::Backward => (0..20).rev().map(|i| 2 * i).collect(),
            };
            assert_eq!(frames, expected, "{direction:?}: one sample per frame shown, at its own time, none past the end of the file");
            for (sample, frame) in result.samples.iter().zip(&frames) {
                assert_eq!(sample.source_time, frame_time(*frame, (30, 1)));
                let expected = (40.0 + *frame as f64 + 15.0) / 160.0;
                assert!(((sample.cx - expected) * 160.0).abs() < 1.0, "{direction:?} frame {frame}: {}", sample.cx);
            }
        }
    }
    /// A file stored landscape and shown turned a quarter (a phone video): the tracker reads the
    /// upright picture, in the shape the editor gives the clip, and the way ffmpeg turns it.
    #[test]
    fn a_turned_file_is_tracked_upright_in_the_shape_of_its_clip() {
        use crate::visuals::worker::Generator;
        let tool = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe"));
        let dir = std::env::temp_dir().join(format!("gpufx-tracking-turned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ffmpeg = |arguments: &[&str]| {
            let status = std::process::Command::new(&tool).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).status().expect("ffmpeg runs");
            assert!(status.success(), "ffmpeg {arguments:?}");
        };
        let stored = dir.join("stored.mp4");
        let turned = dir.join("turned.mp4");
        // A bright block near the top left of the stored picture shows where the turn takes it.
        ffmpeg(&["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=30:d=1", "-vf", "drawbox=x=20:y=20:w=260:h=150:color=white:t=fill", "-c:v", "libopenh264", "-pix_fmt", "yuv420p", "-b:v", "4M", &stored.to_string_lossy()]);
        ffmpeg(&["-display_rotation", "90", "-i", &stored.to_string_lossy(), "-c", "copy", &turned.to_string_lossy()]);
        let facts = crate::visuals::backend::FfmpegGenerator.inspect(&turned).expect("inspect");
        let mut frames = FileFrames::open(&turned).expect("open");
        let (picture, _) = frames.frame_at(Time::ZERO, &|| false).expect("a frame");
        let picture = picture.gray;
        assert!(picture.height > picture.width, "upright: {}x{}", picture.width, picture.height);
        let shape = picture.width as f32 / picture.height as f32;
        assert!((shape - facts.aspect).abs() < 0.02, "tracker {shape} and clip {} agree", facts.aspect);
        // ffmpeg turns the file by itself on decoding: the same picture.
        let raw = dir.join("upright.gray");
        let size = format!("scale={}:{}", picture.width, picture.height);
        ffmpeg(&["-i", &turned.to_string_lossy(), "-frames:v", "1", "-vf", &size, "-f", "rawvideo", "-pix_fmt", "gray", &raw.to_string_lossy()]);
        let oracle = std::fs::read(&raw).unwrap();
        assert_eq!(oracle.len(), picture.data.len());
        let difference = picture.data.iter().zip(&oracle).map(|(a, b)| (a - f32::from(*b) / 255.0).abs()).sum::<f32>() / oracle.len() as f32;
        assert!(difference < 0.05, "turned the way ffmpeg turns it: mean difference {difference}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A sample clip made by ffmpeg: a test pattern 64x48 sliding and bobbing over a still
    /// textured background, 2 s at 30 fps. Both trackers must keep the box on it: the robust one
    /// when its model is built (`scripts/convert-tracking-models.py` of FFmpegGpuFilter) and a GPU
    /// is there.
    #[test]
    fn both_trackers_follow_an_object_through_a_sample_clip() {
        let tool = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe"));
        let dir = std::env::temp_dir().join(format!("gpufx-tracking-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("sliding.mp4");
        let status = std::process::Command::new(&tool)
            .args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", "nullsrc=s=640x360:r=30:d=2,geq=lum='128+50*sin(X/9)*cos(Y/7)':cb=128:cr=128", "-f", "lavfi", "-i", "testsrc2=s=64x48:r=30:d=2"])
            .args(["-filter_complex", "[0][1]overlay=x='80+160*t':y='150+40*sin(2*t)':eval=frame", "-c:v", "libopenh264", "-pix_fmt", "yuv420p", "-b:v", "6M"])
            .arg(&clip)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "ffmpeg made the clip");
        // The middle of the pattern at frame `i`, in pixels (pixel centres are whole numbers).
        let truth = |i: f64| {
            let t = i / 30.0;
            ((80.0 + 160.0 * t).floor() + 31.5, (150.0 + 40.0 * (2.0 * t).sin()).floor() + 23.5)
        };
        let start = TrackSample { source_time: Time::ZERO, cx: (80.0 + 32.0) / 640.0, cy: (150.0 + 24.0) / 360.0, w: 64.0 / 640.0, h: 48.0 / 360.0, angle: 0.0, confidence: 1.0, manual: true };
        let track = MotionTrack { id: 5, media_path: clip.to_string_lossy().into_owned(), aspect: 16.0 / 9.0, samples: vec![start] };
        let fast = RunRequest::for_track(&track, None, Direction::Forward, false).unwrap();
        let mut requests = vec![fast.clone()];
        let model = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/FFmpegGpuFilter/build/packages/gpufx_ai_tracking/nanotrack-v2.zip");
        if !model.is_file() {
            eprintln!("skipping the robust tracker: nanotrack-v2.zip is not built");
        } else if let Err(error) = tracking_gpu() {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {error}");
            eprintln!("skipping the robust tracker: {error}");
        } else {
            requests.push(RunRequest { mode: TrackMode::Robust, model: Some(model), ..fast });
        }
        for request in requests {
            let mode = request.mode;
            let path = clip.clone();
            let mut runner = TrackRunner::default();
            let id = runner.start(request, move || FileFrames::open(&path).map(|frames| Box::new(frames) as Box<dyn FrameSource>)).unwrap();
            runner.wait(id);
            let Some(RunState::Finished(result)) = runner.status(id) else { panic!("{mode:?}: {:?}", runner.status(id)) };
            assert_eq!(result.end, RunEnd::Finished, "{mode:?}");
            assert!(result.samples.len() >= 55, "{mode:?}: {} samples", result.samples.len());
            // NanoTrack places its box from score cells 16 search pixels apart: a few pixels coarser.
            let (most, mean_most) = if mode == TrackMode::Fast { (3.0, 1.5) } else { (8.0, 4.0) };
            let (mut worst, mut sum) = (0.0f64, 0.0f64);
            for sample in &result.samples {
                let frame = frame_index(sample.source_time, (30, 1)) as f64;
                let (x, y) = truth(frame);
                let error = ((sample.cx * 640.0 - 0.5 - x).powi(2) + (sample.cy * 360.0 - 0.5 - y).powi(2)).sqrt();
                worst = worst.max(error);
                sum += error;
                assert!(error < most, "{mode:?} frame {frame}: box at ({}, {}), pattern at ({x}, {y})", sample.cx * 640.0 - 0.5, sample.cy * 360.0 - 0.5);
                assert!((sample.w * 640.0 - 64.0).abs() < 16.0 && (sample.h * 360.0 - 48.0).abs() < 12.0, "{mode:?} frame {frame}: size {} x {}", sample.w * 640.0, sample.h * 360.0);
                assert!(sample.confidence >= engine::tracker::LOST_CONFIDENCE, "{mode:?} frame {frame}: confidence {}", sample.confidence);
            }
            let mean = sum / result.samples.len() as f64;
            eprintln!("{mode:?}: worst error {worst:.2} px, mean {mean:.2} px over {} samples", result.samples.len());
            assert!(mean < mean_most, "{mode:?}: mean error {mean} px");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
