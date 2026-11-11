//! Renders a project to a video file: the one path `gpufx-render` and the editor both use.

use std::cell::Cell;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use engine::render::{render_with_audio, FrameSink, RenderError};
use engine::{frame_count, validate, AudioSource, Project};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::audio::AudioFiles;
use crate::compositor::D3d12Compositor;
use crate::encoder::{EncoderChoice, Mp4Encoder};
use crate::gpu::Gpu;
use crate::paths::ProjectPaths;

/// Why an export produced no file. A failed or cancelled export leaves the target as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    Cancelled,
    Failed(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Cancelled => write!(f, "the export was cancelled"),
            ExportError::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl From<String> for ExportError {
    fn from(message: String) -> Self {
        ExportError::Failed(message)
    }
}

/// One render to a file.
pub struct ExportJob {
    pub project: Project,
    /// The project on disk, which the output must not overwrite. `None` for an unsaved project.
    pub project_file: Option<PathBuf>,
    pub paths: Arc<ProjectPaths>,
    pub out: PathBuf,
    pub encoder: EncoderChoice,
    /// Where to write `frame sha256-of-RGBA` lines.
    pub hashes: Option<PathBuf>,
}

/// What an export reports while it runs and how it is stopped.
#[derive(Default)]
pub struct ExportControl<'a> {
    /// Set to stop the export; the partial file is removed.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called after each frame with the frames done and the total.
    pub progress: Option<&'a dyn Fn(i64, i64)>,
    /// Called with the encoder's name once it is open, before the first frame.
    pub encoder_opened: Option<&'a dyn Fn(&str)>,
    /// Called with each temporary file as soon as it exists, so a process that may be killed can name it.
    pub reserved: Option<&'a dyn Fn(&Path)>,
    /// Called once, when the export has ended in a result or a failure other than a cancel, before the
    /// GPU is released: a crash or hang while it is released cannot lose the outcome or change what is
    /// on disk. `Err` holds the reason, which names any file kept.
    pub settled: Option<&'a dyn Fn(&Result<ExportReport, String>)>,
}

/// What a finished export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReport {
    pub encoder: String,
    pub frames: i64,
    pub audio_samples: i64,
    pub clamped_samples: u64,
    /// SHA-256 of the mixed samples as little-endian f32; `None` without an audio output.
    pub pcm_sha256: Option<String>,
}

/// `path` in a form two spellings of one file share: canonical when the file exists, else the
/// canonical directory plus the file name, else the absolute path. Lower-cased, as Windows paths
/// are case-insensitive.
fn identity(path: &Path) -> String {
    let resolved = path.canonicalize().ok().or_else(|| {
        let absolute = std::path::absolute(path).ok()?;
        let name = absolute.file_name()?.to_owned();
        Some(absolute.parent()?.canonicalize().ok()?.join(name))
    });
    resolved.or_else(|| std::path::absolute(path).ok()).unwrap_or_else(|| path.to_path_buf()).to_string_lossy().to_lowercase()
}

/// Whether `first` and `second` name one file, however they are spelled.
pub fn same_file(first: &Path, second: &Path) -> bool {
    identity(first) == identity(second)
}

/// The files a render of `project` reads, each with what a message calls it.
fn project_inputs(project: &Project, project_file: Option<&Path>, paths: &ProjectPaths) -> Vec<(PathBuf, &'static str)> {
    let mut inputs: Vec<(PathBuf, &'static str)> = project_file.iter().map(|file| (file.to_path_buf(), "the project file")).collect();
    // A text clip reads no file.
    for clip in project.tracks.iter().flat_map(|t| &t.clips).filter(|clip| !clip.is_text()) {
        inputs.push((paths.source_path(&clip.source), "a source of the project"));
    }
    let packages = project.tracks.iter().flat_map(|t| t.clips.iter().flat_map(|c| c.effects.iter().map(|e| &e.package)).chain(t.transitions.iter().map(|t| &t.package)));
    for package in packages {
        if let Some(path) = paths.package_path(package) {
            inputs.push((path, "a package of the project"));
        }
    }
    inputs
}

/// What of `project` writing to `output` would destroy: a source or a package, by [`same_file`].
pub fn overwritten_input(project: &Project, paths: &ProjectPaths, output: &Path) -> Option<&'static str> {
    project_inputs(project, None, paths).into_iter().find(|(input, _)| same_file(input, output)).map(|(_, what)| what)
}

