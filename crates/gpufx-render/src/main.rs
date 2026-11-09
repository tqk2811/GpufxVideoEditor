//! `gpufx-render`: renders a project JSON to a video file.

use std::cell::Cell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use d3d12_backend::{EncoderChoice, ExportControl, ExportError, ExportJob, ExportReport, Gpu, ProgressEvent, ProjectPaths, CANCEL_LINE, PROGRESS_PROTOCOL};
use engine::{frame_count, Project};

const USAGE: &str = "usage: gpufx-render <project.json> --out <file.mp4> [--encoder <name|auto>] [--hashes <file>] [--packages <dir>]... [--progress <text|json>] [--cancel-on-stdin-eof]\n\
    \n\
    --encoder   H.264 encoder: auto (default), libx264, h264_nvenc, h264_amf, h264_qsv, h264_mf or any encoder name FFmpeg accepts\n\
    --hashes    write 'frame sha256-of-RGBA' lines to a file\n\
    --packages  directory searched for effect packages (repeatable); it holds packages/gpufx/<group>/<name>\n\
    --progress  text (default), or json: one JSON event per stdout line, and a 'cancel' line on stdin stops the render\n\
    \x20           (see docs/plans/spec-mcp-server-vi.md)\n\
    --cancel-on-stdin-eof  with --progress json: the end of stdin stops the render, so a parent that dies stops it too";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProgressMode {
    Text,
    Json,
}

struct Arguments {
    project: PathBuf,
    out: PathBuf,
    encoder: EncoderChoice,
    hashes: Option<PathBuf>,
    package_roots: Vec<PathBuf>,
    progress: ProgressMode,
    cancel_on_stdin_eof: bool,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut project = None;
    let mut out = None;
    let mut encoder = EncoderChoice::Auto;
    let mut hashes = None;
    let mut package_roots = Vec::new();
    let mut progress = ProgressMode::Text;
    let mut cancel_on_stdin_eof = false;
    // Paths need not be Unicode on Windows, so the arguments stay OS strings until they are read as text.
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().ok_or_else(|| format!("{name} needs a value"));
        let flag = argument.to_string_lossy();
        match flag.as_ref() {
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--encoder" => {
                let name = value("--encoder")?.to_string_lossy().into_owned();
                encoder = if name == "auto" { EncoderChoice::Auto } else { EncoderChoice::Named(name) };
            }
            "--hashes" => hashes = Some(PathBuf::from(value("--hashes")?)),
            "--packages" => package_roots.push(PathBuf::from(value("--packages")?)),
            "--progress" => {
                progress = match value("--progress")?.to_string_lossy().as_ref() {
                    "text" => ProgressMode::Text,
                    "json" => ProgressMode::Json,
                    other => return Err(format!("--progress must be text or json, not {other}")),
                }
            }
            "--cancel-on-stdin-eof" => cancel_on_stdin_eof = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if project.is_none() && !other.starts_with("--") => project = Some(PathBuf::from(&argument)),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if package_roots.is_empty() {
        // Running from a checkout: the packages of the submodule.
        package_roots.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter")));
    }
    if cancel_on_stdin_eof && progress != ProgressMode::Json {
        return Err("--cancel-on-stdin-eof needs --progress json".to_owned());
    }
    Ok(Arguments { project: project.ok_or(USAGE)?, out: out.ok_or(USAGE)?, encoder, hashes, package_roots, progress, cancel_on_stdin_eof })
}

/// Why a run wrote no file.
enum Failure {
    Cancelled,
    Message(String),
    /// A failure whose `error` event has already been written.
    Reported(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure::Message(message)
    }
}

/// Writes one event. A reader that went away cannot be told anything, so its error is dropped.
fn emit(event: &ProgressEvent) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", event.to_line());
    let _ = stdout.flush();
}

