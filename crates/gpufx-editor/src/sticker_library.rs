//! The sticker packs installed on this machine, for the Sticker tab. No window here.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use d3d12_backend::{animation_timing, asset_source, still_dimensions, AnimationTiming, ProjectPaths};
use engine::Time;

use crate::i18n::pack::{Localized, PackTranslations};
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
    /// How long a loop lasts and how many times it plays, for an animated sticker; `None` for a
    /// still one or when the file could not be read.
    pub animation: Option<AnimationTiming>,
    /// The name, category and tags in the languages its pack brings.
    pub i18n: Localized,
}

impl LibrarySticker {
    /// The item that goes through the drag and the add paths of the media list.
    pub fn media_item(&self) -> MediaItem {
        MediaItem { source: self.source.clone(), name: self.name.clone(), kind: MediaKind::Video, duration: Some(self.default_duration()), estimated: true }
    }

    /// Whether the sticker moves.
    pub fn is_animated(&self) -> bool {
        self.animation.is_some()
    }

    /// How long a clip of it is when it is added.
    pub fn default_duration(&self) -> Time {
        self.animation.map_or(DEFAULT_IMAGE_DURATION, |timing| animated_duration(DEFAULT_IMAGE_DURATION, timing))
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

/// The length of a new clip of an animation of `timing`: as many whole loops as come closest to
/// `wanted`, at least one, so the clip does not stop in the middle of a loop; or every loop when the
/// animation plays a fixed number of times.
pub fn animated_duration(wanted: Time, timing: AnimationTiming) -> Time {
    const TICKS_PER_MS: i64 = engine::TICKS_PER_SECOND / 1000;
    let length = i64::try_from(timing.loop_ms).unwrap_or(i64::MAX / TICKS_PER_MS).saturating_mul(TICKS_PER_MS);
    if length <= 0 {
        return wanted;
    }
    let count = if timing.loops != 0 { i64::from(timing.loops) } else { ((wanted.ticks() + length / 2) / length).max(1) };
    Time::from_ticks(length.saturating_mul(count))
}

/// One installed pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerPack {
    pub id: String,
    pub name: String,
    pub stickers: Vec<LibrarySticker>,
    /// The pack's name, description and tags in the languages it brings.
    pub i18n: Localized,
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

    /// `category` in language `code`: the first sticker of that category whose pack translates it,
    /// else `category` itself.
    pub fn category_name<'a>(&'a self, code: &str, category: &'a str) -> &'a str {
        self.packs.iter().flat_map(|pack| &pack.stickers).filter(|sticker| sticker.category == category).map(|sticker| sticker.i18n.category(code, category)).find(|name| *name != category).unwrap_or(category)
    }

    /// The stickers in `category` (any when `None`) that match `search` by name, tag or category, best
    /// first (see [`crate::search`]).
    pub fn matching(&self, search: &str, category: Option<&str>) -> Vec<&LibrarySticker> {
        let stickers = self.packs.iter().flat_map(|pack| &pack.stickers).filter(|sticker| category.is_none_or(|wanted| sticker.category == wanted));
        crate::search::rank(stickers, search, |sticker| sticker.search_fields())
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
    let translations = PackTranslations::read_folder(folder);
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
        // Only an animated WebP's chunks or a GIF's blocks are read for its timing; no frame is decoded.
        let animation = animation_timing(&path).ok().flatten();
        stickers.push(LibrarySticker { i18n: translations.item(&item.id), source, name: item.name, tags: item.tags, category: item.category, size, animation });
    }
    Some(StickerPack { i18n: translations.pack(), id: manifest.id.to_ascii_lowercase(), name: manifest.name, stickers })
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, as it does when a clip resolves it.
pub fn scan_sticker_packs(roots: &[PathBuf]) -> StickerLibrary {
    let mut library = StickerLibrary { loaded: true, ..StickerLibrary::default() };
    let disabled = d3d12_backend::read_disabled_packs(roots);
    let mut seen: HashSet<String> = HashSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("packages").join(STICKER_TYPE)) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        folders.sort();
        for folder in folders {
            let id = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().to_ascii_lowercase());
            if seen.contains(&id) || disabled.contains(&format!("{STICKER_TYPE}/{id}")) {
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
        LibrarySticker { i18n: Default::default(), source: format!("asset:sticker/p/{name}.png"), name: name.to_owned(), tags: tags.iter().map(|tag| (*tag).to_owned()).collect(), category: category.to_owned(), size: None, animation: None }
    }

    fn library() -> StickerLibrary {
        StickerLibrary { packs: vec![StickerPack { i18n: Default::default(), id: "p".to_owned(), name: "P".to_owned(), stickers: vec![sticker("Smile", &["face", "happy"], "people"), sticker("Dog", &["pet"], "animals"), sticker("Sad", &["face"], "people")] }], problems: Vec::new(), loaded: true }
    }

    #[test]
    fn a_category_shows_the_first_translation_a_sticker_of_it_brings() {
        let translations = crate::i18n::pack::PackTranslations::from_files([("vi".to_owned(), r#"{"items":{"b":{"category":"Con người"}}}"#.to_owned())]);
        let mut library = library();
        library.packs[0].stickers[2].i18n = translations.item("b");
        assert_eq!(library.category_name("vi", "people"), "Con người");
        assert_eq!(library.category_name("vi", "animals"), "animals", "untranslated");
        assert_eq!(library.category_name("fr", "people"), "people", "another language");
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
        assert!(!sticker("Smile", &[], "c").is_animated());
    }

    fn ms(ms: i64) -> Time {
        Time::from_ticks(ms * (engine::TICKS_PER_SECOND / 1000))
    }

    #[test]
    fn an_endless_animation_lasts_the_whole_loops_closest_to_the_wanted_length() {
        let endless = |loop_ms| AnimationTiming { loop_ms, loops: 0 };
        assert_eq!(animated_duration(ms(5000), endless(600)), ms(4800), "8 loops are closer than 9");
        assert_eq!(animated_duration(ms(5000), endless(700)), ms(4900));
        assert_eq!(animated_duration(ms(5000), endless(1400)), ms(5600), "4 loops are closer than 3");
        assert_eq!(animated_duration(ms(5000), endless(2000)), ms(6000), "half way rounds up");
        assert_eq!(animated_duration(ms(5000), endless(12_000)), ms(12_000), "at least one loop");
        assert_eq!(animated_duration(ms(5000), endless(0)), ms(5000), "a loop of no length keeps the wanted length");
    }

    #[test]
    fn an_animation_that_plays_a_fixed_number_of_times_lasts_every_loop() {
        assert_eq!(animated_duration(ms(5000), AnimationTiming { loop_ms: 600, loops: 3 }), ms(1800));
        assert_eq!(animated_duration(ms(5000), AnimationTiming { loop_ms: 4000, loops: 2 }), ms(8000));
    }

    #[test]
    fn an_animated_sticker_is_added_as_long_as_its_loops() {
        let mut animated = sticker("Spin", &[], "c");
        animated.animation = Some(AnimationTiming { loop_ms: 600, loops: 0 });
        assert!(animated.is_animated());
        assert_eq!(animated.media_item().duration, Some(ms(4800)));
    }

    #[test]
    fn a_scan_finds_which_stickers_move() {
        let dir = scratch("animated");
        write_pack(&dir, "moving", "Moving", &["images/a.png", "images/b.png"]);
        let pack = dir.join("packages/sticker/moving");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../d3d12-backend/tests/fixtures/animated.webp");
        std::fs::copy(&fixture, pack.join("images/b.webp")).unwrap();
        let manifest = std::fs::read_to_string(pack.join("manifest.json")).unwrap().replace("images/b.png", "images/b.webp");
        std::fs::write(pack.join("manifest.json"), manifest).unwrap();

        let library = scan_sticker_packs(std::slice::from_ref(&dir));

        let stickers = &library.packs[0].stickers;
        assert_eq!(stickers.iter().map(LibrarySticker::is_animated).collect::<Vec<_>>(), vec![false, true]);
        assert_eq!(stickers[1].animation, Some(AnimationTiming { loop_ms: 600, loops: 0 }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