/// How many names [`reserve_temporary`] tries before giving up.
const RESERVE_ATTEMPTS: u32 = 10_000;

/// Creates an empty sibling of `target` under a name no file had, and returns its path:
/// `movie.mp4` gives `movie.<pid>-<n>.tmp.mp4`, so the extension that picks the container stays
/// last. The file is closed again, so a writer can open it; only its creator may delete it.
pub fn reserve_temporary(target: &Path) -> std::io::Result<PathBuf> {
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tail = target.extension().map_or_else(|| "tmp".to_owned(), |extension| format!("tmp.{}", extension.to_string_lossy()));
    for attempt in 0..RESERVE_ATTEMPTS {
        let candidate = target.with_file_name(format!("{stem}.{}-{attempt}.{tail}", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("no free temporary name next to {}", target.display())))
}

/// A file written under a reserved temporary name that replaces `target` only on
/// [`PendingFile::commit`]. Dropped without a commit, it deletes the file it reserved, so a failed
/// run leaves `target` as it was and touches nothing it did not create.
struct PendingFile {
    temporary: PathBuf,
    target: PathBuf,
    keep: bool,
}

impl PendingFile {
    fn new(target: &Path) -> Result<Self, String> {
        let temporary = reserve_temporary(target).map_err(|e| format!("reserving a temporary file next to {}: {e}", target.display()))?;
        Ok(Self { temporary, target: target.to_path_buf(), keep: false })
    }

    /// Renames the finished temporary file over the target. Close the file first. When the rename
    /// fails (a player holds the target open) the finished file is kept and named in the error.
    fn commit(&mut self) -> Result<(), String> {
        let renamed = std::fs::rename(&self.temporary, &self.target);
        self.keep = true;
        renamed.map_err(|e| format!("the finished file was kept as {}: renaming it to {} failed: {e}", self.temporary.display(), self.target.display()))
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

/// Hashes each frame and the audio, then passes them to the encoder.
struct ExportSink<'a> {
    encoder: Mp4Encoder,
    hashes: Option<std::io::BufWriter<std::fs::File>>,
    pcm: Sha256,
    cancel: Option<&'a AtomicBool>,
    progress: Option<&'a dyn Fn(i64, i64)>,
    total: i64,
}

impl ExportSink<'_> {
    fn check_cancelled(&self) -> Result<(), RenderError> {
        if self.cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
            return Err(RenderError("the export was cancelled".to_owned()));
        }
        Ok(())
    }
}

impl FrameSink for ExportSink<'_> {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        self.check_cancelled()?;
        if let Some(file) = &mut self.hashes {
            let digest: String = Sha256::digest(rgba).iter().map(|b| format!("{b:02x}")).collect();
            writeln!(file, "{index} {digest}").map_err(|e| RenderError(format!("writing the hash file failed: {e}")))?;
        }
        self.encoder.write_frame(index, rgba)?;
        if let Some(progress) = self.progress {
            progress(index + 1, self.total);
        }
        Ok(())
    }

    fn write_audio(&mut self, samples: &[f32]) -> Result<(), RenderError> {
        for sample in samples {
            self.pcm.update(sample.to_le_bytes());
        }
        self.encoder.write_audio(samples)
    }
}

impl ExportJob {
    /// Checks the project and refuses an output that would overwrite one of its inputs. Needs no GPU.
    pub fn check(&self) -> Result<(), String> {
        self.check_named("the output", "the hash file")
    }

    /// [`ExportJob::check`], calling the two outputs `out` and `hashes` in its messages.
    pub fn check_named(&self, out: &str, hashes: &str) -> Result<(), String> {
        self.check_project()?;
        self.check_outputs(out, hashes)
    }

