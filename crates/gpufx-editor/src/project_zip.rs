//! The packed project: `<stem>.zip.gpufxproj` beside `<stem>.json.gpufxproj`, an ordinary zip holding the
//! project file and its whole bundle (see [`crate::project_bundle`]) with relative paths, to take
//! the project to another machine as one file.
//!
//! Opening one extracts it into a folder beside it named `<stem>` (or `<stem> (2)`, ... when that
//! is taken), then opens the single `*.json.gpufxproj` at the root of the folder.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::package_manager::backup_folder;

/// The extension of a project file, after its stem: JSON inside.
pub const PROJECT_EXTENSION: &str = ".json.gpufxproj";

/// The extension of a packed project, after its stem: a zip inside.
pub const ARCHIVE_EXTENSION: &str = ".zip.gpufxproj";

/// The last part of both extensions, as a file dialog filter takes it.
pub const FILTER_EXTENSION: &str = "gpufxproj";

/// The working folder of a bundle, never packed.
const WORKING_FOLDER: &str = ".copying";

/// Media already compressed: deflating it again only costs time.
const STORED_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "m4v", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "mp3", "aac", "m4a", "ogg", "opus", "wma", "flac", "jpg", "jpeg", "png", "webp", "gif", "heic", "avif", "zip", "woff", "woff2", "7z", "rar", "gz",
];

/// Whether the file at `path` is a packed project. Told by its content, never by the `.zip.` or
/// `.json.` of its name: a file starting with a zip local header (or an empty zip's end record) is
/// packed, anything else is a JSON project.
pub fn is_packed(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    fs::File::open(path).and_then(|mut file| file.read_exact(&mut magic)).is_ok() && (&magic == b"PK\x03\x04" || &magic == b"PK\x05\x06")
}

fn lower_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The stem of a project or packed project file: its name without [`PROJECT_EXTENSION`] or
/// [`ARCHIVE_EXTENSION`], else [`Path::file_stem`] (`project` when it has none).
pub fn project_stem(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_lowercase();
    for suffix in [PROJECT_EXTENSION, ARCHIVE_EXTENSION] {
        if lower.ends_with(suffix) && lower.len() > suffix.len() {
            return name[..name.len() - suffix.len()].to_owned();
        }
    }
    path.file_stem().map_or_else(|| "project".into(), |stem| stem.to_string_lossy().into_owned())
}

/// `path` with [`PROJECT_EXTENSION`] appended when its name does not already end with it, as a
/// name typed in the Save dialog may not.
pub fn with_project_extension(path: PathBuf) -> PathBuf {
    if lower_name(&path).ends_with(PROJECT_EXTENSION) {
        return path;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let name = name.strip_suffix(&format!(".{FILTER_EXTENSION}")).or_else(|| name.strip_suffix(".json")).unwrap_or(&name).to_owned();
    path.with_file_name(format!("{name}{PROJECT_EXTENSION}"))
}

/// `<folder>/<stem>.zip.gpufxproj` for the project file `<folder>/<stem>.json.gpufxproj`.
pub fn packed_path(project_file: &Path) -> PathBuf {
    project_file.with_file_name(format!("{}{ARCHIVE_EXTENSION}", project_stem(project_file)))
}

fn method_for(path: &Path) -> CompressionMethod {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if STORED_EXTENSIONS.contains(&extension.as_str()) { CompressionMethod::Stored } else { CompressionMethod::Deflated }
}

/// Writes `<stem>.zip.gpufxproj` beside `project_file` with the project file and its bundle, through
/// a temporary file renamed into place. Returns the path written.
pub fn write_packed(project_file: &Path) -> Result<PathBuf, String> {
    let target = packed_path(project_file);
    let temporary = target.with_file_name(format!("{}.partial", target.file_name().unwrap_or_default().to_string_lossy()));
    let result = write_zip(project_file, &temporary).and_then(|()| fs::rename(&temporary, &target));
    if let Err(e) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("{}: {e}", target.display()));
    }
    Ok(target)
}

