//! Export in a child process: `gpufx-render --progress json` renders the project, so a crash there
//! does not take the editor down. A lost device is not proven isolated: an adapter reset can hit
//! several processes. [`child`] runs it; this module holds what the UI needs and the thread that
//! follows the child. The render path itself is `d3d12_backend::ExportJob`.
//!
//! The preview and playback still stop during an export: the child needs the GPU memory and video
//! decoders too, and editing stays off because the export shows the project it started with.

pub mod child;
pub mod machine;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use d3d12_backend::{EncoderChoice, ExportJob, ExportReport, AUTO_ENCODERS};
use engine::{frame_count, Project};

use crate::i18n::Message;

pub use child::sibling_render_exe;
pub use machine::ExportOutcome;

/// How long closing the app waits for a cancelled export to stop.
const DROP_WAIT: Duration = Duration::from_secs(2);

/// Frames written so far, of the frames in the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportProgress {
    pub done: i64,
    pub total: i64,
}

impl ExportProgress {
    pub fn fraction(&self) -> f32 {
        if self.total <= 0 {
            return 0.0;
        }
        (self.done.clamp(0, self.total) as f64 / self.total as f64) as f32
    }
}

/// Why the project cannot be exported as it is, if so.
pub fn export_blocker(project: &Project) -> Option<Message> {
    (frame_count(project) <= 0).then_some(Message::NothingToExport)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderOption {
    pub label: Message,
    pub choice: EncoderChoice,
    pub available: bool,
}

/// Auto and every encoder the CLI offers, each marked with whether this FFmpeg build has it.
pub fn encoder_options(exists: impl Fn(&str) -> bool, gpl: bool) -> Vec<EncoderOption> {
    let mut options = vec![EncoderOption { label: Message::EncoderAuto, choice: EncoderChoice::Auto, available: true }];
    for name in AUTO_ENCODERS {
        let available = exists(name);
        let label = match (available, name == "libx264" && !gpl) {
            (true, _) => Message::Technical(name.to_owned()),
            (false, true) => Message::EncoderNeedsGpl(name.to_owned()),
            (false, false) => Message::EncoderMissing(name.to_owned()),
        };
        options.push(EncoderOption { label, choice: EncoderChoice::Named(name.to_owned()), available });
    }
    options
}

/// The status line after a finished export. A project without an audio output has no audio stream,
/// exactly as the CLI writes it.
pub fn finished_message(report: &ExportReport, out: &Path, has_audio: bool) -> Message {
    let (frames, path, encoder) = (report.frames, out.display().to_string(), report.encoder.clone());
    if has_audio {
        Message::ExportFinished { frames, path, encoder }
    } else {
        Message::ExportFinishedSilent { frames, path, encoder }
    }
}

pub struct ExportWorker {
    done: Arc<AtomicI64>,
    total: i64,
    cancel: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<ExportOutcome>>>,
    handle: Option<JoinHandle<()>>,
}

impl ExportWorker {
    /// Runs `job` through the renderer at `tool` once `preview_released` has fired, so the child
    /// and the preview do not hold GPU memory at the same time. `wake` is called when it ends.
    pub fn start(job: ExportJob, package_roots: Vec<PathBuf>, tool: PathBuf, preview_released: Receiver<()>, wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        Self::spawn(job, package_roots, tool, preview_released, wake, child::run_child)
    }

    /// Like [`Self::start`] for a server with no window: see [`child::run_child_headless`].
    pub fn start_headless(job: ExportJob, package_roots: Vec<PathBuf>, tool: PathBuf, preview_released: Receiver<()>, wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        Self::spawn(job, package_roots, tool, preview_released, wake, child::run_child_headless)
    }

    fn spawn(job: ExportJob, package_roots: Vec<PathBuf>, tool: PathBuf, preview_released: Receiver<()>, wake: impl Fn() + Send + 'static, run: fn(&ExportJob, &[PathBuf], &Path, &AtomicBool, &AtomicI64, Duration) -> ExportOutcome) -> Result<Self, String> {
        let total = frame_count(&job.project);
        let done: Arc<AtomicI64> = Arc::default();
        let cancel: Arc<AtomicBool> = Arc::default();
        let outcome: Arc<Mutex<Option<ExportOutcome>>> = Arc::default();
        let (thread_done, thread_cancel, thread_outcome) = (done.clone(), cancel.clone(), outcome.clone());
        let handle = thread::Builder::new().name("export".to_owned()).spawn(move || {
            // Waits for the preview to drop its decoders and images; an error only means it is gone.
            let _ = preview_released.recv();
            let outcome_of = catch_unwind(AssertUnwindSafe(|| run(&job, &package_roots, &tool, &thread_cancel, &thread_done, child::CANCEL_GRACE)));
            let result = outcome_of.unwrap_or_else(|_| ExportOutcome::Failed(Message::ExportFailed("the export thread hit an internal error".to_owned())));
            *thread_outcome.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
            wake();
        });
        let handle = handle.map_err(|e| format!("the export thread could not start: {e}"))?;
        Ok(Self { done, total, cancel, outcome, handle: Some(handle) })
    }

    pub fn progress(&self) -> ExportProgress {
        ExportProgress { done: self.done.load(Ordering::SeqCst), total: self.total }
    }

    /// Asks the export to stop; [`Self::poll`] reports when it has.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// The outcome, once the child and the thread have finished.
    pub fn poll(&mut self) -> Option<ExportOutcome> {
        let outcome = self.outcome.lock().unwrap_or_else(|p| p.into_inner()).take()?;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        Some(outcome)
    }
}

impl Drop for ExportWorker {
    /// Runs on the UI thread as the app closes, so the wait is bounded. A thread still following a
    /// stuck child is left behind; the job object stops that child when the app exits.
    fn drop(&mut self) {
        self.cancel();
        let Some(handle) = self.handle.take() else { return };
        let started = Instant::now();
        while !handle.is_finished() {
            if started.elapsed() >= DROP_WAIT {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Clip, Output, Time, Track, TrackKind, SUPPORTED_VERSION};

    #[test]
    fn progress_is_a_fraction_even_with_no_frames() {
        assert_eq!(ExportProgress { done: 45, total: 90 }.fraction(), 0.5);
        assert_eq!(ExportProgress { done: 0, total: 0 }.fraction(), 0.0);
        assert_eq!(ExportProgress { done: 91, total: 90 }.fraction(), 1.0);
    }

    fn project(video: bool) -> Project {
        let clip = Clip { source: "a.mp4".into(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(9000), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None, animation: Default::default() };
        let kind = if video { TrackKind::Video } else { TrackKind::Audio };
        Project { version: SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: None }, tracks: vec![Track { kind, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false, subtitle: None }] }
    }

    #[test]
    fn a_timeline_without_frames_cannot_be_exported() {
        assert_eq!(export_blocker(&project(false)), Some(Message::NothingToExport));
        assert_eq!(export_blocker(&project(true)), None);
    }

    #[test]
    fn the_lgpl_build_says_libx264_needs_gpl() {
        let options = encoder_options(|name| name != "libx264", false);
        let x264 = options.iter().find(|o| o.choice == EncoderChoice::Named("libx264".into())).unwrap();
        assert!(!x264.available);
        assert_eq!(x264.label, Message::EncoderNeedsGpl("libx264".to_owned()));
        assert!(options[0].available, "Auto is always offered");
    }

    #[test]
    fn a_missing_encoder_in_the_gpl_build_is_not_blamed_on_the_license() {
        let options = encoder_options(|_| false, true);
        let x264 = options.iter().find(|o| o.choice == EncoderChoice::Named("libx264".into())).unwrap();
        assert_eq!(x264.label, Message::EncoderMissing("libx264".to_owned()));
    }

    #[test]
    fn a_project_without_audio_output_says_so_after_export() {
        let report = ExportReport { encoder: "h264_nvenc".into(), frames: 3, audio_samples: 0, clamped_samples: 0, pcm_sha256: None };
        assert!(matches!(finished_message(&report, Path::new("a.mp4"), false), Message::ExportFinishedSilent { .. }));
        assert!(matches!(finished_message(&report, Path::new("a.mp4"), true), Message::ExportFinished { .. }));
    }
}