    /// Whether the project itself can be rendered.
    pub fn check_project(&self) -> Result<(), String> {
        let errors = validate(&self.project, self.paths.as_ref());
        if !errors.is_empty() {
            let lines: Vec<String> = errors.iter().map(ToString::to_string).collect();
            return Err(format!("the project is not valid:\n  {}", lines.join("\n  ")));
        }
        Ok(())
    }

    /// Refuses outputs that would overwrite an input, calling them `out` and `hashes` in messages.
    pub fn check_outputs(&self, out: &str, hashes: &str) -> Result<(), String> {
        let inputs = project_inputs(&self.project, self.project_file.as_deref(), &self.paths);
        let mut outputs = vec![(out, &self.out)];
        outputs.extend(self.hashes.as_ref().map(|path| (hashes, path)));
        for (name, output) in &outputs {
            let output_id = identity(output);
            for (input, what) in &inputs {
                if identity(input) == output_id {
                    return Err(format!("{name} {} is {what}; it would be overwritten", output.display()));
                }
            }
        }
        if outputs.len() == 2 && identity(outputs[0].1) == identity(outputs[1].1) {
            return Err(format!("{out} and {hashes} name the same file"));
        }
        Ok(())
    }

    /// Renders the project (which [`ExportJob::check`] passed) to the output file. wgpu errors are
    /// per thread: one from before the render is not this export's, and one from during it is
    /// reported here and not left for whatever this thread renders next.
    pub fn run(&self, gpu: Arc<Gpu>, control: ExportControl<'_>) -> Result<ExportReport, ExportError> {
        gpu.take_error();
        let result = self.render(gpu.clone(), control);
        gpu.take_error();
        result
    }

    /// Every failure is passed to `control.settled` while the compositor, and so the GPU, is still
    /// held, so exactly one event is reported however the export ends and whatever the teardown does.
    fn render(&self, gpu: Arc<Gpu>, control: ExportControl<'_>) -> Result<ExportReport, ExportError> {
        let reported = Cell::new(false);
        let settle = |result: &Result<ExportReport, String>| {
            if let Some(notify) = control.settled {
                reported.set(true);
                notify(result);
            }
        };
        let (width, height) = (self.project.output.width, self.project.output.height);
        let mut compositor = match D3d12Compositor::new(gpu.clone(), self.paths.clone(), width, height) {
            Ok(compositor) => compositor,
            Err(message) => {
                settle(&Err(message.clone()));
                return Err(message.into());
            }
        };
        if let Some(cancel) = &control.cancel {
            compositor.set_abort(cancel.clone(), Duration::ZERO);
            compositor.set_closing(cancel.clone());
        }
        let result = self.render_into_files(&gpu, &mut compositor, &control, &settle);
        if let Err(ExportError::Failed(message)) = &result {
            if !reported.get() {
                settle(&Err(message.clone()));
            }
        }
        result
    }

