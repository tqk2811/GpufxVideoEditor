//! Tracker runs in the background: each follows the box of one sample of a motion track frame by
//! frame through the file (its proxy when it has one), forwards or backwards, until the next
//! sample placed by hand, the end of the file, a lost step, or a cancel. Frames are decoded in
//! software at [`ANALYSIS_HEIGHT`] pixels, upright, and HDR sources tone mapped to SDR (see
//! [`FrameSampler`]), then turned gray. Nothing here touches the project: the editor writes the
//! samples of a finished run as one undo step ([`engine::apply_track_run`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use d3d12_backend::{probe_media, probe_video_format, FrameSampler};
use engine::tracker::{track_frames, GrayImage, RunEnd, TrackBox, Tracker, TrackerOptions};
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
    /// The frame shown at source time `at`, gray.
    fn frame_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<GrayImage, String>;
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

    fn frame_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<GrayImage, String> {
        let picture = self.sampler.picture_at(at, cancelled).map_err(|error| format!("{error:?}"))?;
        Ok(GrayImage::from_rgba(picture.width as usize, picture.height as usize, &picture.rgba))
    }
}

/// What a run is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRequest {
    pub track_id: u32,
    /// The sample the run starts from.
    pub start: TrackSample,
    pub direction: Direction,
    /// The source time of the next sample placed by hand that way, where the run stops before.
    pub stop: Option<Time>,
    pub rotation: bool,
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
        Ok(Self { track_id: track.id, start, direction, stop, rotation })
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
    while times.len() < MAX_RUN_FRAMES {
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

/// How a run ended, with the samples it made, in the order made.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    pub track_id: u32,
    pub samples: Vec<TrackSample>,
    pub end: RunEnd,
}

/// What a finished run did, kept for an agent that asks later.
#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    pub track_id: u32,
    /// `done`, `lost`, `cancelled` or `failed`.
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
    let first = source.frame_at(start_time, &cancelled)?;
    let (width, height) = (first.width, first.height);
    let area = TrackBox::from_sample(&request.start, width, height);
    let options = TrackerOptions { rotation: request.rotation, ..TrackerOptions::default() };
    let mut tracker = Tracker::new(first, area, options);
    let frames = Frames { source, times: &times, next: 0, direction: request.direction, ahead: Vec::new(), cancel: &shared.cancel };
    let (steps, end) = track_frames(&mut tracker, frames, &shared.cancel, |index, _| shared.done.store(index + 1, Ordering::Relaxed))?;
    let samples = steps.iter().zip(&times).map(|(step, at)| step.area.to_sample(width, height, *at, step.confidence)).collect();
    Ok(RunResult { track_id: request.track_id, samples, end })
}

/// The frames at `times`, in that order; backwards they are read a chunk at a time in file order.
struct Frames<'a> {
    source: &'a mut dyn FrameSource,
    times: &'a [Time],
    next: usize,
    direction: Direction,
    /// Frames read ahead, the next one last.
    ahead: Vec<GrayImage>,
    cancel: &'a AtomicBool,
}

impl Iterator for Frames<'_> {
    type Item = Result<GrayImage, String>;

    fn next(&mut self) -> Option<Self::Item> {
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
        let frame = match self.direction {
            Direction::Backward => self.ahead.pop().map(Ok),
            Direction::Forward => Some(self.source.frame_at(self.times[self.next], &cancelled)),
        };
        self.next += 1;
        frame
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

        fn frame_at(&mut self, at: Time, _: &dyn Fn() -> bool) -> Result<GrayImage, String> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let frame = frame_index(at, (30, 1)) as f64;
            let left = 40.0 + self.speed * frame;
            let (width, height) = (160, 90);
            let mut data = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let inside = (x as f64) >= left && (x as f64) < left + 30.0 && (30..60).contains(&y);
                    data.push(if inside { texture(x as f64 - left, y as f64) } else { 0.5 });
                }
            }
            Ok(GrayImage::new(width, height, data))
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
    }

    #[test]
    fn the_times_of_a_fractional_rate_land_on_their_frames() {
        let rate = (30_000, 1001);
        for index in [0, 1, 7, 1000, 107_892] {
            assert_eq!(frame_index(frame_time(index, rate), rate), index);
        }
    }

    fn run_on(source: Sliding, request: RunRequest) -> (u64, TrackRunner) {
        let mut runner = TrackRunner::default();
        let id = runner.start(request, move || Ok(Box::new(source) as Box<dyn FrameSource>)).unwrap();
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
    }

    impl FrameSource for Gated {
        fn timing(&self) -> (Option<(i32, i32)>, Option<Time>) {
            self.inner.timing()
        }

        fn frame_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<GrayImage, String> {
            let (allowed, signal) = &*self.gate;
            let mut allowed = allowed.lock().unwrap();
            while *allowed == 0 && !cancelled() {
                allowed = signal.wait_timeout(allowed, Duration::from_millis(5)).unwrap().0;
            }
            *allowed = allowed.saturating_sub(1);
            drop(allowed);
            self.inner.frame_at(at, cancelled)
        }
    }

    #[test]
    fn a_cancelled_run_ends_with_the_samples_made_so_far() {
        let gate = Arc::new((Mutex::new(4usize), Condvar::new()));
        let source = Gated { inner: Sliding { speed: 1.0, reads: Arc::default() }, gate: gate.clone() };
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
}
