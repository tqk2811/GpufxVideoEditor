//! Beat detection of source files, on a thread of its own per file, with a cache per version of the
//! file. The UI starts a detection and polls it each frame; an agent of a session without a window
//! waits for it. Nothing here touches the project: the caller writes the beats to a clip.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use d3d12_backend::{probe_media, AudioFileDecoder};
use engine::beats::{detect_beats, downmix_quarter, BeatAnalysis, ANALYSIS_RATE};
use engine::{MIX_CHANNELS, MIX_SAMPLE_RATE, TICKS_PER_SECOND};

use crate::visuals::worker::FileStamp;

/// The most sound one detection reads: an hour. Past that the rest of the file has no beats.
pub const MAX_SECONDS: i64 = 3600;
/// Frames decoded per read: a second at the mixing rate.
const READ_FRAMES: usize = MIX_SAMPLE_RATE as usize;
/// The part of the progress bar decoding takes; detection takes the rest.
const DECODE_SHARE: f32 = 0.6;

/// Where the detection of a file stands.
#[derive(Debug, Clone, PartialEq)]
pub enum BeatStatus {
    /// Nothing is known or running for it.
    Idle,
    /// Running, this far from 0 to 1.
    Running(f32),
    Ready(Arc<BeatAnalysis>),
    Failed(String),
}

/// What one running detection shares with its thread.
#[derive(Default)]
struct Shared {
    /// The progress as the bits of an `f32`.
    progress: AtomicU32,
    cancel: AtomicBool,
    /// `None` while it runs; `Some(None)` when it was cancelled.
    result: Mutex<Option<Option<Result<BeatAnalysis, String>>>>,
    done: Condvar,
}

impl Shared {
    fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    fn set_progress(&self, value: f32) {
        self.progress.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn finish(&self, result: Option<Result<BeatAnalysis, String>>) {
        *self.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
        self.done.notify_all();
    }
}

struct Job {
    stamp: Option<FileStamp>,
    shared: Arc<Shared>,
}

/// The detections of this session. A finished one stays in the cache for as long as the file keeps
/// its size and modification time; a failed one stays until it is started again.
#[derive(Default)]
pub struct BeatDetector {
    jobs: HashMap<PathBuf, Job>,
    cache: HashMap<PathBuf, (Option<FileStamp>, Result<Arc<BeatAnalysis>, String>)>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn stamp_of(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileStamp { size: metadata.len(), modified: metadata.modified().ok() })
}

impl BeatDetector {
    /// Called from a detection thread when it has finished, so a window repaints.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    /// Takes in the detections that have finished.
    fn collect(&mut self) {
        let finished: Vec<PathBuf> = self.jobs.iter().filter(|(_, job)| job.shared.result.lock().map_or(true, |result| result.is_some())).map(|(path, _)| path.clone()).collect();
        for path in finished {
            let Some(job) = self.jobs.remove(&path) else { continue };
            let result = job.shared.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take().flatten();
            if let Some(result) = result {
                self.cache.insert(path, (job.stamp, result.map(Arc::new)));
            }
        }
    }

    /// Where the detection of `path` stands, without starting one. A cached result of an older
    /// version of the file does not count.
    pub fn status(&mut self, path: &Path) -> BeatStatus {
        self.collect();
        if let Some(job) = self.jobs.get(path) {
            return BeatStatus::Running(job.shared.progress());
        }
        match self.cache.get(path) {
            Some((stamp, result)) if *stamp == stamp_of(path) => match result {
                Ok(analysis) => BeatStatus::Ready(analysis.clone()),
                Err(message) => BeatStatus::Failed(message.clone()),
            },
            _ => BeatStatus::Idle,
        }
    }

    /// Starts detecting `path` unless it runs already or a result for this version of the file
    /// is cached, and says where it stands. A failure is tried again.
    pub fn start(&mut self, path: &Path) -> BeatStatus {
        match self.status(path) {
            BeatStatus::Idle | BeatStatus::Failed(_) => {}
            status => return status,
        }
        let shared = Arc::new(Shared::default());
        let job = Job { stamp: stamp_of(path), shared: shared.clone() };
        let file = path.to_path_buf();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name("beat detection".to_owned()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| analyze_file(&file, &shared.cancel, &mut |value| shared.set_progress(value))))
                .unwrap_or_else(|_| Some(Err("the detection stopped on an internal error".to_owned())));
            shared.finish(result);
            if let Some(waker) = waker {
                waker();
            }
        });
        if let Err(error) = spawned {
            return BeatStatus::Failed(error.to_string());
        }
        self.cache.remove(path);
        self.jobs.insert(path.to_path_buf(), job);
        BeatStatus::Running(0.0)
    }

    /// Starts `path` as [`Self::start`] does and waits until it has finished.
    pub fn wait(&mut self, path: &Path) -> BeatStatus {
        if let BeatStatus::Running(_) = self.start(path) {
            if let Some(job) = self.jobs.get(path) {
                let shared = job.shared.clone();
                let mut result = shared.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                while result.is_none() {
                    result = shared.done.wait(result).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
        }
        self.status(path)
    }

    /// Stops the detection of `path`, if one runs. Nothing is cached for it.
    pub fn cancel(&mut self, path: &Path) {
        if let Some(job) = self.jobs.remove(path) {
            job.shared.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether any detection runs.
    pub fn running(&mut self) -> bool {
        self.collect();
        !self.jobs.is_empty()
    }
}

/// Decodes `path` to mono at [`ANALYSIS_RATE`] and detects its beats. `progress` hears from 0 to 1;
/// `None` when `cancel` was set.
pub fn analyze_file(path: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Option<Result<BeatAnalysis, String>> {
    let samples = match decode_mono(path, cancel, progress) {
        Ok(Some(samples)) => samples,
        Ok(None) => return None,
        Err(message) => return Some(Err(message)),
    };
    let analysis = detect_beats(&samples, ANALYSIS_RATE, &mut |value| {
        progress(DECODE_SHARE + (1.0 - DECODE_SHARE) * value);
        !cancel.load(Ordering::Relaxed)
    })?;
    Some(Ok(analysis))
}

/// The sound of `path` as mono at [`ANALYSIS_RATE`], from the first source sample, at most
/// [`MAX_SECONDS`] of it. `Ok(None)` when cancelled.
fn decode_mono(path: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Result<Option<Vec<f32>>, String> {
    let probe = probe_media(path)?;
    if !probe.has_audio {
        return Err(probe.audio_problem.unwrap_or_else(|| "the file has no sound".to_owned()));
    }
    let length = probe.audio_duration.map(|length| length.ticks() * i64::from(MIX_SAMPLE_RATE) / TICKS_PER_SECOND).filter(|frames| *frames > 0);
    let limit = MAX_SECONDS * i64::from(MIX_SAMPLE_RATE);
    let mut decoder = AudioFileDecoder::open(path)?;
    let mut block = vec![0.0_f32; READ_FRAMES * MIX_CHANNELS];
    let mut samples = Vec::with_capacity(length.unwrap_or(0).min(limit) as usize / 4 + 1);
    let mut position = 0_i64;
    while !decoder.finished() && position < limit {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        block.fill(0.0);
        let covered = decoder.read_counted(position, &mut block)?;
        let frames = if decoder.finished() { covered.min(READ_FRAMES) } else { READ_FRAMES };
        downmix_quarter(&block[..frames * MIX_CHANNELS], &mut samples);
        position += READ_FRAMES as i64;
        if let Some(length) = length {
            progress(DECODE_SHARE * (position as f32 / length.min(limit) as f32).min(1.0));
        }
    }
    Ok(Some(samples))
}