    /// The render itself. `settle` is called here only for the end of a run that got as far as
    /// renaming; the caller reports any other failure.
    fn render_into_files(&self, gpu: &Gpu, compositor: &mut D3d12Compositor, control: &ExportControl<'_>, settle: &dyn Fn(&Result<ExportReport, String>)) -> Result<ExportReport, ExportError> {
        let cancelled = || control.cancel.as_ref().is_some_and(|cancel| cancel.load(Ordering::SeqCst));
        let mut audio_files = self.project.output.audio.map(|_| AudioFiles::new(self.paths.clone()));
        // Declared before the sink, so they drop after it has closed its files.
        let mut out_file = PendingFile::new(&self.out)?;
        let mut hashes_file = self.hashes.as_deref().map(PendingFile::new).transpose()?;
        if let Some(reserved) = control.reserved {
            reserved(&out_file.temporary);
            if let Some(hashes) = &hashes_file {
                reserved(&hashes.temporary);
            }
        }
        if let Some(hashes) = &hashes_file {
            if identity(&hashes.target) == identity(&out_file.target) {
                return Err("the output and the hash file name the same file".to_owned().into());
            }
        }
        let encoder = Mp4Encoder::create(&out_file.temporary, &self.project.output, &self.encoder)?;
        let encoder_name = encoder.encoder_name().to_owned();
        if let Some(opened) = control.encoder_opened {
            opened(&encoder_name);
        }
        let hashes = hashes_file.as_ref().map(|file| std::fs::File::create(&file.temporary).map(std::io::BufWriter::new).map_err(|e| format!("creating {}: {e}", file.temporary.display()))).transpose()?;
        let total = frame_count(&self.project);
        let mut sink = ExportSink { encoder, hashes, pcm: Sha256::new(), cancel: control.cancel.as_deref(), progress: control.progress, total };

        let audio = audio_files.as_mut().map(|files| files as &mut dyn AudioSource);
        let summary = match render_with_audio(&self.project, compositor, &mut sink, audio) {
            Ok(summary) => summary,
            Err(_) if cancelled() => return Err(ExportError::Cancelled),
            Err(e) => return Err(ExportError::Failed(e.to_string())),
        };
        if summary.frames != total {
            return Err(format!("rendered {} of {total} frames", summary.frames).into());
        }
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        sink.encoder.finish()?;
        if let Some(mut file) = sink.hashes.take() {
            file.flush().map_err(|e| format!("writing the hash file failed: {e}"))?;
        }
        let pcm_sha256 = self.project.output.audio.map(|_| sink.pcm.clone().finalize().iter().map(|b| format!("{b:02x}")).collect());
        // Closes the output and hash files, which the renames need.
        drop(sink);
        gpu.check_errors()?;
        let report = ExportReport { encoder: encoder_name, frames: summary.frames, audio_samples: summary.audio_samples, clamped_samples: summary.clamped_samples, pcm_sha256 };
        let settled = commit_outputs(&mut out_file, hashes_file.as_mut()).map(|()| report);
        settle(&settled);
        Ok(settled?)
    }
}

/// Moves the finished files to their targets.
fn commit_outputs(out_file: &mut PendingFile, hashes_file: Option<&mut PendingFile>) -> Result<(), String> {
    if let Err(error) = out_file.commit() {
        // The hashes describe the render that was just kept.
        if let Some(file) = hashes_file {
            file.keep = true;
        }
        return Err(error);
    }
    match hashes_file {
        Some(file) => file.commit(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_failed_rename_keeps_the_finished_file_and_names_it() {
        let dir = scratch("locked");
        let target = dir.join("out.mp4");
        std::fs::write(&target, b"old").unwrap();
        let mut pending = PendingFile::new(&target).unwrap();
        std::fs::write(&pending.temporary, b"finished").unwrap();
        let temporary = pending.temporary.clone();
        // A player holding the target open without sharing: the rename over it fails.
        let lock = std::os::windows::fs::OpenOptionsExt::share_mode(&mut std::fs::OpenOptions::new(), 0).read(true).open(&target).unwrap();
        let message = pending.commit().unwrap_err();
        drop(pending);
        drop(lock);
        assert!(message.contains(&temporary.display().to_string()), "{message}");
        assert_eq!(std::fs::read(&temporary).unwrap(), b"finished");
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_reserved_name_keeps_the_extension_last_and_avoids_existing_files() {
        let dir = scratch("reserve");
        let target = dir.join("movie.mp4");
        let first = reserve_temporary(&target).unwrap();
        let second = reserve_temporary(&target).unwrap();
        assert_ne!(first, second);
        for path in [&first, &second] {
            assert_eq!(path.extension().unwrap(), "mp4");
            assert!(path.is_file());
        }
        let without_extension = reserve_temporary(&dir.join("movie")).unwrap();
        assert_eq!(without_extension.extension().unwrap(), "tmp");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_pending_file_dropped_without_commit_leaves_the_target_and_other_files() {
        let dir = scratch("drop");
        let target = dir.join("out.mp4");
        std::fs::write(&target, b"old").unwrap();
        let bystander = dir.join("out.tmp.mp4");
        std::fs::write(&bystander, b"user").unwrap();
        {
            let pending = PendingFile::new(&target).unwrap();
            std::fs::write(&pending.temporary, b"partial").unwrap();
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read(&bystander).unwrap(), b"user");
        assert_eq!(file_names(&dir), ["out.mp4", "out.tmp.mp4"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
