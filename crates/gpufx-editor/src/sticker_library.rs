//! The sticker packs installed on this machine, for the Sticker tab. No window here.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use d3d12_backend::{asset_source, still_dimensions, ProjectPaths};

use crate::audio_library::LibraryProblem;
use crate::editor::DEFAULT_IMAGE_DURATION;
use crate::media::{MediaItem, MediaKind};
use crate::sticker_install::{check_sticker_fields, parse_sticker_manifest, STICKER_TYPE};

/// One sticker of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySticker {
    /// The `asset:` source a clip stores.
    pub source: String,
    pub name: String,
    pub tags: Vec<String>,
    pub category: String,
    /// Width and height from the file's header, or `None` when it could not be read.
    pub size: Option<(u32, u32)>,
}

impl LibrarySticker {
    /// The item that goes through the drag and the add paths of the media list.
    pub fn media_item(&self) -> MediaItem {
        MediaItem { source: self.source.clone(), name: self.name.clone(), kind: MediaKind::Video, duration: Some(DEFAULT_IMAGE_DURATION), estimated: true }
    }

    fn matches(&self, word: &str) -> bool {
        self.name.to_lowercase().contains(word) || self.category.to_lowercase().contains(word) || self.tags.iter().any(|tag| tag.to_lowercase().contains(word))
    }
}

/// One installed pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerPack {
    pub id: String,
    pub name: String,
    pub stickers: Vec<LibrarySticker>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StickerLibrary {
    pub packs: Vec<StickerPack>,
    pub problems: Vec<LibraryProblem>,
    /// The first scan has finished.
    pub loaded: bool,
}

impl StickerLibrary {
    /// Width over height of the sticker at `source`, when its file could be read.
    pub fn aspect_of(&self, source: &str) -> Option<f64> {
        let sticker = self.packs.iter().flat_map(|pack| &pack.stickers).find(|sticker| sticker.source == source)?;
        sticker.size.filter(|(_, height)| *height > 0).map(|(width, height)| f64::from(width) / f64::from(height))
    }

    /// The categories of every sticker, in order, without repeats.
    pub fn categories(&self) -> Vec<&str> {
        self.packs.iter().flat_map(|pack| &pack.stickers).map(|sticker| sticker.category.as_str()).filter(|category| !category.is_empty()).collect::<BTreeSet<_>>().into_iter().collect()
    }

    /// The stickers in `category` (any when `None`) that match every word of `search`, by name, tag
    /// or category.
    pub fn matching(&self, search: &str, category: Option<&str>) -> Vec<&LibrarySticker> {
        let search = search.to_lowercase();
        let words: Vec<&str> = search.split_whitespace().collect();
        self.packs.iter().flat_map(|pack| &pack.stickers).filter(|sticker| category.is_none_or(|wanted| sticker.category == wanted)).filter(|sticker| words.iter().all(|word| sticker.matches(word))).collect()
    }
}