fn write_zip(project_file: &Path, to: &Path) -> io::Result<()> {
    let mut writer = ZipWriter::new(io::BufWriter::new(fs::File::create(to)?));
    let name = project_file.file_name().ok_or_else(|| io::Error::other("the project file has no name"))?.to_string_lossy().into_owned();
    add_file(&mut writer, project_file, &name)?;
    let bundle = backup_folder(project_file);
    if bundle.is_dir() {
        let root = bundle.file_name().unwrap_or_default().to_string_lossy().into_owned();
        add_folder(&mut writer, &bundle, &root)?;
    }
    writer.finish()?.flush()
}

fn add_file<W: Write + io::Seek>(writer: &mut ZipWriter<W>, path: &Path, name: &str) -> io::Result<()> {
    let options = SimpleFileOptions::default().compression_method(method_for(path)).large_file(fs::metadata(path)?.len() >= u32::MAX as u64);
    writer.start_file(name, options)?;
    io::copy(&mut fs::File::open(path)?, writer)?;
    Ok(())
}

fn add_folder<W: Write + io::Seek>(writer: &mut ZipWriter<W>, folder: &Path, name: &str) -> io::Result<()> {
    writer.add_directory(format!("{name}/"), SimpleFileOptions::default())?;
    let mut entries: Vec<_> = fs::read_dir(folder)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name == WORKING_FOLDER {
            continue;
        }
        let entry_name = format!("{name}/{file_name}");
        if entry.file_type()?.is_dir() {
            add_folder(writer, &entry.path(), &entry_name)?;
        } else {
            add_file(writer, &entry.path(), &entry_name)?;
        }
    }
    Ok(())
}

/// The folder a packed project extracts to: `<stem>` beside it, or `<stem> (2)`, ... when that
/// exists and is not empty.
pub fn extraction_folder(packed: &Path) -> PathBuf {
    let stem = project_stem(packed);
    let parent = packed.parent().unwrap_or(Path::new("."));
    let free = |path: &Path| !path.exists() || fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none());
    let first = parent.join(&stem);
    if free(&first) {
        return first;
    }
    (2..).map(|n| parent.join(format!("{stem} ({n})"))).find(|path| free(path)).expect("some folder name is free")
}

/// The relative path of a zip entry, or `None` when it would land outside the folder: an absolute
/// path, a drive or UNC prefix, or a `..`.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') || name.contains(':') {
        return None;
    }
    let mut path = PathBuf::new();
    for component in Path::new(&name).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(path)
}

/// Extracts `packed` (see [`extraction_folder`]) and returns its project file, the single `*.json.gpufxproj`
/// at the root of the extracted folder. An entry escaping the folder fails the whole extraction.
pub fn extract_packed(packed: &Path) -> Result<PathBuf, String> {
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", packed.display());
    let mut archive = ZipArchive::new(io::BufReader::new(fs::File::open(packed).map_err(|e| fail(&e))?)).map_err(|e| fail(&e))?;
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|e| fail(&e))?;
        let name = entry.name().map_err(|e| fail(&e))?.into_owned();
        let relative = safe_relative(&name).ok_or_else(|| fail(&format!("the entry {name} is outside the folder")))?;
        entries.push((index, relative, entry.is_dir()));
    }
    let folder = extraction_folder(packed);
    fs::create_dir_all(&folder).map_err(|e| fail(&e))?;
    for (index, relative, is_dir) in entries {
        let target = folder.join(&relative);
        if is_dir || relative.as_os_str().is_empty() {
            fs::create_dir_all(&target).map_err(|e| fail(&e))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| fail(&e))?;
        }
        let mut entry = archive.by_index(index).map_err(|e| fail(&e))?;
        let mut out = fs::File::create(&target).map_err(|e| fail(&e))?;
        io::copy(&mut (&mut entry as &mut dyn Read), &mut out).map_err(|e| fail(&e))?;
    }
    project_file_in(&folder).map_err(|e| fail(&e))
}

