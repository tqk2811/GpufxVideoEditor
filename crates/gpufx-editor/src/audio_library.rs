//! The audio packs installed on this machine, for the Audio tab. No window here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use d3d12_backend::{asset_source, ProjectPaths};
use engine::{Time, TICKS_PER_SECOND};

use crate::asset_install::{check_manifest_fields, parse_audio_manifest, AudioPackKind};
use crate::media::{MediaItem, MediaKind};

const PACK_TYPES: [AudioPackKind; 2] = [AudioPackKind::Sfx, AudioPackKind::Music];

/// One sound of a pack.
#[derive(Debug, Clone, PartialEq)]
pub struct LibrarySound {
    /// The `asset:` source a clip stores.
    pub source: String,
    pub name: String,
    /// What the manifest says; a clip uses what a probe finds.
    pub duration: Option<Time>,
    pub tags: Vec<String>,
    pub category: String,
    pub looped: bool,
}

impl LibrarySound {
    /// The item that goes through the drag and the add paths of the media list.
    pub fn media_item(&self) -> MediaItem {
        MediaItem { source: self.source.clone(), name: self.name.clone(), kind: MediaKind::Audio, duration: self.duration, estimated: true }
    }

    fn matches(&self, word: &str) -> bool {
        self.name.to_lowercase().contains(word) || self.category.to_lowercase().contains(word) || self.tags.iter().any(|tag| tag.to_lowercase().contains(word))
    }
}

/// One installed pack.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryPack {
    pub kind: AudioPackKind,
    pub id: String,
    pub name: String,
    pub version: String,
    pub sounds: Vec<LibrarySound>,
}

/// A pack that could not be read, or a sound of one that is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryProblem {
    pub pack: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioLibrary {
    pub packs: Vec<LibraryPack>,
    pub problems: Vec<LibraryProblem>,
    /// The first scan has finished.
    pub loaded: bool,
}

impl AudioLibrary {
    /// The packs with the sounds matching every word of `search`, by name, tag or category.
    pub fn matching(&self, search: &str) -> Vec<(&LibraryPack, Vec<&LibrarySound>)> {
        let search = search.to_lowercase();
        let words: Vec<&str> = search.split_whitespace().collect();
        let mut found: Vec<(&LibraryPack, Vec<&LibrarySound>)> = self.packs.iter().map(|pack| (pack, pack.sounds.iter().filter(|sound| words.iter().all(|word| sound.matches(word))).collect::<Vec<_>>())).filter(|(_, sounds)| !sounds.is_empty()).collect();
        found.sort_by_key(|(pack, _)| pack.name.to_lowercase());
        found
    }
}

/// `seconds` from a manifest as a time.
fn seconds_to_time(seconds: f64) -> Option<Time> {
    let ticks = (seconds * TICKS_PER_SECOND as f64).round();
    (ticks.is_finite() && ticks > 0.0 && ticks < i64::MAX as f64).then(|| Time::from_ticks(ticks as i64))
}