fn read_pack(root: &Path, folder: &Path, library: &mut StickerLibrary) -> Option<StickerPack> {
    let label = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut problem = |message: String| library.problems.push(LibraryProblem { pack: label.clone(), message });
    let bytes = match std::fs::read(folder.join("manifest.json")) {
        Ok(bytes) => bytes,
        Err(error) => {
            problem(error.to_string());
            return None;
        }
    };
    let manifest = match parse_sticker_manifest(&bytes).and_then(|manifest| check_sticker_fields(&manifest).map(|()| manifest)) {
        Ok(manifest) => manifest,
        Err(error) => {
            problem(format!("{error:?}"));
            return None;
        }
    };
    if manifest.id.to_ascii_lowercase() != label.to_ascii_lowercase() {
        problem(format!("its manifest does not match the folder ({})", manifest.id));
        return None;
    }
    // Each sticker goes through what a clip goes through, so nothing listed can leave the pack.
    let paths = ProjectPaths::roots_only(Path::new("library.json"), vec![root.to_path_buf()]);
    let mut stickers = Vec::new();
    for item in manifest.items {
        let source = asset_source(STICKER_TYPE, &manifest.id, &item.file);
        let path = match paths.asset_path(&source) {
            Ok(path) => path,
            Err(error) => {
                problem(error.to_string());
                continue;
            }
        };
        // This scan runs off the UI thread, and only reads each file's header.
        let size = still_dimensions(&path).ok();
        stickers.push(LibrarySticker { source, name: item.name, tags: item.tags, category: item.category, size });
    }
    Some(StickerPack { id: manifest.id.to_ascii_lowercase(), name: manifest.name, stickers })
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, as it does when a clip resolves it.
pub fn scan_sticker_packs(roots: &[PathBuf]) -> StickerLibrary {
    let mut library = StickerLibrary { loaded: true, ..StickerLibrary::default() };
    let mut seen: HashSet<String> = HashSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("packages").join(STICKER_TYPE)) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        folders.sort();
        for folder in folders {
            let id = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().to_ascii_lowercase());
            if seen.contains(&id) {
                continue;
            }
            if let Some(pack) = read_pack(root, &folder, &mut library) {
                seen.insert(id);
                library.packs.push(pack);
            }
        }
    }
    library.packs.sort_by_key(|pack| pack.name.to_lowercase());
    library
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sticker(name: &str, tags: &[&str], category: &str) -> LibrarySticker {
        LibrarySticker { source: format!("asset:sticker/p/{name}.png"), name: name.to_owned(), tags: tags.iter().map(|tag| (*tag).to_owned()).collect(), category: category.to_owned(), size: None }
    }

    fn library() -> StickerLibrary {
        StickerLibrary { packs: vec![StickerPack { id: "p".to_owned(), name: "P".to_owned(), stickers: vec![sticker("Smile", &["face", "happy"], "people"), sticker("Dog", &["pet"], "animals"), sticker("Sad", &["face"], "people")] }], problems: Vec::new(), loaded: true }
    }

    #[test]
    fn a_search_looks_at_every_word_in_names_tags_and_the_category() {
        let names = |search: &str, category: Option<&str>| library().matching(search, category).iter().map(|sticker| sticker.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("", None).len(), 3);
        assert_eq!(names("FACE", None), vec!["Smile", "Sad"], "a tag, ignoring case");
        assert_eq!(names("face happy", None), vec!["Smile"], "every word must match");
        assert_eq!(names("", Some("animals")), vec!["Dog"]);
        assert_eq!(names("face", Some("animals")), Vec::<String>::new());
        assert_eq!(names("people", None).len(), 2, "the category is searched too");
    }

    #[test]
    fn the_categories_are_sorted_and_not_repeated() {
        assert_eq!(library().categories(), vec!["animals", "people"]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-sticker-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_pack(root: &Path, folder: &str, name: &str, files: &[&str]) {
        let path = root.join("packages/sticker").join(folder);
        std::fs::create_dir_all(path.join("images")).unwrap();
        for file in files {
            std::fs::write(path.join(file), b"x").unwrap();
        }
        let manifest = format!(r#"{{"format":1,"type":"sticker","id":"{folder}","name":"{name}","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{{"id":"a","file":"images/a.png","name":"A","tags":["t"],"category":"c"}},{{"id":"b","file":"images/b.png","name":"B","tags":[],"category":"c"}}]}}"#);
        std::fs::write(path.join("manifest.json"), manifest).unwrap();
    }

    #[test]
    fn a_scan_reads_each_pack_once_and_reports_what_is_broken() {
        let dir = scratch("scan");
        let (shipped, user) = (dir.join("shipped"), dir.join("user"));
        write_pack(&shipped, "shared", "From Shipped", &["images/a.png", "images/b.png"]);
        write_pack(&user, "shared", "From User", &["images/a.png", "images/b.png"]);
        write_pack(&user, "other", "Other", &["images/a.png"]);
        std::fs::create_dir_all(user.join("packages/sticker/broken")).unwrap();
        std::fs::write(user.join("packages/sticker/broken/manifest.json"), b"not json").unwrap();

        let library = scan_sticker_packs(&[shipped, user]);

        assert!(library.loaded);
        let packs: Vec<(&str, usize)> = library.packs.iter().map(|pack| (pack.name.as_str(), pack.stickers.len())).collect();
        assert_eq!(packs, vec![("From Shipped", 2), ("Other", 1)], "the first root wins and a missing file is left out");
        assert_eq!(library.packs[0].stickers[0].source, "asset:sticker/shared/images/a.png");
        assert_eq!(library.problems.len(), 2, "{:?}", library.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_sticker_is_a_video_item_marked_as_an_estimate() {
        let media = sticker("Smile", &[], "c").media_item();
        assert_eq!((media.kind, media.duration, media.estimated), (MediaKind::Video, Some(DEFAULT_IMAGE_DURATION), true));
    }
}
