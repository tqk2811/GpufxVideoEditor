//! Where the editor finds its files, and saving and opening project JSON. No window here.
//!
//! In memory every source is absolute. The file on disk holds a source under the project's folder
//! relative to it, with forward slashes, so a folder of media and project can be moved as one.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use d3d12_backend::{reserve_temporary, ProjectPaths};
use engine::{validate, Project, Resolver};

/// Names a directory holding `packages/gpufx`, for an install that keeps them elsewhere.
pub const PACKAGE_ROOT_VARIABLE: &str = "GPUFX_PACKAGE_ROOT";
/// The packages of a checkout, the last place looked in.
const DEV_PACKAGE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter");
/// A project with no file yet: its clips hold absolute paths, which [`ProjectPaths`] keeps as they are.
const UNSAVED_PROJECT_FILE: &str = "unsaved.json";

/// Whether `root` holds effect packages.
pub fn has_packages(root: &Path) -> bool {
    root.join("packages").join("gpufx").is_dir()
}

/// The first of the candidates that holds packages: next to the executable, the variable, the checkout.
pub fn choose_package_root(exe_dir: Option<PathBuf>, variable: Option<PathBuf>, dev: PathBuf, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    [exe_dir, variable, Some(dev)].into_iter().flatten().find(|root| exists(root))
}

/// The package root of this run, or none when no candidate has packages.
pub fn package_roots() -> Vec<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let variable = std::env::var_os(PACKAGE_ROOT_VARIABLE).map(PathBuf::from);
    choose_package_root(exe_dir, variable, PathBuf::from(DEV_PACKAGE_ROOT), has_packages).into_iter().collect()
}

/// The paths of the project stored in `file`, or of an unsaved one.
pub fn project_paths(file: Option<&Path>, package_roots: &[PathBuf]) -> ProjectPaths {
    ProjectPaths::new(file.unwrap_or_else(|| Path::new(UNSAVED_PROJECT_FILE)), package_roots.to_vec())
}

/// The normal parts of `path`, lower-cased as Windows compares them.
fn comparable(path: &Path) -> Vec<String> {
    path.components().map(|part| part.as_os_str().to_string_lossy().to_lowercase()).collect()
}

/// `source` relative to `folder` with forward slashes, when it is under `folder`.
pub fn relative_under(folder: &Path, source: &Path) -> Option<String> {
    let folder_parts = comparable(&std::path::absolute(folder).ok()?);
    let source_absolute = std::path::absolute(source).ok()?;
    let source_parts = comparable(&source_absolute);
    if source_parts.len() <= folder_parts.len() || source_parts[..folder_parts.len()] != folder_parts[..] {
        return None;
    }
    let tail: Vec<String> = source_absolute.components().skip(folder_parts.len()).map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    Some(tail.join("/"))
}

fn is_relative_source(source: &str) -> bool {
    !Path::new(source).has_root() && !matches!(Path::new(source).components().next(), Some(Component::Prefix(_)))
}

/// `project` as stored in `file`: sources under its folder become relative.
pub fn with_relative_sources(project: &Project, file: &Path) -> Project {
    let folder = file.parent().unwrap_or_else(|| Path::new("."));
    let mut stored = project.clone();
    for clip in stored.tracks.iter_mut().flat_map(|track| &mut track.clips) {
        if let Some(relative) = relative_under(folder, Path::new(&clip.source)) {
            clip.source = relative;
        }
    }
    stored
}

/// `project`, read from `file`, with every source absolute.
pub fn with_absolute_sources(mut project: Project, file: &Path) -> Project {
    let folder = file.parent().unwrap_or_else(|| Path::new("."));
    for clip in project.tracks.iter_mut().flat_map(|track| &mut track.clips) {
        if is_relative_source(&clip.source) {
            let joined = folder.join(&clip.source);
            clip.source = std::path::absolute(&joined).unwrap_or(joined).to_string_lossy().into_owned();
        }
    }
    project
}

