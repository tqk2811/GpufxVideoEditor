//! A small pool of detached threads that make pictures and level data. The UI replaces the queue
//! with what is on screen on each frame, so nothing requested for a clip that went away can pile up.
//!
//! Workers are never joined: a thread stuck in a seek or on a dead network drive must not delay
//! closing the app. The process exit ends such a thread.

use std::any::Any;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::SystemTime;

use d3d12_backend::ThumbnailImage;
use engine::Time;

/// Size and modification time: a file replaced under the same name gets new pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// What a source can be drawn from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceFacts {
    pub has_video: bool,
    pub has_audio: bool,
    /// Width divided by height of the pictures.
    pub aspect: f32,
    /// How long the source runs, when it says.
    pub length: Option<Time>,
}

/// The file work behind the pictures. A trait so the pool can be tested without FFmpeg.
pub trait Generator: Send + Sync + 'static {
    /// What a worker keeps between jobs, such as an open decoder.
    type Local: Default;
    fn stamp(&self, path: &Path) -> Option<FileStamp>;
    fn inspect(&self, path: &Path) -> Result<SourceFacts, String>;
    fn frame(&self, local: &mut Self::Local, path: &Path, at: Time, cancelled: &dyn Fn() -> bool) -> Result<ThumbnailImage, String>;
    /// Calls `emit` with further peaks until it returns false or the audio ends.
    fn peaks(&self, path: &Path, emit: &mut dyn FnMut(&[f32]) -> bool) -> Result<(), String>;
}

/// Which job is meant, for de-duplicating them. Frames and peaks name the identity the UI gave a
/// source, so a late result for a replaced file is told apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobKey {
    Stamp(u32),
    Inspect(u32),
    Frame(u32, i64),
    Peaks(u32),
}

#[derive(Debug, Clone)]
pub struct Job {
    pub key: JobKey,
    pub path: Arc<Path>,
}

/// A finished job or part of one, for the UI thread.
#[derive(Debug)]
pub enum Done {
    Stamp { slot: u32, stamp: Option<FileStamp> },
    Inspected { slot: u32, stamp: Option<FileStamp>, result: Result<SourceFacts, String> },
    Frame { identity: u32, bucket: i64, result: Result<ThumbnailImage, String> },
    Peaks { identity: u32, peaks: Vec<f32>, finished: bool },
}

#[derive(Default)]
struct Queues {
    frames: Vec<Job>,
    audio: Vec<Job>,
    running: HashSet<JobKey>,
    done: Vec<Done>,
    cancelled_peaks: HashSet<u32>,
}

