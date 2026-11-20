//! The bundle of a project: `<folder>/<project stem>.bundle`, holding a copy of every pack the
//! project uses (`packages/<key>`, see [`crate::package_manager::backup_used_packs`]) and of every
//! file it reads (`media/<kind>/<file name>`), so the folder can move to another machine whole.
//!
//! ```text
//! <stem>.json
//! <stem>.bundle/
//!   packages/<pack key>/...
//!   media/video/  media/audio/  media/images/  media/fonts/  media/subtitles/  media/other/
//! ```
//!
//! A file is hard linked into the bundle when it is on the same volume, and copied otherwise.
//! The project is then pointed at the files in the bundle; since they are under the project's
//! folder, the project file stores them relative.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use d3d12_backend::is_asset_source;
use engine::{FontRef, Project};

use crate::package_manager::{backup_folder, link_or_copy, next_unique};
use crate::project_file::{same_source, texture_files};

/// The folder under the bundle holding the media.
pub const MEDIA_FOLDER: &str = "media";

/// The subfolder of [`MEDIA_FOLDER`] a file goes to, by its extension.
pub fn media_subfolder(path: &Path) -> &'static str {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "ts" | "mts" | "m2ts" | "3gp" => "video",
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aif" | "aiff" => "audio",
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "tif" | "tiff" | "tga" | "dds" | "heic" | "avif" | "svg" | "ico" | "exr" | "hdr" => "images",
        "ttf" | "otf" | "ttc" | "woff" | "woff2" => "fonts",
        "srt" | "ass" | "ssa" | "vtt" | "sub" | "sbv" => "subtitles",
        _ => "other",
    }
}

/// Calls `visit` with every file path `project` holds: the sources of media clips, the font files
/// and the texture files of text clips, and the media of the motion tracks. An `asset:` source,
/// a text or a censor clip names no file and is left out.
fn for_each_path(project: &mut Project, mut visit: impl FnMut(&mut String)) {
    for clip in project.tracks.iter_mut().flat_map(|track| &mut track.clips) {
        if let Some(FontRef::File { path }) = clip.text.as_mut().map(|text| &mut text.font) {
            visit(path);
        }
        for path in texture_files(clip) {
            visit(path);
        }
        if !is_asset_source(&clip.source) && !clip.is_text() && !clip.is_censor() {
            visit(&mut clip.source);
        }
    }
    for motion in &mut project.motion_tracks {
        if !is_asset_source(&motion.media_path) {
            visit(&mut motion.media_path);
        }
    }
}

/// The file paths of `project` (see [`for_each_path`]), each once.
pub fn referenced_files(project: &Project) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut copy = project.clone();
    for_each_path(&mut copy, |path| {
        if !path.is_empty() && !found.iter().any(|known| same_source(known, path)) {
            found.push(path.clone());
        }
    });
    found
}

/// What bundling the media of a project did.
#[derive(Debug, Default, PartialEq)]
pub struct BundleReport {
    /// Each file moved into the bundle: its old path and the path of its copy.
    pub relocated: BTreeMap<String, String>,
    /// Files copied or linked this time; files whose copy was up to date are not counted.
    pub written: usize,
    /// Files the project names that are not there.
    pub missing: Vec<String>,
    /// Files that could not be copied, with the reason.
    pub failed: Vec<String>,
}

/// Whether `copy` is an up to date copy of `source`: the same size and modified time, as a hard
/// link to it always is.
fn up_to_date(source: &fs::Metadata, copy: &Path) -> bool {
    let Ok(copy) = fs::metadata(copy) else { return false };
    copy.is_file() && copy.len() == source.len() && matches!((copy.modified(), source.modified()), (Ok(a), Ok(b)) if a == b)
}

/// `name` with ` (n)` before its extension.
fn numbered(name: &Path, n: usize) -> String {
    let stem = name.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    match name.extension() {
        Some(extension) => format!("{stem} ({n}).{}", extension.to_string_lossy()),
        None => format!("{stem} ({n})"),
    }
}