/// Writes `project` to `file` through a temporary file, so a failure leaves any earlier save intact.
pub fn save_project_file(project: &Project, file: &Path) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&with_relative_sources(project, file)).map_err(|e| format!("serialising the project failed: {e}"))?;
    let temporary = reserve_temporary(file).map_err(|e| format!("saving {} failed: {e}", file.display()))?;
    let written = (|| {
        let mut output = std::fs::OpenOptions::new().write(true).open(&temporary)?;
        output.write_all(text.as_bytes())?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&temporary, file)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("saving {} failed: {e}", file.display())
    })
}

/// Parses `text` as the project stored in `file` and validates it, reporting at most a few problems.
pub fn parse_project(text: &str, file: &Path, resolver: &dyn Resolver) -> Result<Project, String> {
    let project = Project::from_json(text).map_err(|e| format!("{}: {e}", file.display()))?;
    let project = with_absolute_sources(project, file);
    let errors = validate(&project, resolver);
    if let Some(first) = errors.first() {
        return Err(format!("{}: {first} ({} problems in all)", file.display(), errors.len()));
    }
    Ok(project)
}

/// Reads and validates the project in `file`. Nothing else changes on failure.
pub fn load_project_file(file: &Path, package_roots: &[PathBuf]) -> Result<Project, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    parse_project(&text, file, &project_paths(Some(file), package_roots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{AcceptAll, Clip, Output, Time, Track, TrackKind, SUPPORTED_VERSION};

    fn temp_folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clip(source: String, start: i64) -> Clip {
        Clip { source, start: Time::from_ticks(start), source_in: Time::ZERO, duration: Time::from_ticks(900), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO }
    }

    fn project(sources: Vec<String>) -> Project {
        let clips = sources.into_iter().enumerate().map(|(i, source)| clip(source, i as i64 * 900)).collect();
        Project { version: SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: None }, tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new() }] }
    }

    #[test]
    fn a_source_under_the_folder_becomes_relative_with_forward_slashes() {
        assert_eq!(relative_under(Path::new(r"D:\Work"), Path::new(r"d:\work\clips\a.mp4")), Some("clips/a.mp4".to_owned()));
        assert_eq!(relative_under(Path::new(r"D:\Work"), Path::new(r"D:\Other\a.mp4")), None);
        assert_eq!(relative_under(Path::new(r"D:\Work"), Path::new(r"D:\Work\..\Other\a.mp4")), None);
        assert_eq!(relative_under(Path::new(r"D:\Work"), Path::new(r"D:\Work")), None);
    }

    #[test]
    fn saved_sources_are_relative_inside_the_folder_and_absolute_outside() {
        let file = Path::new(r"D:\Work\p.json");
        let stored = with_relative_sources(&project(vec![r"D:\Work\clips\a.mp4".to_owned(), r"E:\media\b.mp4".to_owned()]), file);
        assert_eq!(stored.tracks[0].clips[0].source, "clips/a.mp4");
        assert_eq!(stored.tracks[0].clips[1].source, r"E:\media\b.mp4");
    }

    #[test]
    fn an_opened_project_holds_absolute_sources() {
        let file = Path::new(r"D:\Work\p.json");
        let opened = with_absolute_sources(project(vec!["clips/a.mp4".to_owned(), r"E:\media\b.mp4".to_owned()]), file);
        assert_eq!(opened.tracks[0].clips[0].source, r"D:\Work\clips\a.mp4");
        assert_eq!(opened.tracks[0].clips[1].source, r"E:\media\b.mp4");
    }

    #[test]
    fn save_then_open_gives_an_equal_project() {
        let dir = temp_folder("roundtrip");
        let inside = dir.join("clips").join("a.mp4");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, b"x").unwrap();
        let outside = temp_folder("roundtrip-outside").join("b.mp4");
        std::fs::write(&outside, b"x").unwrap();
        let mut original = project(vec![inside.to_string_lossy().into_owned(), outside.to_string_lossy().into_owned()]);
        original.tracks[0].clips[0].gain_db = -3.5;
        let file = dir.join("p.json");

        save_project_file(&original, &file).unwrap();

        assert!(std::fs::read_to_string(&file).unwrap().contains("clips/a.mp4"), "kept readable and relative");
        assert_eq!(load_project_file(&file, &package_roots()).unwrap(), original);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(names.len(), 2, "only the project and its clips folder remain: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_save_keeps_the_earlier_file() {
        let dir = temp_folder("failed-save");
        let file = dir.join("p.json");
        std::fs::write(&file, b"earlier").unwrap();
        let bystander = dir.join("p.tmp.json");
        std::fs::write(&bystander, b"user data").unwrap();
        // Opened without sharing, so the rename over it fails.
        let lock = std::os::windows::fs::OpenOptionsExt::share_mode(&mut std::fs::OpenOptions::new(), 0).read(true).open(&file).unwrap();
        assert!(save_project_file(&project(Vec::new()), &file).is_err());
        drop(lock);
        assert_eq!(std::fs::read(&file).unwrap(), b"earlier");
        assert_eq!(std::fs::read(&bystander).unwrap(), b"user data");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "the reserved file is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_leaves_a_file_with_the_temporary_look_alone() {
        let dir = temp_folder("keep-tmp");
        let file = dir.join("p.json");
        let bystander = dir.join("p.tmp.json");
        std::fs::write(&bystander, b"user data").unwrap();
        save_project_file(&project(Vec::new()), &file).unwrap();
        assert_eq!(std::fs::read(&bystander).unwrap(), b"user data");
        assert!(file.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_version_opens() {
        let text = r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":3000},"tracks":[{"clips":[]}]}"#;
        let opened = parse_project(text, Path::new(r"D:\p.json"), &AcceptAll).unwrap();
        assert_eq!(opened.version, SUPPORTED_VERSION);
    }

    #[test]
    fn broken_or_invalid_projects_are_reported() {
        let file = Path::new(r"D:\p.json");
        assert!(parse_project("not json", file, &AcceptAll).unwrap_err().contains("p.json"));
        let zero_size = r#"{"version":2,"output":{"width":0,"height":16,"frame_duration":3000},"tracks":[]}"#;
        assert!(parse_project(zero_size, file, &AcceptAll).is_err());
        assert!(parse_project(r#"{"version":9,"output":{"width":16,"height":16,"frame_duration":3000},"tracks":[]}"#, file, &AcceptAll).unwrap_err().contains("unsupported"));
    }

    #[test]
    fn a_missing_source_fails_to_open() {
        let dir = temp_folder("missing");
        let file = dir.join("p.json");
        save_project_file(&project(vec![dir.join("gone.mp4").to_string_lossy().into_owned()]), &file).unwrap();
        assert!(load_project_file(&file, &package_roots()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn root(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn next_to_the_executable_wins() {
        let chosen = choose_package_root(Some(root("exe")), Some(root("var")), root("dev"), |_| true);
        assert_eq!(chosen, Some(root("exe")));
    }

    #[test]
    fn the_variable_comes_before_the_checkout() {
        let chosen = choose_package_root(Some(root("exe")), Some(root("var")), root("dev"), |p| p != Path::new("exe"));
        assert_eq!(chosen, Some(root("var")));
    }

    #[test]
    fn the_checkout_is_the_last_resort_and_may_be_missing() {
        assert_eq!(choose_package_root(None, None, root("dev"), |_| true), Some(root("dev")));
        assert_eq!(choose_package_root(Some(root("exe")), None, root("dev"), |_| false), None);
    }

    #[test]
    fn paths_follow_the_project_file() {
        let paths = project_paths(Some(Path::new(r"D:\work\a.json")), &[]);
        assert_eq!(paths.source_path("b.mp4"), PathBuf::from(r"D:\work\b.mp4"));
    }
}
