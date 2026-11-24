//! `gpufx-render`: renders a project JSON to a video file.

mod effect_previews;

use std::cell::Cell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use d3d12_backend::{EncoderChoice, ExportControl, ExportError, ExportJob, ExportReport, Gpu, ProgressEvent, ProjectPaths, CANCEL_LINE, PROGRESS_PROTOCOL};
use engine::{frame_count, Project};

const USAGE: &str = "usage: gpufx-render <project.json> --out <file.mp4> [--codec <h264|hevc>] [--encoder <name|auto>] [--hashes <file>] [--keyint <frames>] [--packages <dir>]... [--progress <text|json>] [--cancel-on-stdin-eof] [--no-overwrite]\n\
    \n\
    --codec     video codec: h264 (default) or hevc; with --encoder auto, picks which encoders Auto tries\n\
    --encoder   auto (default): the first encoder that opens on this machine, hardware first\n\
    \x20           (h264_nvenc, h264_qsv, h264_amf, h264_mf, then libx264 in the GPL build, then libopenh264;\n\
    \x20           for hevc: hevc_nvenc, hevc_qsv, hevc_amf, hevc_mf, libx265, libkvazaar), or one of those names,\n\
    \x20           or any encoder name FFmpeg accepts. A named encoder that fails stops the render.\n\
    --hashes    write 'frame sha256-of-RGBA' lines to a file\n\
    --keyint    frames between keyframes (1 to 600); default two seconds. The editor's preview proxies use a short one
\n    --packages  directory searched for effect packages (repeatable); it holds packages/gpufx/<group>/<name>,\n\
    \x20           and packs such as packages/font/<id>; without it, the checkout's packages and %LOCALAPPDATA%\\gpufx-editor\n\
    --progress  text (default), or json: one JSON event per stdout line, and a 'cancel' line on stdin stops the render\n\
    \x20           (see docs/plans/spec-mcp-server-vi.md)\n\
    --cancel-on-stdin-eof  with --progress json: the end of stdin stops the render, so a parent that dies stops it too\n\
    \n\
    usage: gpufx-render --effect-previews\n\
    \n\
    --effect-previews  helper mode, alone on the command line: JSON jobs on stdin, JSON events on stdout, exits at the end of stdin\n\
    \x20           (see crates/gpufx-render/src/effect_previews.rs)";

/// How long a render gets to stop by itself once stdin has ended, under `--cancel-on-stdin-eof`.
const CANCEL_ON_EOF_DEADLINE: Duration = Duration::from_secs(30);

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
    keyframe_interval: Option<u32>,
    package_roots: Vec<PathBuf>,
    progress: ProgressMode,
    cancel_on_stdin_eof: bool,
    /// False under `--no-overwrite`: a file that appears at `out` meanwhile is kept.
    overwrite: bool,
}

/// The folder under `%LOCALAPPDATA%` that the editor installs the user's packs in (fonts, sounds,
/// stickers, styles), as the editor's `project_file::user_package_root` names it.
const USER_FOLDER: &str = "gpufx-editor";

/// The roots searched without `--packages`: the packages of the submodule of a checkout, then the
/// packs the user installed through the editor, so a project naming an installed font pack or
/// asset renders from the command line as it does from the editor.
fn default_package_roots(local_app_data: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let checkout = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter"));
    let user = local_app_data.filter(|value| !value.is_empty()).map(|folder| PathBuf::from(folder).join(USER_FOLDER));
    std::iter::once(checkout).chain(user).collect()
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut project = None;
    let mut out = None;
    let mut codec = None;
    let mut encoder = None;
    let mut hashes = None;
    let mut keyframe_interval = None;
    let mut package_roots = Vec::new();
    let mut progress = ProgressMode::Text;
    let mut cancel_on_stdin_eof = false;
    let mut overwrite = true;
    // Paths need not be Unicode on Windows, so the arguments stay OS strings until they are read as text.
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().ok_or_else(|| format!("{name} needs a value"));
        let flag = argument.to_string_lossy();
        match flag.as_ref() {
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--codec" => codec = Some(value("--codec")?.to_string_lossy().into_owned()),
            "--encoder" => encoder = Some(value("--encoder")?.to_string_lossy().into_owned()),
            "--hashes" => hashes = Some(PathBuf::from(value("--hashes")?)),
            "--keyint" => keyframe_interval = Some(parse_keyint(&value("--keyint")?.to_string_lossy())?),
            "--packages" => package_roots.push(PathBuf::from(value("--packages")?)),
            "--progress" => {
                progress = match value("--progress")?.to_string_lossy().as_ref() {
                    "text" => ProgressMode::Text,
                    "json" => ProgressMode::Json,
                    other => return Err(format!("--progress must be text or json, not {other}")),
                }
            }
            "--cancel-on-stdin-eof" => cancel_on_stdin_eof = true,
            "--no-overwrite" => overwrite = false,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if project.is_none() && !other.starts_with("--") => project = Some(PathBuf::from(&argument)),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if package_roots.is_empty() {
        package_roots = default_package_roots(std::env::var_os("LOCALAPPDATA"));
    }
    if cancel_on_stdin_eof && progress != ProgressMode::Json {
        return Err("--cancel-on-stdin-eof needs --progress json".to_owned());
    }
    let encoder = EncoderChoice::parse(codec.as_deref(), encoder.as_deref())?;
    Ok(Arguments { project: project.ok_or(USAGE)?, out: out.ok_or(USAGE)?, encoder, hashes, keyframe_interval, package_roots, progress, cancel_on_stdin_eof, overwrite })
}

/// The `--keyint` value: a whole number of frames from 1 to 600.
fn parse_keyint(text: &str) -> Result<u32, String> {
    text.parse::<u32>().ok().filter(|frames| (1..=600).contains(frames)).ok_or_else(|| format!("--keyint must be a number of frames from 1 to 600, not {text}"))
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
            stop_after_eof(&cancel, CANCEL_ON_EOF_DEADLINE, || std::process::exit(1));
        }
    });
}