/// Copies every file `project` reads into the bundle beside `project_file`, by kind and under its
/// own name (` (2)`, ` (3)`… when another file took it), and says where each went. A file already
/// in the bundle stays where it is; a copy that is up to date is kept; a missing file or one on the
/// network or a device is left out. A failure on one file does not stop the others.
pub fn bundle_media(project: &Project, project_file: &Path) -> BundleReport {
    let bundle = backup_folder(project_file);
    let media = bundle.join(MEDIA_FOLDER);
    let working = bundle.join(".copying");
    let mut report = BundleReport::default();
    // The copies given out this time, so two files never share one.
    let mut taken: Vec<PathBuf> = Vec::new();
    for source in referenced_files(project) {
        if gpufx_text::is_network_or_device_path(&source) || crate::project_file::relative_under(&bundle, Path::new(&source)).is_some() {
            continue;
        }
        let from = Path::new(&source);
        let meta = match fs::metadata(from) {
            Ok(meta) if meta.is_file() => meta,
            _ => {
                report.missing.push(source.clone());
                continue;
            }
        };
        let Some(name) = from.file_name().map(PathBuf::from) else {
            report.missing.push(source.clone());
            continue;
        };
        let folder = media.join(media_subfolder(from));
        let mut n = 1;
        let to = loop {
            let candidate = folder.join(if n == 1 { name.to_string_lossy().into_owned() } else { numbered(&name, n) });
            let claimed = taken.iter().any(|other| same_source(&other.to_string_lossy(), &candidate.to_string_lossy()));
            if !claimed && (fs::symlink_metadata(&candidate).is_err() || up_to_date(&meta, &candidate)) {
                break candidate;
            }
            n += 1;
        };
        taken.push(to.clone());
        if !up_to_date(&meta, &to) {
            if let Err(e) = place(from, &to, &working) {
                report.failed.push(format!("{source}: {e}"));
                continue;
            }
            report.written += 1;
        }
        report.relocated.insert(source, std::path::absolute(&to).unwrap_or(to).to_string_lossy().into_owned());
    }
    let _ = fs::remove_dir(&working);
    report
}

