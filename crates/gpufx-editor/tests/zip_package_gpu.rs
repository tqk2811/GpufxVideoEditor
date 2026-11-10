//! A real zip from the import through the catalog to a rendered frame and an effect preview. The
//! parts that need a GPU skip on a machine without a hardware adapter.

#![cfg(windows)]

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use d3d12_backend::{read_package_info, Gpu, PreviewRenderer, ProjectPaths};
use engine::{Clip, Effect, Output, Project, Time, Track, TrackKind, SUPPORTED_VERSION};
use gpufx_editor::package_import::{install_zip, ImportOutcome, InstalledPackage};
use gpufx_editor::packages::{scan, short_name, Catalog, PackageKind};
use gpufx_editor::preview::backend::D3d12Producer;
use gpufx_editor::project_file::project_paths;
use zip::write::SimpleFileOptions;

const PACKAGES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter");
const INSTALLED_ID: &str = "io.github.tqk2811.invert";

fn shipped_folder(name: &str) -> PathBuf {
    Path::new(PACKAGES).join("packages/gpufx/ffmpeg").join(name)
}

fn files_under(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).expect("the packages are checked out").flatten() {
        if entry.path().is_dir() {
            files_under(&entry.path(), found);
        } else {
            found.push(entry.path());
        }
    }
}

/// The package in `folder` as a zip, with its manifest id replaced by `id`.
fn zip_of(folder: &Path, id: &str) -> Vec<u8> {
    let mut files = Vec::new();
    files_under(folder, &mut files);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for file in files {
        let name = file.strip_prefix(folder).unwrap().to_string_lossy().replace('\\', "/");
        let mut bytes = std::fs::read(&file).unwrap();
        if name == "manifest.json" {
            let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            manifest["id"] = serde_json::Value::String(id.to_owned());
            bytes = serde_json::to_vec(&manifest).unwrap();
        }
        writer.start_file(name, options).unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

struct Scratch {
    folder: PathBuf,
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let folder = std::env::temp_dir().join(format!("gpufx-zip-gpu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        Self { root: folder.join("user"), folder }
    }

    /// Installs the shipped package `name` under the fixed id, and says what became of it.
    fn install(&self, name: &str, overwrite: bool) -> ImportOutcome {
        let source = self.folder.join("input.zip");
        std::fs::write(&source, zip_of(&shipped_folder(name), INSTALLED_ID)).unwrap();
        install_zip(&source, &self.root, overwrite).expect("the zip installs")
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(PACKAGES), self.root.clone()]
    }

    fn catalog(&self) -> Catalog {
        scan(&self.roots(), read_package_info)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

fn installed_name() -> String {
    format!("packages/gpufx/user/{INSTALLED_ID}.zip")
}

fn project(effects: Vec<Effect>) -> Project {
    let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-clips/clip-a.mp4"));
    let source = std::path::absolute(source).unwrap().to_string_lossy().into_owned();
    let clip = Clip { source, start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_seconds(1).unwrap(), effects, gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None };
    Project { version: SUPPORTED_VERSION, output: Output { width: 320, height: 180, frame_duration: Time::from_ticks(3000), audio: None }, tracks: vec![Track { kind: TrackKind::Video, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false }] }
}

fn with_package(package: &str) -> Project {
    project(vec![Effect { package: package.to_owned(), params: Default::default() }])
}

fn skip(reason: &str) {
    assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but {reason}");
    eprintln!("zip_package_gpu: skipped: {reason}");
}

fn hardware_gpu() -> Option<Arc<Gpu>> {
    let Some(gpu) = Gpu::create_hardware().expect("device") else {
        skip("no hardware adapter");
        return None;
    };
    if !Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-clips/clip-a.mp4")).is_file() {
        skip("test-clips is missing (scripts/setup-poc.ps1 makes it)");
        return None;
    }
    Some(Arc::new(gpu))
}

fn capture(producer: &mut D3d12Producer, project: &Project, paths: &Arc<ProjectPaths>) -> Vec<u8> {
    producer.capture(project, paths, 3).expect("the frame renders")
}

#[test]
fn an_installed_zip_is_listed_found_rendered_and_previewed_like_its_folder() {
    let scratch = Scratch::new("catalog");
    let outcome = scratch.install("invert", false);
    assert!(matches!(&outcome, ImportOutcome::Installed(InstalledPackage { kind: PackageKind::Effect, replaced: false, .. })), "{outcome:?}");

    let catalog = scratch.catalog();
    let entry = catalog.effects.iter().find(|entry| entry.package == installed_name()).unwrap_or_else(|| panic!("listed: {:?}", catalog.problems));
    assert_eq!(entry.name, "Invert");
    assert_eq!(short_name(&entry.package), INSTALLED_ID, "no extension where a name is shown");

    let paths = Arc::new(project_paths(None, &scratch.roots()));
    let zip_path = paths.package_path(&entry.package).expect("found by the name a project stores");
    assert_eq!(paths.package_path(INSTALLED_ID), Some(zip_path.clone()), "found by its bare name, without the extension");
    let agent = ProjectPaths::roots_only(Path::new("agent.json"), scratch.roots());
    assert_eq!(agent.package_path(&entry.package), Some(zip_path.clone()), "an agent may use what the user installed");

    let Some(gpu) = hardware_gpu() else { return };
    let mut producer = D3d12Producer::new(gpu);
    let plain = capture(&mut producer, &project(Vec::new()), &paths);
    let from_zip = capture(&mut producer, &with_package(&entry.package), &paths);
    let from_folder = capture(&mut producer, &with_package("packages/gpufx/ffmpeg/invert"), &paths);
    assert_eq!(from_zip, from_folder, "the zip renders as its folder does");
    assert_ne!(from_zip, plain, "the effect did something");
    drop(producer);

    let mut renderer = PreviewRenderer::new().expect("renderer");
    assert_eq!(renderer.render_still(&zip_path).expect("still of the zip"), renderer.render_still(&shipped_folder("invert")).expect("still of the folder"));
}

#[test]
fn overwriting_a_package_shows_the_new_one_once_the_paths_are_new() {
    let scratch = Scratch::new("overwrite");
    scratch.install("invert", false);
    let Some(gpu) = hardware_gpu() else { return };
    let mut producer = D3d12Producer::new(gpu);
    let installed = with_package(&installed_name());
    let before_paths = Arc::new(project_paths(None, &scratch.roots()));
    let before = capture(&mut producer, &installed, &before_paths);

    assert_eq!(scratch.install("grayscale", false), ImportOutcome::AlreadyInstalled { name: "Invert".to_owned() }, "the name already installed");
    assert!(matches!(scratch.install("grayscale", true), ImportOutcome::Installed(InstalledPackage { replaced: true, .. })));

    // What the window does after an install: new paths, so the compositor and its graphs go.
    let after_paths = Arc::new(project_paths(None, &scratch.roots()));
    let after = capture(&mut producer, &installed, &after_paths);
    let grayscale = capture(&mut producer, &with_package("packages/gpufx/ffmpeg/grayscale"), &after_paths);
    assert_ne!(before, after, "the old graph must not be reused");
    assert_eq!(after, grayscale);
}