/// Asks the render to stop and gives it `deadline` to do so. A child stuck in a decode, an encode
/// or the GPU never reads the flag, and with its parent gone no one would ever stop it.
fn stop_after_eof(cancel: &AtomicBool, deadline: Duration, give_up: impl FnOnce()) {
    cancel.store(true, Ordering::SeqCst);
    std::thread::sleep(deadline);
    give_up();
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

    // The presets of the clip animation packs, which the editor that started this export reads too.
    engine::set_pack_presets(d3d12_backend::read_pack_presets(&arguments.package_roots));
    let paths = Arc::new(ProjectPaths::new(&arguments.project, arguments.package_roots.clone()));
    let has_audio = project.output.audio.is_some();
    let total = frame_count(&project);
    let job = ExportJob { project, project_file: Some(arguments.project.clone()), paths, out: arguments.out.clone(), encoder: arguments.encoder, hashes: arguments.hashes, keyframe_interval: arguments.keyframe_interval, overwrite: arguments.overwrite };
    job.check_named("--out", "--hashes")?;

    let gpu = Arc::new(Gpu::create()?);
    if json {
        emit(&ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: gpu.adapter_name.clone(), total, out: path_text(&arguments.out) });
        let started = Instant::now();
        let progress = |frame: i64, total: i64| emit(&ProgressEvent::Progress { frame, total, elapsed_ms: started.elapsed().as_millis() as u64 });
        let encoder_opened = |name: &str| emit(&ProgressEvent::Encoder { name: name.to_owned() });
        let reserved = |path: &Path| emit(&ProgressEvent::OutputReserved { path: path_text(path) });
        // The end, a failure included, goes out ahead of the GPU teardown. A cancel is reported in `main`.
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
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if arguments.iter().any(|argument| argument == "--effect-previews") {
        if arguments.len() != 1 {
            eprintln!("error: --effect-previews takes no other argument\n{USAGE}");
            return ExitCode::FAILURE;
        }
        return effect_previews::run_stdio();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn the_end_of_stdin_cancels_and_gives_up_after_the_deadline() {
        let cancel = AtomicBool::new(false);
        let gave_up = Cell::new(false);
        stop_after_eof(&cancel, Duration::from_millis(1), || gave_up.set(true));
        assert!(cancel.load(Ordering::SeqCst), "the render is asked to stop first");
        assert!(gave_up.get(), "a render that did not stop is ended");
    }

    #[test]
    fn without_packages_the_user_packs_of_the_editor_are_searched_after_the_checkout() {
        let scratch = std::env::temp_dir().join(format!("gpufx-render-font-root-{}", std::process::id()));
        let roots = default_package_roots(Some(scratch.clone().into()));
        assert_eq!(roots.len(), 2, "{roots:?}");
        assert!(roots[0].ends_with("external/FFmpegGpuFilter"), "{roots:?}");
        assert_eq!(roots[1], scratch.join("gpufx-editor"));
        let pack = scratch.join("gpufx-editor").join("packages").join("font").join("open-fonts");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("manifest.json"), "{}").unwrap();
        std::fs::write(pack.join("a.ttf"), b"font").unwrap();
        let paths = ProjectPaths::new(&scratch.join("p.json"), roots);
        assert_eq!(paths.font_pack_path("open-fonts", "a.ttf"), Ok(pack.join("a.ttf")), "a font pack installed by the editor is found");
        assert_eq!(default_package_roots(Some("".into())).len(), 1, "an empty variable counts as unset");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn the_keyframe_interval_is_a_frame_count_from_one_to_600() {
        assert_eq!(parse_keyint("10"), Ok(10));
        assert_eq!(parse_keyint("600"), Ok(600));
        assert!(parse_keyint("0").is_err());
        assert!(parse_keyint("601").is_err());
        assert!(parse_keyint("ten").is_err());
    }
}