/// Links or copies `from` to `to`, which does not exist, through `working` so `to` is never half
/// written.
fn place(from: &Path, to: &Path, working: &Path) -> std::io::Result<()> {
    fs::create_dir_all(working)?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = working.join(next_unique());
    let result = link_or_copy(from, &temporary).and_then(|()| fs::rename(&temporary, to));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Points every path of `project` found in `relocated` at its new place.
pub fn relocate(project: &mut Project, relocated: &BTreeMap<String, String>) {
    if relocated.is_empty() {
        return;
    }
    for_each_path(project, |path| {
        if let Some((_, to)) = relocated.iter().find(|(from, _)| same_source(from, path)) {
            *path = to.clone();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Clip, MotionTrack, Output, Time, Track, TrackKind};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-project-bundle-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clip(source: &str) -> Clip {
        Clip { source: source.to_owned(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(900), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None, animation: Default::default(), denoise: Default::default(), voice: Default::default(), reverse: false, matte: Default::default(), follow: None, lock: None, censor: None }
    }

    fn project(clips: Vec<Clip>) -> Project {
        let track = Track { kind: TrackKind::Video, clips, transitions: Vec::new(), locked: false, hidden: false, subtitle: None };
        Project { version: engine::SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_rate: engine::FrameRate::whole(30), audio: None }, tracks: vec![track], markers: Vec::new(), beats: Default::default(), motion_tracks: Vec::new() }
    }

    fn write(path: &Path, bytes: &[u8]) -> String {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn files_are_sorted_into_folders_by_extension() {
        assert_eq!(media_subfolder(Path::new("a.MP4")), "video");
        assert_eq!(media_subfolder(Path::new("a.wav")), "audio");
        assert_eq!(media_subfolder(Path::new("a.jpeg")), "images");
        assert_eq!(media_subfolder(Path::new("a.otf")), "fonts");
        assert_eq!(media_subfolder(Path::new("a.srt")), "subtitles");
        assert_eq!(media_subfolder(Path::new("a.cube")), "other");
        assert_eq!(media_subfolder(Path::new("noextension")), "other");
    }

    #[test]
    fn media_is_copied_once_with_names_kept_and_collisions_numbered() {
        let dir = scratch("copy");
        let a = write(&dir.join("in/one/clip.mp4"), b"first");
        let b = write(&dir.join("in/two/clip.mp4"), b"second file");
        let font = write(&dir.join("in/f.ttf"), b"font");
        let mut text = clip(engine::TEXT_SOURCE);
        let mut content = engine::TextContent::new("Hi", 0.1);
        content.font = FontRef::File { path: font.clone() };
        text.text = Some(content);
        let mut project = project(vec![clip(&a), clip(&a), clip(&b), text, clip("asset:sticker/mine/a.png"), clip(&dir.join("in/gone.mp4").to_string_lossy())]);
        project.motion_tracks.push(MotionTrack { id: 1, media_path: a.clone(), aspect: 1.0, samples: Vec::new() });
        let file = dir.join("work/film.json");
        let report = bundle_media(&project, &file);
        let media = dir.join("work/film.bundle/media");
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.written, 3, "one copy for the clip used twice");
        assert_eq!(report.missing, vec![dir.join("in/gone.mp4").to_string_lossy().into_owned()]);
        assert_eq!(fs::read(media.join("video/clip.mp4")).unwrap(), b"first");
        assert_eq!(fs::read(media.join("video/clip (2).mp4")).unwrap(), b"second file");
        assert_eq!(fs::read(media.join("fonts/f.ttf")).unwrap(), b"font");
        assert!(!report.relocated.keys().any(|from| from.starts_with("asset:")));

        relocate(&mut project, &report.relocated);
        let clips = &project.tracks[0].clips;
        assert_eq!(Path::new(&clips[0].source), std::path::absolute(media.join("video/clip.mp4")).unwrap());
        assert_eq!(clips[0].source, clips[1].source);
        assert_eq!(project.motion_tracks[0].media_path, clips[0].source, "the motion track follows its clip");
        assert!(clips[2].source.ends_with("clip (2).mp4"));
        assert!(matches!(&clips[3].text.as_ref().unwrap().font, FontRef::File { path } if path.ends_with("f.ttf") && path.contains("film.bundle")));
        assert_eq!(clips[4].source, "asset:sticker/mine/a.png");

        // Saved again: everything is in the bundle already, so nothing moves.
        let again = bundle_media(&project, &file);
        assert_eq!((again.written, again.relocated.len()), (0, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_up_to_date_copy_is_kept_and_a_changed_file_gets_a_new_name() {
        let dir = scratch("unchanged");
        let a = write(&dir.join("in/clip.mp4"), b"first");
        let file = dir.join("film.json");
        let project = project(vec![clip(&a)]);
        assert_eq!(bundle_media(&project, &file).written, 1);
        let second = bundle_media(&project, &file);
        assert_eq!(second.written, 0, "the copy is up to date");
        assert!(second.relocated[&a].ends_with("clip.mp4"));

        // A different file of the same name, not a hard link of the copy.
        fs::remove_file(&a).unwrap();
        write(&dir.join("in/clip.mp4"), b"another one");
        let third = bundle_media(&project, &file);
        assert_eq!(third.written, 1);
        assert!(third.relocated[&a].ends_with("clip (2).mp4"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_on_the_same_volume_is_hard_linked() {
        let dir = scratch("link");
        let a = write(&dir.join("in/song.mp3"), b"la la");
        let file = dir.join("film.json");
        let report = bundle_media(&project(vec![clip(&a)]), &file);
        assert_eq!(report.written, 1);
        let copy = PathBuf::from(&report.relocated[&a]);
        // Writing through one name shows through the other only when both name one file.
        use std::io::Write;
        fs::OpenOptions::new().append(true).open(&a).unwrap().write_all(b"!").unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"la la!", "the copy is a hard link of the source");
        let _ = fs::remove_dir_all(&dir);
    }
}