pub type Waker = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    queues: Mutex<Queues>,
    work: Condvar,
    shutdown: AtomicBool,
    /// Set from the first result after the UI last looked, so a burst repaints only once.
    woken: AtomicBool,
    alive: AtomicUsize,
    waker: Option<Waker>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn deliver(&self, done: Done) {
        self.lock().done.push(done);
        self.wake();
    }

    fn wake(&self) {
        if !self.woken.swap(true, Ordering::AcqRel) {
            if let Some(waker) = &self.waker {
                waker();
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Lane {
    Frames,
    Audio,
}

/// Where a frame worker stands, so a job that continues its file comes first.
type Cursor = Option<(Arc<Path>, i64)>;

/// A running pool. Dropping it stops the workers without waiting for them.
pub struct WorkerPool {
    shared: Arc<Shared>,
}

impl WorkerPool {
    /// Starts `frame_workers` threads for pictures and inspection and `audio_workers` for levels.
    pub fn start<G: Generator>(generator: Arc<G>, frame_workers: usize, audio_workers: usize, waker: Option<Waker>) -> Self {
        let shared = Arc::new(Shared {
            queues: Mutex::new(Queues::default()),
            work: Condvar::new(),
            shutdown: AtomicBool::new(false),
            woken: AtomicBool::new(false),
            alive: AtomicUsize::new(0),
            waker,
        });
        for (count, lane) in [(frame_workers, Lane::Frames), (audio_workers, Lane::Audio)] {
            for index in 0..count {
                let worker_shared = shared.clone();
                let generator = generator.clone();
                shared.alive.fetch_add(1, Ordering::AcqRel);
                // A thread that does not start only means fewer pictures at once.
                let _ = std::thread::Builder::new().name(format!("clip-visuals-{index}")).spawn(move || {
                    run_worker(&worker_shared, &*generator, lane);
                    worker_shared.alive.fetch_sub(1, Ordering::AcqRel);
                });
            }
        }
        Self { shared }
    }

    /// Replaces the waiting jobs with `frames` and `audio`, minus what is already running.
    pub fn publish(&self, frames: Vec<Job>, audio: Vec<Job>) {
        let mut queues = self.shared.lock();
        for job in &audio {
            if let JobKey::Peaks(identity) = job.key {
                queues.cancelled_peaks.remove(&identity);
            }
        }
        let running = std::mem::take(&mut queues.running);
        queues.frames = frames.into_iter().filter(|job| !running.contains(&job.key)).collect();
        queues.audio = audio.into_iter().filter(|job| !running.contains(&job.key)).collect();
        queues.running = running;
        drop(queues);
        self.shared.work.notify_all();
    }

    /// Takes what finished. A result delivered after the flag is cleared wakes the UI again.
    pub fn take_done(&self) -> Vec<Done> {
        self.shared.woken.store(false, Ordering::Release);
        std::mem::take(&mut self.shared.lock().done)
    }

    /// Stops reading levels of `identity`: its clip is gone.
    pub fn cancel_peaks(&self, identity: u32) {
        self.shared.lock().cancelled_peaks.insert(identity);
    }

    /// How many jobs wait or run, for tests and the frame report.
    pub fn outstanding(&self) -> usize {
        let queues = self.shared.lock();
        queues.frames.len() + queues.audio.len() + queues.running.len()
    }

    #[cfg(test)]
    fn alive(&self) -> usize {
        self.shared.alive.load(Ordering::Acquire)
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        let mut queues = self.shared.lock();
        queues.frames.clear();
        queues.audio.clear();
        drop(queues);
        self.shared.work.notify_all();
    }
}

/// Runs jobs until the pool is dropped.
fn run_worker<G: Generator>(shared: &Shared, generator: &G, lane: Lane) {
    let mut local = G::Local::default();
    let mut cursor: Cursor = None;
    loop {
        let job = {
            let mut queues = shared.lock();
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                let picked = match lane {
                    Lane::Frames => pick_job(&mut queues.frames, &cursor),
                    Lane::Audio => (!queues.audio.is_empty()).then(|| queues.audio.remove(0)),
                };
                if let Some(job) = picked {
                    queues.running.insert(job.key.clone());
                    break job;
                }
                queues = shared.work.wait(queues).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        if let JobKey::Frame(_, bucket) = job.key {
            cursor = Some((job.path.clone(), bucket));
        }
        let done = match catch_unwind(AssertUnwindSafe(|| run_job(shared, generator, &mut local, &job))) {
            Ok(done) => done,
            Err(payload) => {
                local = G::Local::default();
                failure(&job, &panic_message(&payload))
            }
        };
        let mut queues = shared.lock();
        queues.running.remove(&job.key);
        queues.done.push(done);
        drop(queues);
        shared.wake();
    }
}

/// The next frame job: inspections first, then a picture continuing the file this worker has
/// open, then the first on the list.
fn pick_job(jobs: &mut Vec<Job>, cursor: &Cursor) -> Option<Job> {
    if let Some(index) = jobs.iter().position(|job| !matches!(job.key, JobKey::Frame(..))) {
        return Some(jobs.remove(index));
    }
    let continuing = cursor.as_ref().and_then(|(path, at)| {
        let forward = jobs.iter().enumerate().filter(|(_, job)| job.path == *path).filter_map(|(index, job)| match job.key {
            JobKey::Frame(_, bucket) if bucket >= *at => Some((bucket, index)),
            _ => None,
        });
        forward.min().map(|(_, index)| index)
    });
    match continuing {
        Some(index) => Some(jobs.remove(index)),
        None => (!jobs.is_empty()).then(|| jobs.remove(0)),
    }
}

fn run_job<G: Generator>(shared: &Shared, generator: &G, local: &mut G::Local, job: &Job) -> Done {
    match job.key {
        JobKey::Stamp(slot) => Done::Stamp { slot, stamp: generator.stamp(&job.path) },
        JobKey::Inspect(slot) => {
            let stamp = generator.stamp(&job.path);
            Done::Inspected { slot, stamp, result: generator.inspect(&job.path) }
        }
        JobKey::Frame(identity, bucket) => {
            let cancelled = || shared.shutdown.load(Ordering::Acquire);
            let result = generator.frame(local, &job.path, Time::from_ticks(bucket), &cancelled);
            Done::Frame { identity, bucket, result }
        }
        JobKey::Peaks(identity) => {
            // What was read before an error is kept and drawn; only a cancel leaves it unfinished.
            let _ = generator.peaks(&job.path, &mut |peaks| {
                if shared.shutdown.load(Ordering::Acquire) || shared.lock().cancelled_peaks.contains(&identity) {
                    return false;
                }
                shared.deliver(Done::Peaks { identity, peaks: peaks.to_vec(), finished: false });
                true
            });
            let cancelled = shared.lock().cancelled_peaks.contains(&identity);
            Done::Peaks { identity, peaks: Vec::new(), finished: !cancelled }
        }
    }
}

fn failure(job: &Job, message: &str) -> Done {
    match job.key {
        JobKey::Stamp(slot) => Done::Stamp { slot, stamp: None },
        JobKey::Inspect(slot) => Done::Inspected { slot, stamp: None, result: Err(message.to_owned()) },
        JobKey::Frame(identity, bucket) => Done::Frame { identity, bucket, result: Err(message.to_owned()) },
        JobKey::Peaks(identity) => Done::Peaks { identity, peaks: Vec::new(), finished: true },
    }
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::{Duration, Instant};

    /// Answers instantly, except that frames wait while `block` is set.
    #[derive(Default)]
    struct Fake {
        calls: AtomicU32,
        block: AtomicBool,
        panic_at: Option<i64>,
    }

    impl Generator for Fake {
        type Local = ();

        fn stamp(&self, _: &Path) -> Option<FileStamp> {
            None
        }

        fn inspect(&self, _: &Path) -> Result<SourceFacts, String> {
            Ok(SourceFacts { has_video: true, has_audio: false, aspect: 1.0, length: None })
        }

        fn frame(&self, _: &mut (), _: &Path, at: Time, _: &dyn Fn() -> bool) -> Result<ThumbnailImage, String> {
            assert_ne!(Some(at.ticks()), self.panic_at, "boom");
            self.calls.fetch_add(1, Ordering::SeqCst);
            while self.block.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(ThumbnailImage { width: 1, height: 1, rgba: vec![0; 4] })
        }

        fn peaks(&self, _: &Path, emit: &mut dyn FnMut(&[f32]) -> bool) -> Result<(), String> {
            while emit(&[0.5]) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        }
    }

    fn frame(identity: u32, bucket: i64) -> Job {
        Job { key: JobKey::Frame(identity, bucket), path: Path::new("a.mp4").into() }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_worker_continues_forward_in_the_file_it_has_open() {
        let mut jobs = vec![frame(1, 500), frame(1, 300), frame(1, 100)];
        let cursor: Cursor = Some((Path::new("a.mp4").into(), 200));
        assert_eq!(pick_job(&mut jobs, &cursor).expect("a job").key, JobKey::Frame(1, 300));
        assert_eq!(pick_job(&mut jobs, &cursor).expect("a job").key, JobKey::Frame(1, 500), "forward first");
    }

    #[test]
    fn inspection_goes_before_pictures() {
        let mut jobs = vec![frame(1, 500), Job { key: JobKey::Inspect(2), path: Path::new("b.mp4").into() }];
        assert_eq!(pick_job(&mut jobs, &None).expect("a job").key, JobKey::Inspect(2));
    }

    #[test]
    fn jobs_that_are_no_longer_wanted_are_not_run() {
        let generator = Arc::new(Fake::default());
        generator.block.store(true, Ordering::SeqCst);
        let pool = WorkerPool::start(generator.clone(), 1, 0, None);
        pool.publish(vec![frame(1, 0)], Vec::new());
        wait_for(|| generator.calls.load(Ordering::SeqCst) == 1);
        // A waiting pile for clips scrolled away, then replaced by nothing.
        pool.publish((1..100).map(|i| frame(1, i)).collect(), Vec::new());
        pool.publish(Vec::new(), Vec::new());
        generator.block.store(false, Ordering::SeqCst);
        wait_for(|| pool.outstanding() == 0);
        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_running_job_is_not_started_twice() {
        let generator = Arc::new(Fake::default());
        generator.block.store(true, Ordering::SeqCst);
        let pool = WorkerPool::start(generator.clone(), 2, 0, None);
        pool.publish(vec![frame(1, 0)], Vec::new());
        wait_for(|| generator.calls.load(Ordering::SeqCst) == 1);
        pool.publish(vec![frame(1, 0)], Vec::new());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
        generator.block.store(false, Ordering::SeqCst);
        wait_for(|| pool.outstanding() == 0);
    }

    #[test]
    fn dropping_the_pool_does_not_wait_for_a_stuck_job() {
        let generator = Arc::new(Fake::default());
        generator.block.store(true, Ordering::SeqCst);
        let pool = WorkerPool::start(generator.clone(), 1, 0, None);
        pool.publish(vec![frame(1, 0)], Vec::new());
        wait_for(|| generator.calls.load(Ordering::SeqCst) == 1);
        assert_eq!(pool.alive(), 1);
        let started = Instant::now();
        drop(pool);
        assert!(started.elapsed() < Duration::from_millis(500), "shutting down waited for the worker");
        generator.block.store(false, Ordering::SeqCst);
    }

    #[test]
    fn a_panic_is_reported_as_a_failure_and_the_worker_survives() {
        let generator = Arc::new(Fake { panic_at: Some(7), ..Fake::default() });
        let pool = WorkerPool::start(generator.clone(), 1, 0, None);
        pool.publish(vec![frame(1, 7), frame(1, 8)], Vec::new());
        wait_for(|| pool.outstanding() == 0);
        let done = pool.take_done();
        assert!(done.iter().any(|d| matches!(d, Done::Frame { bucket: 7, result: Err(_), .. })), "{done:?}");
        assert!(done.iter().any(|d| matches!(d, Done::Frame { bucket: 8, result: Ok(_), .. })), "{done:?}");
    }

    #[test]
    fn a_burst_of_results_wakes_the_ui_once() {
        let wakes = Arc::new(AtomicU32::new(0));
        let count = wakes.clone();
        let generator = Arc::new(Fake::default());
        let waker: Waker = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let pool = WorkerPool::start(generator, 1, 0, Some(waker));
        pool.publish((0..20).map(|i| frame(1, i)).collect(), Vec::new());
        wait_for(|| pool.outstanding() == 0);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(pool.take_done().len(), 20);
    }

    #[test]
    fn levels_of_a_removed_clip_stop_being_read() {
        let generator = Arc::new(Fake::default());
        let pool = WorkerPool::start(generator, 0, 1, None);
        pool.publish(Vec::new(), vec![Job { key: JobKey::Peaks(3), path: Path::new("a.mp4").into() }]);
        wait_for(|| pool.lock_for_test().done.iter().any(|d| matches!(d, Done::Peaks { .. })));
        pool.cancel_peaks(3);
        wait_for(|| pool.outstanding() == 0);
        let done = pool.take_done();
        assert!(done.iter().any(|d| matches!(d, Done::Peaks { finished: false, peaks, .. } if peaks.is_empty())), "{done:?}");
        assert!(!done.iter().any(|d| matches!(d, Done::Peaks { finished: true, .. })), "{done:?}");
    }

    impl WorkerPool {
        fn lock_for_test(&self) -> MutexGuard<'_, Queues> {
            self.shared.lock()
        }
    }
}
