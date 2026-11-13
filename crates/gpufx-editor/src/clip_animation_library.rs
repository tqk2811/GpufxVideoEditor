//! The clip animation packs installed on this machine. No window here.
//!
//! Each item of a pack is a preset a clip names as `"<pack id>/<item id>"`, the pack id lower-cased.
//! The scan only reads manifests; the engine gets the curves as a table (see
//! [`ClipAnimationLibrary::preset_table`]) and never reads a pack itself.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine::{AnimationCurve, AnimationGroup, Clip, Time};

use crate::audio_library::LibraryProblem;
use crate::clip_animation_install::{check_clip_animation_fields, parse_clip_animation_manifest, CLIP_ANIMATION_TYPE};
use crate::timecode::from_seconds;

/// The kind of clip an animation can be offered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimatedClipKind {
    Text,
    Image,
    Video,
    Sticker,
}

impl AnimatedClipKind {
    /// The kind of `name` in a manifest's `applies_to`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "sticker" => Some(Self::Sticker),
            _ => None,
        }
    }

    /// The kind of `clip`, which is on a track with pictures.
    pub fn of(clip: &Clip) -> Self {
        if clip.is_text() {
            Self::Text
        } else if clip.source.starts_with("asset:sticker/") {
            Self::Sticker
        } else if clip.is_image() {
            Self::Image
        } else {
            Self::Video
        }
    }
}

/// One animation of a pack.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryAnimation {
    /// What a clip stores as its preset: `"<pack id>/<item id>"`.
    pub reference: String,
    pub name: String,
    pub group: AnimationGroup,
    pub default_duration: Time,
    /// The kinds of clip it is offered for; every kind when `None`.
    pub applies_to: Option<Vec<AnimatedClipKind>>,
    pub curve: AnimationCurve,
}

impl LibraryAnimation {
    /// Whether the animation is offered for a clip of `kind`.
    pub fn applies_to(&self, kind: AnimatedClipKind) -> bool {
        self.applies_to.as_ref().is_none_or(|kinds| kinds.contains(&kind))
    }
}

/// One installed pack.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipAnimationPack {
    pub id: String,
    pub name: String,
    pub animations: Vec<LibraryAnimation>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipAnimationLibrary {
    pub packs: Vec<ClipAnimationPack>,
    pub problems: Vec<LibraryProblem>,
    /// The first scan has finished.
    pub loaded: bool,
}

impl ClipAnimationLibrary {
    /// Every animation of `group` offered for a clip of `kind`, with the name of its pack, packs in
    /// the order of the library.
    pub fn offered(&self, group: AnimationGroup, kind: AnimatedClipKind) -> Vec<(&str, &LibraryAnimation)> {
        self.packs.iter().flat_map(|pack| pack.animations.iter().map(move |animation| (pack.name.as_str(), animation))).filter(|(_, animation)| animation.group == group && animation.applies_to(kind)).collect()
    }

    /// The animation a clip names as `reference` in `group`.
    pub fn find(&self, group: AnimationGroup, reference: &str) -> Option<(&str, &LibraryAnimation)> {
        self.packs.iter().flat_map(|pack| pack.animations.iter().map(move |animation| (pack.name.as_str(), animation))).find(|(_, animation)| animation.group == group && animation.reference == reference)
    }
}

/// What a clip stores to name item `item` of pack `pack`.
pub fn pack_reference(pack: &str, item: &str) -> String {
    format!("{}{}{item}", pack.to_ascii_lowercase(), engine::PACK_PRESET_SEPARATOR)
}

