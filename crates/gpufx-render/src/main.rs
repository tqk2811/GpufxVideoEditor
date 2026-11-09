//! `gpufx-render`: renders a project JSON to a video file.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use d3d12_backend::{D3d12Compositor, EncoderChoice, Gpu, Mp4Encoder, ProjectPaths};
use engine::render::{render, FrameSink, RenderError};
use engine::{frame_count, validate, Project};
use sha2::{Digest, Sha256};

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

/// Hashes each frame's RGBA, then passes it to the encoder.
struct HashingSink {
    encoder: Mp4Encoder,
    hashes: Option<std::io::BufWriter<std::fs::File>>,
}

impl FrameSink for HashingSink {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        if let Some(file) = &mut self.hashes {
            let digest: String = Sha256::digest(rgba).iter().map(|b| format!("{b:02x}")).collect();
            writeln!(file, "{index} {digest}").map_err(|e| RenderError(format!("writing the hash file failed: {e}")))?;
        }
        self.encoder.write_frame(index, rgba)
    }
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments()?;
    let text = std::fs::read_to_string(&arguments.project).map_err(|e| format!("reading {}: {e}", arguments.project.display()))?;
    let project = Project::from_json(&text).map_err(|e| e.to_string())?;

    let paths = Arc::new(ProjectPaths::new(&arguments.project, arguments.package_roots));
    let errors = validate(&project, paths.as_ref());
    if !errors.is_empty() {
        let lines: Vec<String> = errors.iter().map(ToString::to_string).collect();
        return Err(format!("the project is not valid:\n  {}", lines.join("\n  ")));
    }

    let gpu = Arc::new(Gpu::create()?);
    println!("gpu: {}", gpu.adapter_name);
    let (width, height) = (project.output.width, project.output.height);
    let mut compositor = D3d12Compositor::new(gpu, paths, width, height)?;
    let encoder = Mp4Encoder::create(&arguments.out, &project.output, &arguments.encoder)?;
    println!("encoder: {}", encoder.encoder_name());
    let hashes = arguments.hashes.as_ref().map(|path| std::fs::File::create(path).map(std::io::BufWriter::new).map_err(|e| format!("creating {}: {e}", path.display()))).transpose()?;
    let mut sink = HashingSink { encoder, hashes };

    let total = frame_count(&project);
    let summary = render(&project, &mut compositor, &mut sink).map_err(|e| e.to_string())?;
    if summary.frames != total {
        return Err(format!("rendered {} of {total} frames", summary.frames));
    }
    sink.encoder.finish()?;
    if let Some(mut file) = sink.hashes.take() {
        file.flush().map_err(|e| format!("writing the hash file failed: {e}"))?;
    }
    println!("wrote {} frames to {}", summary.frames, arguments.out.display());
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