/// The single project file directly in `folder`: a `*.json.gpufxproj`, or a `*.json`.
fn project_file_in(folder: &Path) -> Result<PathBuf, String> {
    let mut found: Vec<PathBuf> = fs::read_dir(folder)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && (lower_name(path).ends_with(PROJECT_EXTENSION) || lower_name(path).ends_with(".json")))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("no project file (*.json.gpufxproj) at the root of the archive".to_owned()),
        n => Err(format!("{n} project files (*.json.gpufxproj) at the root of the archive, expected one")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-project-zip-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn names(packed: &Path) -> Vec<String> {
        let archive = ZipArchive::new(fs::File::open(packed).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(|name| name.unwrap().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn the_packed_file_holds_the_project_and_its_bundle_with_relative_paths() {
        let dir = scratch("pack");
        let project = dir.join("film.json.gpufxproj");
        write(&project, b"{}");
        write(&dir.join("film.bundle/media/video/a.mp4"), b"video");
        write(&dir.join("film.bundle/packages/fx/manifest.json"), b"{}");
        write(&dir.join("film.bundle/.copying/half"), b"x");
        write(&dir.join("other.mp4"), b"not in the bundle");
        let packed = write_packed(&project).unwrap();
        assert_eq!(packed, dir.join("film.zip.gpufxproj"));
        assert_eq!(names(&packed), ["film.bundle/", "film.bundle/media/", "film.bundle/media/video/", "film.bundle/media/video/a.mp4", "film.bundle/packages/", "film.bundle/packages/fx/", "film.bundle/packages/fx/manifest.json", "film.json.gpufxproj"]);
        let mut archive = ZipArchive::new(fs::File::open(&packed).unwrap()).unwrap();
        assert_eq!(archive.by_name("film.bundle/media/video/a.mp4").unwrap().compression(), CompressionMethod::Stored);
        assert_eq!(archive.by_name("film.json.gpufxproj").unwrap().compression(), CompressionMethod::Deflated);
        assert!(!dir.join("film.zip.gpufxproj.partial").exists());
    }

    #[test]
    fn a_packed_project_opens_from_its_extracted_folder() {
        let dir = scratch("round");
        let source = dir.join("made");
        let project = source.join("film.json.gpufxproj");
        write(&project, br#"{"source":"film.bundle/media/video/a.mp4"}"#);
        write(&source.join("film.bundle/media/video/a.mp4"), b"video");
        let packed = write_packed(&project).unwrap();
        let moved = dir.join("film.zip.gpufxproj");
        fs::rename(&packed, &moved).unwrap();
        let opened = extract_packed(&moved).unwrap();
        assert_eq!(opened, dir.join("film").join("film.json.gpufxproj"));
        assert_eq!(fs::read(dir.join("film/film.bundle/media/video/a.mp4")).unwrap(), b"video");
        // A second open does not write over the first.
        assert_eq!(extract_packed(&moved).unwrap(), dir.join("film (2)").join("film.json.gpufxproj"));
    }

    #[test]
    fn a_saved_project_round_trips_with_its_media_inside_the_extracted_folder() {
        let dir = scratch("project");
        let source = dir.join("made");
        let media = source.join("film.bundle/media/video/a.mp4");
        write(&media, b"video");
        use engine::{Clip, Output, Time, Track, TrackKind};
        let clip = Clip { source: media.to_string_lossy().into_owned(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(900), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None, animation: Default::default(), denoise: Default::default(), voice: Default::default(), reverse: false, matte: Default::default(), follow: None, lock: None, censor: None };
        let track = Track { kind: TrackKind::Video, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false, subtitle: None };
        let project = engine::Project { version: engine::SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_rate: engine::FrameRate::whole(30), audio: None }, tracks: vec![track], markers: Vec::new(), beats: Default::default(), motion_tracks: Vec::new() };
        let file = source.join("film.json.gpufxproj");
        crate::project_file::save_project_file(&project, &file).unwrap();
        let packed = write_packed(&file).unwrap();
        let moved = dir.join("film.zip.gpufxproj");
        fs::rename(&packed, &moved).unwrap();
        fs::remove_dir_all(&source).unwrap();
        let opened = extract_packed(&moved).unwrap();
        let loaded = crate::project_file::load_project_file(&opened, &[]).unwrap();
        let clip_source = &loaded.tracks.iter().flat_map(|t| &t.clips).next().expect("one clip").source;
        assert!(Path::new(clip_source).starts_with(dir.join("film")), "{clip_source}");
        assert!(Path::new(clip_source).is_file());
    }

    #[test]
    fn entries_escaping_the_folder_are_refused() {
        for bad in ["../evil.txt", "/abs.txt", "C:/win.txt", "a/../../b.txt", "..\\up.txt"] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
        assert_eq!(safe_relative("film.bundle/./a.mp4"), Some(PathBuf::from("film.bundle/a.mp4")));
        let dir = scratch("slip");
        let packed = dir.join("bad.zip.gpufxproj");
        let mut writer = ZipWriter::new(fs::File::create(&packed).unwrap());
        writer.start_file("bad.json.gpufxproj", SimpleFileOptions::default()).unwrap();
        writer.start_file("../evil.txt", SimpleFileOptions::default()).unwrap();
        writer.finish().unwrap();
        assert!(extract_packed(&packed).is_err());
        assert!(!dir.join("evil.txt").exists());
        assert!(!dir.join("bad").exists(), "nothing is extracted");
    }

    #[test]
    fn the_extraction_folder_takes_the_stem_and_skips_used_names() {
        let dir = scratch("names");
        let packed = dir.join("film.zip.gpufxproj");
        assert_eq!(extraction_folder(&packed), dir.join("film"));
        fs::create_dir_all(dir.join("film")).unwrap();
        assert_eq!(extraction_folder(&packed), dir.join("film"), "an empty folder is used");
        write(&dir.join("film/x"), b"x");
        write(&dir.join("film (2)/x"), b"x");
        assert_eq!(extraction_folder(&packed), dir.join("film (3)"));
        assert_eq!(extraction_folder(&dir.join("clip.zip")), dir.join("clip"));
    }

    #[test]
    fn a_project_is_packed_or_not_by_its_content_never_by_its_name() {
        let dir = scratch("sniff");
        let project = dir.join("made").join("film.json.gpufxproj");
        write(&project, br#"{"a":1}"#);
        let packed = write_packed(&project).unwrap();
        // A zip project named .json.gpufxproj opens as a zip.
        let zip_named_json = dir.join("zipped.json.gpufxproj");
        fs::copy(&packed, &zip_named_json).unwrap();
        assert!(is_packed(&zip_named_json));
        assert_eq!(fs::read(extract_packed(&zip_named_json).unwrap()).unwrap(), br#"{"a":1}"#);
        // A JSON project named .zip.gpufxproj, or .zip, opens as JSON.
        for name in ["text.zip.gpufxproj", "text.zip", "text.gpufxproj"] {
            write(&dir.join(name), b"{}");
            assert!(!is_packed(&dir.join(name)), "{name}");
        }
        write(&dir.join("pk.gpufxproj"), b"PK not a zip");
        assert!(!is_packed(&dir.join("pk.gpufxproj")), "the full local header is needed");
        assert!(!is_packed(&dir.join("missing.zip.gpufxproj")));
    }

    #[test]
    fn the_stem_strips_the_double_extension() {
        assert_eq!(project_stem(Path::new("a/film.json.gpufxproj")), "film");
        assert_eq!(project_stem(Path::new("a/film.ZIP.gpufxproj")), "film");
        assert_eq!(project_stem(Path::new("a/film.json")), "film");
        assert_eq!(project_stem(Path::new("a/my.film.zip")), "my.film");
        assert_eq!(backup_folder(Path::new("a/film.json.gpufxproj")), PathBuf::from("a/film.bundle"));
        assert_eq!(with_project_extension(PathBuf::from("a/film")), PathBuf::from("a/film.json.gpufxproj"));
        assert_eq!(with_project_extension(PathBuf::from("a/film.gpufxproj")), PathBuf::from("a/film.json.gpufxproj"));
        assert_eq!(with_project_extension(PathBuf::from("a/film.json")), PathBuf::from("a/film.json.gpufxproj"));
        assert_eq!(with_project_extension(PathBuf::from("a/film.json.gpufxproj")), PathBuf::from("a/film.json.gpufxproj"));
    }

    #[test]
    fn several_project_files_are_an_error() {
        let dir = scratch("several");
        write(&dir.join("a.json.gpufxproj"), b"{}");
        write(&dir.join("b.json"), b"{}");
        assert!(project_file_in(&dir).is_err());
        let empty = scratch("none");
        assert!(project_file_in(&empty).is_err());
    }
}
