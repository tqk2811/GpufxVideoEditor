//! The audio packs installed on this machine, for the Audio tab. No window here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use d3d12_backend::{asset_source, ProjectPaths};
use engine::{Time, TICKS_PER_SECOND};

use crate::asset_install::{check_manifest_fields, parse_audio_manifest, AudioPackKind};
use crate::i18n::pack::{Localized, PackCollection, PackTranslations};
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
    /// What the manifest says the sound is like, in English; empty when it says nothing.
    pub description: String,
    /// The name, category and tags in the languages its pack brings.
    pub i18n: Localized,
}

impl LibrarySound {
    /// The item that goes through the drag and the add paths of the media list.
    pub fn media_item(&self) -> MediaItem {
        MediaItem { source: self.source.clone(), name: self.name.clone(), kind: MediaKind::Audio, duration: self.duration, estimated: true }
    }

    /// The name in language `code`, else the manifest's.
    pub fn display_name(&self, code: &str) -> &str {
        self.i18n.name(code, &self.name)
    }

    /// The fields a search looks at, in English and in every language the pack brings.
    pub(crate) fn search_fields(&self) -> crate::search::SearchFields<'_> {
        self.i18n.search(crate::search::SearchFields::new(&self.name).tags(std::iter::once(self.category.as_str()).chain(self.tags.iter().map(String::as_str))))
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
    /// The pack's name, description and tags in the languages it brings.
    pub i18n: Localized,
    /// The collection the lists show it under.
    pub collection: PackCollection,
}

impl LibraryPack {
    /// The name in language `code`, else the manifest's.
    pub fn display_name(&self, code: &str) -> &str {
        self.i18n.name(code, &self.name)
    }
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
    /// The packs with the sounds matching `search` by name, tag or category, best first (see
    /// [`crate::search`]).
    pub fn matching(&self, search: &str) -> Vec<(&LibraryPack, Vec<&LibrarySound>)> {
        let mut found: Vec<(&LibraryPack, Vec<&LibrarySound>)> = self.packs.iter().map(|pack| (pack, crate::search::rank(&pack.sounds, search, |sound| sound.search_fields()))).filter(|(_, sounds)| !sounds.is_empty()).collect();
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
    let manifest = match std::fs::read(folder.join("manifest.json")).map_err(|e| e.to_string()).and_then(|bytes| parse_audio_manifest(&bytes).map(|manifest| (manifest, bytes)).map_err(|e| format!("{e:?}"))) {
        Ok(manifest) => manifest,
        Err(message) => {
            problem(message);
            return None;
        }
    };
    let (manifest, bytes) = manifest;
    if check_manifest_fields(&manifest).ok() != Some(kind) || manifest.id.to_ascii_lowercase() != label.to_ascii_lowercase() {
        problem(format!("its manifest does not match the folder ({} {})", manifest.kind, manifest.id));
        return None;
    }
    // Each sound goes through what a clip goes through, so nothing listed can leave the pack.
    let paths = ProjectPaths::roots_only(Path::new("library.json"), vec![root.to_path_buf()]);
    let translations = PackTranslations::read_folder(folder);
    let mut sounds = Vec::new();
    for item in manifest.items {
        let source = asset_source(kind.name(), &manifest.id, &item.file);
        if let Err(error) = paths.asset_path(&source) {
            problem(error.to_string());
            continue;
        }
        sounds.push(LibrarySound { i18n: translations.item(&item.id), source, name: item.name, duration: seconds_to_time(item.duration), tags: item.tags, category: item.category, looped: item.looped, description: item.description });
    }
    let collection = PackCollection::from_manifest(&bytes, &manifest.id, &manifest.name, &translations);
    Some(LibraryPack { i18n: translations.pack(), collection, kind, id: manifest.id.to_ascii_lowercase(), name: manifest.name, version: manifest.version, sounds })
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, as it does when a clip resolves it.
pub fn scan_audio_packs(roots: &[PathBuf]) -> AudioLibrary {
    let mut library = AudioLibrary { loaded: true, ..AudioLibrary::default() };
    let mut seen: HashSet<(AudioPackKind, String)> = HashSet::new();
    let disabled = d3d12_backend::read_disabled_packs(roots);
    for root in roots {
        for kind in PACK_TYPES {
            let Ok(entries) = std::fs::read_dir(root.join("packages").join(kind.name())) else { continue };
            let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
            folders.sort();
            for folder in folders {
                let id = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().to_ascii_lowercase());
                if seen.contains(&(kind, id.clone())) || disabled.contains(&format!("{}/{id}", kind.name())) {
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

    const COLLECTION_FIELDS: &str = r#""version":"1.0.0","collection":"Fun","collection_name":"Fun Set","description":"d","category":"c","tags":["x"]"#;

    fn sound(name: &str, tags: &[&str], category: &str) -> LibrarySound {
        LibrarySound { i18n: Default::default(), source: format!("asset:sfx/p/{name}.flac"), name: name.to_owned(), duration: None, tags: tags.iter().map(|tag| (*tag).to_owned()).collect(), category: category.to_owned(), looped: false, description: String::new() }
    }

    fn pack(name: &str, sounds: Vec<LibrarySound>) -> LibraryPack {
        LibraryPack { collection: Default::default(), i18n: Default::default(), kind: AudioPackKind::Sfx, id: name.to_lowercase(), name: name.to_owned(), version: "1.0.0".to_owned(), sounds }
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
    fn one_item_packs_of_one_collection_load_and_group_together() {
        let dir = scratch("collection");
        write_pack(&dir, "sfx", "fun-a", "A", &["audio/a.flac", "audio/b.flac"]);
        write_pack(&dir, "sfx", "fun-b", "B", &["audio/a.flac", "audio/b.flac"]);
        write_pack(&dir, "sfx", "solo", "Solo", &["audio/a.flac", "audio/b.flac"]);
        for folder in [dir.join("packages/sfx/fun-a"), dir.join("packages/sfx/fun-b")] {
            let manifest = folder.join("manifest.json");
            let text = std::fs::read_to_string(&manifest).unwrap().replacen(r#""version":"1.0.0""#, COLLECTION_FIELDS, 1);
            std::fs::write(manifest, text).unwrap();
            std::fs::create_dir_all(folder.join("i18n")).unwrap();
            std::fs::write(folder.join("i18n/vi.json"), r#"{"collection_name": "Bộ vui", "category": "k"}"#).unwrap();
        }
        let library = scan_audio_packs(&[dir.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(library.problems.is_empty(), "{:?}", library.problems);
        let groups = crate::i18n::pack::group_by_collection(library.packs.iter().map(|pack| (&pack.collection, pack.id.as_str())), "vi");
        let shown: Vec<(&str, &str, Vec<&str>)> = groups.iter().map(|(c, ids)| (c.id.as_str(), c.display_name("vi"), ids.clone())).collect();
        assert_eq!(shown, vec![("fun", "Bộ vui", vec!["fun-a", "fun-b"]), ("pack:solo", "Solo", vec!["solo"])]);
        assert_eq!(library.packs[0].collection.name, "Fun Set");
    }

    #[test]
    fn a_library_sound_is_an_audio_item_with_the_manifest_length_marked_as_an_estimate() {
        let mut item = sound("Boom", &[], "c");
        item.duration = Some(Time::from_ticks(9000));
        let media = item.media_item();
        assert_eq!((media.kind, media.duration, media.estimated, media.source.as_str()), (MediaKind::Audio, Some(Time::from_ticks(9000)), true, "asset:sfx/p/Boom.flac"));
    }
}