fn read_pack(folder: &Path, library: &mut ClipAnimationLibrary) -> Option<ClipAnimationPack> {
    let label = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut problem = |message: String| library.problems.push(LibraryProblem { pack: label.clone(), message });
    let bytes = match std::fs::read(folder.join("manifest.json")) {
        Ok(bytes) => bytes,
        Err(error) => {
            problem(error.to_string());
            return None;
        }
    };
    let manifest = match parse_clip_animation_manifest(&bytes).and_then(|manifest| check_clip_animation_fields(&manifest).map(|()| manifest)) {
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
    let id = manifest.id.to_ascii_lowercase();
    let animations = manifest
        .items
        .into_iter()
        .filter_map(|item| {
            let group = item.group()?;
            Some(LibraryAnimation {
                reference: pack_reference(&id, &item.id),
                name: item.name,
                group,
                default_duration: from_seconds(item.default_duration).max(Time::from_ticks(1)),
                applies_to: item.applies_to.map(|kinds| kinds.iter().filter_map(|kind| AnimatedClipKind::from_name(kind)).collect()),
                curve: item.curve,
            })
        })
        .collect();
    Some(ClipAnimationPack { id, name: manifest.name, animations })
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, as for sticker packs.
pub fn scan_clip_animation_packs(roots: &[PathBuf]) -> ClipAnimationLibrary {
    let mut library = ClipAnimationLibrary { loaded: true, ..ClipAnimationLibrary::default() };
    let mut seen: HashSet<String> = HashSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("packages").join(CLIP_ANIMATION_TYPE)) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        folders.sort();
        for folder in folders {
            let id = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().to_ascii_lowercase());
            if seen.contains(&id) {
                continue;
            }
            if let Some(pack) = read_pack(&folder, &mut library) {
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-clip-animation-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_pack(root: &Path, folder: &str, name: &str) {
        let path = root.join("packages/clip-animation").join(folder);
        std::fs::create_dir_all(&path).unwrap();
        let manifest = format!(
            r#"{{"format":1,"type":"clip-animation","id":"{folder}","name":"{name}","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{{"id":"rise","name":"Rise","kind":"in","default_duration":0.5,"applies_to":["text"],"curve":{{"tracks":{{"dy":[{{"t":0,"value":0.1}},{{"t":1,"value":0}}]}}}}}},{{"id":"wave","name":"Wave","kind":"loop","default_duration":2,"curve":{{"tracks":{{"rotation":[{{"t":0,"value":0}},{{"t":0.5,"value":10}},{{"t":1,"value":0}}]}}}}}}]}}"#
        );
        std::fs::write(path.join("manifest.json"), manifest).unwrap();
    }

    #[test]
    fn a_scan_reads_each_pack_once_and_reports_what_is_broken() {
        let dir = scratch("scan");
        let (shipped, user) = (dir.join("shipped"), dir.join("user"));
        write_pack(&shipped, "Shared", "From Shipped");
        write_pack(&user, "shared", "From User");
        write_pack(&user, "other", "Other");
        std::fs::create_dir_all(user.join("packages/clip-animation/broken")).unwrap();
        std::fs::write(user.join("packages/clip-animation/broken/manifest.json"), b"not json").unwrap();

        let library = scan_clip_animation_packs(&[shipped, user]);

        assert!(library.loaded);
        let packs: Vec<&str> = library.packs.iter().map(|pack| pack.name.as_str()).collect();
        assert_eq!(packs, vec!["From Shipped", "Other"], "the first root wins");
        let rise = &library.packs[0].animations[0];
        assert_eq!((rise.reference.as_str(), rise.group, rise.default_duration), ("shared/rise", AnimationGroup::Enter, from_seconds(0.5)));
        assert_eq!(library.problems.len(), 1, "{:?}", library.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn applies_to_filters_what_a_clip_is_offered() {
        let dir = scratch("offered");
        write_pack(&dir, "p", "P");
        let library = scan_clip_animation_packs(&[dir.clone()]);
        let names = |group, kind| library.offered(group, kind).iter().map(|(pack, animation)| format!("{pack}:{}", animation.name)).collect::<Vec<_>>();
        assert_eq!(names(AnimationGroup::Enter, AnimatedClipKind::Text), vec!["P:Rise"]);
        assert!(names(AnimationGroup::Enter, AnimatedClipKind::Video).is_empty(), "Rise is for text only");
        assert_eq!(names(AnimationGroup::Repeat, AnimatedClipKind::Sticker), vec!["P:Wave"], "no applies_to: every kind");
        assert!(library.find(AnimationGroup::Repeat, "p/wave").is_some());
        assert!(library.find(AnimationGroup::Enter, "p/wave").is_none(), "another group");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
