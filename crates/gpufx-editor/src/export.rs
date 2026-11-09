//! Export on a thread of its own. The render path itself is `d3d12_backend::ExportJob`, shared with
//! the CLI; this module only runs it in the background and reports. The UI must have stopped the
//! preview and playback first, and passes the receiver that says the preview has let go.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use d3d12_backend::{EncoderChoice, ExportControl, ExportError, ExportJob, ExportReport, Gpu, AUTO_ENCODERS};
use engine::{frame_count, Project};

const NOTHING_TO_EXPORT: &str = "Nothing to export: the timeline has no video frames";

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
pub fn export_blocker(project: &Project) -> Option<&'static str> {
    (frame_count(project) <= 0).then_some(NOTHING_TO_EXPORT)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderOption {
    pub label: String,
    pub choice: EncoderChoice,
    pub available: bool,
}

/// Auto and every encoder the CLI offers, each marked with whether this FFmpeg build has it.
pub fn encoder_options(exists: impl Fn(&str) -> bool, gpl: bool) -> Vec<EncoderOption> {
    let mut options = vec![EncoderOption { label: "Auto".to_owned(), choice: EncoderChoice::Auto, available: true }];
    for name in AUTO_ENCODERS {
        let available = exists(name);
        let label = match (available, name == "libx264" && !gpl) {
            (true, _) => name.to_owned(),
            (false, true) => format!("{name} (needs the GPL build)"),
            (false, false) => format!("{name} (not in this build)"),
        };
        options.push(EncoderOption { label, choice: EncoderChoice::Named(name.to_owned()), available });
    }
    options
}

/// The status line after a finished export. A project without an audio output has no audio stream,
/// exactly as the CLI writes it.
pub fn finished_message(report: &ExportReport, out: &Path, has_audio: bool) -> String {
    let audio = if has_audio { "" } else { " The project has no audio output, so the file has no sound." };
    format!("Exported {} frames to {} with {}.{audio}", report.frames, out.display(), report.encoder)
}

/// How an export thread ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    Finished(ExportReport),
    Cancelled,
    Failed(String),
}

pub struct ExportWorker {
    done: Arc<AtomicI64>,
    total: i64,
    cancel: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<ExportOutcome>>>,
    handle: Option<JoinHandle<()>>,
}

impl ExportWorker {
    /// Starts rendering `job` once `preview_released` has fired. `wake` is called when it ends.
    pub fn start(job: ExportJob, gpu: Arc<Gpu>, preview_released: Receiver<()>, wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let total = frame_count(&job.project);
        let done: Arc<AtomicI64> = Arc::default();
        let cancel: Arc<AtomicBool> = Arc::default();
        let outcome: Arc<Mutex<Option<ExportOutcome>>> = Arc::default();
        let (thread_done, thread_cancel, thread_outcome) = (done.clone(), cancel.clone(), outcome.clone());
        let handle = thread::Builder::new().name("export".to_owned()).spawn(move || {
            // Waits for the preview to drop its decoders and images; an error only means it is gone.
            let _ = preview_released.recv();
            let run = catch_unwind(AssertUnwindSafe(|| run_job(&job, gpu, thread_cancel, &thread_done)));
            let result = run.unwrap_or_else(|_| ExportOutcome::Failed("the export thread hit an internal error".to_owned()));
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

    /// The outcome, once the thread has finished. The thread has released the GPU by then.
    pub fn poll(&mut self) -> Option<ExportOutcome> {
        let outcome = self.outcome.lock().unwrap_or_else(|p| p.into_inner()).take()?;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        Some(outcome)
    }
}

impl Drop for ExportWorker {
    fn drop(&mut self) {
        self.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_job(job: &ExportJob, gpu: Arc<Gpu>, cancel: Arc<AtomicBool>, done: &AtomicI64) -> ExportOutcome {
    if let Err(message) = job.check() {
        return ExportOutcome::Failed(message);
    }
    if cancel.load(Ordering::SeqCst) {
        return ExportOutcome::Cancelled;
    }
    let progress = |frames: i64, _total: i64| done.store(frames, Ordering::SeqCst);
    match job.run(gpu, ExportControl { cancel: Some(cancel), progress: Some(&progress) }) {
        Ok(report) => ExportOutcome::Finished(report),
        Err(ExportError::Cancelled) => ExportOutcome::Cancelled,
        Err(ExportError::Failed(message)) => ExportOutcome::Failed(message),
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
        let clip = Clip { source: "a.mp4".into(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(9000), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO };
        let kind = if video { TrackKind::Video } else { TrackKind::Audio };
        Project { version: SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: None }, tracks: vec![Track { kind, clips: vec![clip], transitions: Vec::new() }] }
    }

    #[test]
    fn a_timeline_without_frames_cannot_be_exported() {
        assert_eq!(export_blocker(&project(false)), Some(NOTHING_TO_EXPORT));
        assert_eq!(export_blocker(&project(true)), None);
    }

    #[test]
    fn the_lgpl_build_says_libx264_needs_gpl() {
        let options = encoder_options(|name| name != "libx264", false);
        let x264 = options.iter().find(|o| o.choice == EncoderChoice::Named("libx264".into())).unwrap();
        assert!(!x264.available && x264.label.contains("GPL"));
        assert!(options[0].available, "Auto is always offered");
    }

    #[test]
    fn a_missing_encoder_in_the_gpl_build_is_not_blamed_on_the_license() {
        let options = encoder_options(|_| false, true);
        let x264 = options.iter().find(|o| o.choice == EncoderChoice::Named("libx264".into())).unwrap();
        assert!(!x264.label.contains("GPL"));
    }

    #[test]
    fn a_project_without_audio_output_says_so_after_export() {
        let report = ExportReport { encoder: "h264_nvenc".into(), frames: 3, audio_samples: 0, clamped_samples: 0, pcm_sha256: None };
        assert!(finished_message(&report, Path::new("a.mp4"), false).contains("no sound"));
        assert!(!finished_message(&report, Path::new("a.mp4"), true).contains("no sound"));
    }
}