fn read_pack(root: &Path, kind: AudioPackKind, folder: &Path, library: &mut AudioLibrary) -> Option<LibraryPack> {
    let label = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut problem = |message: String| library.problems.push(LibraryProblem { pack: label.clone(), message });
    let manifest = match std::fs::read(folder.join("manifest.json")).map_err(|e| e.to_string()).and_then(|bytes| parse_audio_manifest(&bytes).map_err(|e| format!("{e:?}"))) {
        Ok(manifest) => manifest,
        Err(message) => {
            problem(message);
            return None;
        }
    };
    if check_manifest_fields(&manifest).ok() != Some(kind) || manifest.id.to_ascii_lowercase() != label.to_ascii_lowercase() {
        problem(format!("its manifest does not match the folder ({} {})", manifest.kind, manifest.id));
        return None;
    }
    // Each sound goes through what a clip goes through, so nothing listed can leave the pack.
    let paths = ProjectPaths::roots_only(Path::new("library.json"), vec![root.to_path_buf()]);
    let mut sounds = Vec::new();
    for item in manifest.items {
        let source = asset_source(kind.name(), &manifest.id, &item.file);
        if let Err(error) = paths.asset_path(&source) {
            problem(error.to_string());
            continue;
        }
        sounds.push(LibrarySound { source, name: item.name, duration: seconds_to_time(item.duration), tags: item.tags, category: item.category, looped: item.looped });
    }
    Some(LibraryPack { kind, id: manifest.id.to_ascii_lowercase(), name: manifest.name, version: manifest.version, sounds })
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, as it does when a clip resolves it.
pub fn scan_audio_packs(roots: &[PathBuf]) -> AudioLibrary {
    let mut library = AudioLibrary { loaded: true, ..AudioLibrary::default() };
    let mut seen: HashSet<(AudioPackKind, String)> = HashSet::new();
    for root in roots {
        for kind in PACK_TYPES {
            let Ok(entries) = std::fs::read_dir(root.join("packages").join(kind.name())) else { continue };
            let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
            folders.sort();
            for folder in folders {
                let id = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().to_ascii_lowercase());
                if seen.contains(&(kind, id.clone())) {
                    continue;
                }
                if let Some(pack) = read_pack(root, kind, &folder, &mut library) {
                    seen.insert((kind, id));
                    library.packs.push(pack);
                }
            }
        }
    }
    library
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(name: &str, tags: &[&str], category: &str) -> LibrarySound {
        LibrarySound { source: format!("asset:sfx/p/{name}.flac"), name: name.to_owned(), duration: None, tags: tags.iter().map(|tag| (*tag).to_owned()).collect(), category: category.to_owned(), looped: false }
    }

    fn pack(name: &str, sounds: Vec<LibrarySound>) -> LibraryPack {
        LibraryPack { kind: AudioPackKind::Sfx, id: name.to_lowercase(), name: name.to_owned(), version: "1.0.0".to_owned(), sounds }
    }

    fn library() -> AudioLibrary {
        AudioLibrary { packs: vec![pack("Zeta", vec![sound("Boom", &["hit", "low"], "impact")]), pack("Alpha", vec![sound("Ding", &["ui"], "chime"), sound("Boom Big", &["hit"], "impact")])], problems: Vec::new(), loaded: true }
    }

    #[test]
    fn packs_are_grouped_by_name_and_only_those_with_a_match_are_kept() {
        let names = |search: &str| library().matching(search).iter().map(|(pack, sounds)| (pack.name.clone(), sounds.len())).collect::<Vec<_>>();
        assert_eq!(names(""), vec![("Alpha".to_owned(), 2), ("Zeta".to_owned(), 1)]);
        assert_eq!(names("ding"), vec![("Alpha".to_owned(), 1)]);
    }

    #[test]
    fn the_search_looks_at_names_tags_and_the_category_ignoring_case() {
        let total = |search: &str| library().matching(search).iter().map(|(_, sounds)| sounds.len()).sum::<usize>();
        assert_eq!(total("HIT"), 2, "a tag");
        assert_eq!(total("chime"), 1, "a category");
        assert_eq!(total("boom low"), 1, "every word must match");
        assert_eq!(total("nothing"), 0);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-audio-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_pack(root: &Path, kind: &str, folder: &str, name: &str, files: &[&str]) {
        let path = root.join("packages").join(kind).join(folder);
        std::fs::create_dir_all(path.join("audio")).unwrap();
        for file in files {
            std::fs::write(path.join(file), b"x").unwrap();
        }
        let manifest = format!(r#"{{"format":1,"type":"{kind}","id":"{folder}","name":"{name}","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{{"id":"a","file":"audio/a.flac","name":"A","duration":1.5,"tags":["t"],"category":"c","loop":true}},{{"id":"b","file":"audio/b.flac","name":"B","duration":2.0,"tags":[],"category":"c","loop":false}}]}}"#);
        std::fs::write(path.join("manifest.json"), manifest).unwrap();
    }

    #[test]
    fn a_scan_reads_each_pack_once_and_reports_what_is_broken() {
        let dir = scratch("scan");
        let (shipped, user) = (dir.join("shipped"), dir.join("user"));
        write_pack(&shipped, "sfx", "shared", "From Shipped", &["audio/a.flac", "audio/b.flac"]);
        write_pack(&user, "sfx", "shared", "From User", &["audio/a.flac", "audio/b.flac"]);
        write_pack(&user, "music", "tunes", "Tunes", &["audio/a.flac"]);
        std::fs::create_dir_all(user.join("packages/sfx/broken")).unwrap();
        std::fs::write(user.join("packages/sfx/broken/manifest.json"), b"not json").unwrap();
        write_pack(&user, "sfx", "wrong-name", "Wrong", &["audio/a.flac", "audio/b.flac"]);
        let manifest = user.join("packages/sfx/wrong-name/manifest.json");
        let text = std::fs::read_to_string(&manifest).unwrap().replace(r#""id":"wrong-name""#, r#""id":"other""#);
        std::fs::write(manifest, text).unwrap();

        let library = scan_audio_packs(&[shipped, user]);

        assert!(library.loaded);
        let names: Vec<(&str, usize)> = library.packs.iter().map(|pack| (pack.name.as_str(), pack.sounds.len())).collect();
        assert_eq!(names, vec![("From Shipped", 2), ("Tunes", 1)], "the first root wins and a missing sound is left out");
        let shipped_pack = &library.packs[0];
        assert_eq!(shipped_pack.sounds[0].source, "asset:sfx/shared/audio/a.flac");
        assert_eq!(shipped_pack.sounds[0].duration, Some(Time::from_ticks(TICKS_PER_SECOND + TICKS_PER_SECOND / 2)));
        assert!(shipped_pack.sounds[0].looped);
        assert_eq!(library.problems.len(), 3, "{:?}", library.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_sound_is_an_audio_item_with_the_manifest_length_marked_as_an_estimate() {
        let mut item = sound("Boom", &[], "c");
        item.duration = Some(Time::from_ticks(9000));
        let media = item.media_item();
        assert_eq!((media.kind, media.duration, media.estimated, media.source.as_str()), (MediaKind::Audio, Some(Time::from_ticks(9000)), true, "asset:sfx/p/Boom.flac"));
    }
}
