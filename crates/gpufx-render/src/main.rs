//! `gpufx-render`: renders a project JSON to a video file.

use std::io::Write;
use std::path::{Path, PathBuf};
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

/// The sibling file a result is written to first: `movie.mp4` becomes `movie.tmp.mp4`, so the
/// extension that picks the container is kept.
fn temporary_path(target: &Path) -> PathBuf {
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match target.extension() {
        Some(extension) => format!("{stem}.tmp.{}", extension.to_string_lossy()),
        None => format!("{stem}.tmp"),
    };
    target.with_file_name(name)
}

/// Refuses an output that is the project file, one of its sources or packages, or each other.
fn reject_overwriting_inputs(arguments: &Arguments, project: &Project, paths: &ProjectPaths) -> Result<(), String> {
    let mut inputs: Vec<(PathBuf, &str)> = vec![(arguments.project.clone(), "the project file")];
    for clip in project.tracks.iter().flat_map(|t| &t.clips) {
        inputs.push((paths.source_path(&clip.source), "a source of the project"));
    }
    let packages = project.tracks.iter().flat_map(|t| t.clips.iter().flat_map(|c| c.effects.iter().map(|e| &e.package)).chain(t.transitions.iter().map(|t| &t.package)));
    for package in packages {
        if let Some(path) = paths.package_path(package) {
            inputs.push((path, "a package of the project"));
        }
    }
    let mut outputs = vec![("--out", &arguments.out)];
    outputs.extend(arguments.hashes.as_ref().map(|h| ("--hashes", h)));
    for (flag, output) in &outputs {
        let output_id = identity(output);
        for (input, what) in &inputs {
            if identity(input) == output_id {
                return Err(format!("{flag} {} is {what}; it would be overwritten", output.display()));
            }
        }
    }
    if outputs.len() == 2 && identity(outputs[0].1) == identity(outputs[1].1) {
        return Err("--out and --hashes name the same file".to_owned());
    }
    Ok(())
}

/// A file written under a temporary name that replaces `target` only on [`PendingFile::commit`].
/// Dropped without a commit, it deletes the temporary file, so a failed run leaves `target` as it was.
struct PendingFile {
    temporary: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl PendingFile {
    fn new(target: &Path) -> Self {
        Self { temporary: temporary_path(target), target: target.to_path_buf(), committed: false }
    }

    /// Renames the finished temporary file over the target. Close the file first.
    fn commit(&mut self) -> Result<(), String> {
        std::fs::rename(&self.temporary, &self.target).map_err(|e| format!("renaming {} to {}: {e}", self.temporary.display(), self.target.display()))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
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

    let paths = Arc::new(ProjectPaths::new(&arguments.project, arguments.package_roots.clone()));
    let errors = validate(&project, paths.as_ref());
    if !errors.is_empty() {
        let lines: Vec<String> = errors.iter().map(ToString::to_string).collect();
        return Err(format!("the project is not valid:\n  {}", lines.join("\n  ")));
    }

    reject_overwriting_inputs(&arguments, &project, &paths)?;

    let gpu = Arc::new(Gpu::create()?);
    println!("gpu: {}", gpu.adapter_name);
    let (width, height) = (project.output.width, project.output.height);
    let mut compositor = D3d12Compositor::new(gpu, paths, width, height)?;
    // Declared before the sink, so they drop after it has closed its files.
    let mut out_file = PendingFile::new(&arguments.out);
    let mut hashes_file = arguments.hashes.as_deref().map(PendingFile::new);
    if let Some(hashes) = &hashes_file {
        if identity(&hashes.temporary) == identity(&out_file.temporary) {
            return Err("--out and --hashes name the same file".to_owned());
        }
    }
    let encoder = Mp4Encoder::create(&out_file.temporary, &project.output, &arguments.encoder)?;
    println!("encoder: {}", encoder.encoder_name());
    let hashes = hashes_file.as_ref().map(|file| std::fs::File::create(&file.temporary).map(std::io::BufWriter::new).map_err(|e| format!("creating {}: {e}", file.temporary.display()))).transpose()?;
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
    // Closes the output and hash files, which the renames need.
    drop(sink);
    out_file.commit()?;
    if let Some(file) = &mut hashes_file {
        file.commit()?;
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
