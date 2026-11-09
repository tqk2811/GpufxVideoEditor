//! `gpufx-render`: renders a project JSON to a video file.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use d3d12_backend::{EncoderChoice, ExportControl, ExportJob, Gpu, ProjectPaths};
use engine::Project;

const USAGE: &str = "usage: gpufx-render <project.json> --out <file.mp4> [--encoder <name|auto>] [--hashes <file>] [--packages <dir>]...\n\
    \n\
    --encoder   H.264 encoder: auto (default), libx264, h264_nvenc, h264_amf, h264_qsv, h264_mf or any encoder name FFmpeg accepts\n\
    --hashes    write 'frame sha256-of-RGBA' lines to a file\n\
    --packages  directory searched for effect packages (repeatable); it holds packages/gpufx/<group>/<name>";

struct Arguments {
    project: PathBuf,
    out: PathBuf,
    encoder: EncoderChoice,
    hashes: Option<PathBuf>,
    package_roots: Vec<PathBuf>,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut project = None;
    let mut out = None;
    let mut encoder = EncoderChoice::Auto;
    let mut hashes = None;
    let mut package_roots = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().ok_or_else(|| format!("{name} needs a value"));
        match argument.as_str() {
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--encoder" => {
                let name = value("--encoder")?;
                encoder = if name == "auto" { EncoderChoice::Auto } else { EncoderChoice::Named(name) };
            }
            "--hashes" => hashes = Some(PathBuf::from(value("--hashes")?)),
            "--packages" => package_roots.push(PathBuf::from(value("--packages")?)),
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if project.is_none() && !other.starts_with("--") => project = Some(PathBuf::from(other)),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if package_roots.is_empty() {
        // Running from a checkout: the packages of the submodule.
        package_roots.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter")));
    }
    Ok(Arguments { project: project.ok_or(USAGE)?, out: out.ok_or(USAGE)?, encoder, hashes, package_roots })
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments()?;
    let text = std::fs::read_to_string(&arguments.project).map_err(|e| format!("reading {}: {e}", arguments.project.display()))?;
    let project = Project::from_json(&text).map_err(|e| e.to_string())?;

    let paths = Arc::new(ProjectPaths::new(&arguments.project, arguments.package_roots.clone()));
    let has_audio = project.output.audio.is_some();
    let job = ExportJob { project, project_file: Some(arguments.project.clone()), paths, out: arguments.out.clone(), encoder: arguments.encoder, hashes: arguments.hashes };
    job.check()?;

    let gpu = Arc::new(Gpu::create()?);
    println!("gpu: {}", gpu.adapter_name);
    let report = job.run(gpu, ExportControl::default()).map_err(|e| e.to_string())?;
    println!("encoder: {}", report.encoder);
    if has_audio {
        println!("audio: {} samples, {} clamped", report.audio_samples, report.clamped_samples);
        println!("pcm sha256: {}", report.pcm_sha256.as_deref().unwrap_or_default());
    }
    println!("wrote {} frames to {}", report.frames, arguments.out.display());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