/// Sets `cancel` when a cancel line arrives. The end of stdin means nothing unless `eof_cancels`:
/// a caller may simply have no use for it.
fn watch_stdin(cancel: Arc<AtomicBool>, eof_cancels: bool) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) if line.trim() == CANCEL_LINE => cancel.store(true, Ordering::SeqCst),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if eof_cancels {
            cancel.store(true, Ordering::SeqCst);
        }
    });
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn run() -> Result<(), Failure> {
    let arguments = parse_arguments()?;
    let text = std::fs::read_to_string(&arguments.project).map_err(|e| format!("reading {}: {e}", arguments.project.display()))?;
    let project = Project::from_json(&text).map_err(|e| e.to_string())?;
    let json = arguments.progress == ProgressMode::Json;
    let cancel: Arc<AtomicBool> = Arc::default();
    if json {
        watch_stdin(cancel.clone(), arguments.cancel_on_stdin_eof);
    }

    let paths = Arc::new(ProjectPaths::new(&arguments.project, arguments.package_roots.clone()));
    let has_audio = project.output.audio.is_some();
    let total = frame_count(&project);
    let job = ExportJob { project, project_file: Some(arguments.project.clone()), paths, out: arguments.out.clone(), encoder: arguments.encoder, hashes: arguments.hashes };
    job.check_named("--out", "--hashes")?;

    let gpu = Arc::new(Gpu::create()?);
    if json {
        emit(&ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: gpu.adapter_name.clone(), total, out: path_text(&arguments.out) });
        let started = Instant::now();
        let progress = |frame: i64, total: i64| emit(&ProgressEvent::Progress { frame, total, elapsed_ms: started.elapsed().as_millis() as u64 });
        let encoder_opened = |name: &str| emit(&ProgressEvent::Encoder { name: name.to_owned() });
        let reserved = |path: &Path| emit(&ProgressEvent::OutputReserved { path: path_text(path) });
        // The end goes out as soon as the files are settled, ahead of the GPU teardown.
        let settled_reported = Cell::new(false);
        let settled = |result: &Result<ExportReport, String>| {
            settled_reported.set(true);
            emit(&match result {
                Ok(report) => ProgressEvent::Finished { report: report.clone() },
                Err(message) => ProgressEvent::Error { message: message.clone() },
            });
        };
        let control = ExportControl { cancel: Some(cancel), progress: Some(&progress), encoder_opened: Some(&encoder_opened), reserved: Some(&reserved), settled: Some(&settled) };
        return match job.run(gpu, control) {
            Ok(_) => Ok(()),
            Err(ExportError::Cancelled) => Err(Failure::Cancelled),
            Err(ExportError::Failed(message)) if settled_reported.get() => Err(Failure::Reported(message)),
            Err(ExportError::Failed(message)) => Err(Failure::Message(message)),
        };
    }

    println!("gpu: {}", gpu.adapter_name);
    let report = job.run(gpu, ExportControl { encoder_opened: Some(&|name| println!("encoder: {name}")), ..ExportControl::default() }).map_err(|e| e.to_string())?;
    if has_audio {
        println!("audio: {} samples, {} clamped", report.audio_samples, report.clamped_samples);
        println!("pcm sha256: {}", report.pcm_sha256.as_deref().unwrap_or_default());
    }
    println!("wrote {} frames to {}", report.frames, arguments.out.display());
    Ok(())
}

/// Whether the command line asks for JSON, known even when the rest of it is wrong.
fn json_requested() -> bool {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    arguments.windows(2).any(|pair| pair[0] == "--progress" && pair[1] == "json")
}

fn main() -> ExitCode {
    let json = json_requested();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Cancelled) => {
            if json {
                emit(&ProgressEvent::Cancelled);
            }
            eprintln!("cancelled");
            ExitCode::FAILURE
        }
        Err(Failure::Message(message)) => {
            if json {
                emit(&ProgressEvent::Error { message: message.clone() });
            }
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
        Err(Failure::Reported(message)) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
