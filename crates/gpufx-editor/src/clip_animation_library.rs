//! The clip animation packs installed on this machine. No window here.
//!
//! Each item of a pack is a preset a clip names as `"<pack id>/<item id>"`, the pack id lower-cased.
//! The scan only reads manifests; the engine gets the curves as a table (see
//! [`ClipAnimationLibrary::preset_table`]) and never reads a pack itself. An item may also run a
//! gpufx effect; the scan finds its package (see [`d3d12_backend::resolve_item_effects`]) and a
//! missing one is listed among the problems while the item keeps its curve.

use std::path::PathBuf;

use std::collections::HashMap;

use d3d12_backend::{resolve_item_effects, scan_clip_animation_folders, ClipAnimationManifest};
use engine::{AnimationCurve, AnimationGroup, Clip, PackPreset, PackPresets, PresetEffect, Time};

use crate::audio_library::LibraryProblem;
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

    /// The name of the kind in a manifest's `applies_to`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Video => "video",
            Self::Sticker => "sticker",
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
    /// The gpufx package id the item's effect names, when it has one.
    pub effect_package: Option<String>,
    /// That effect, when its package is installed and fits; it plays on top of the curve.
    pub effect: Option<PresetEffect>,
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

    /// The curves of every animation, for [`engine::set_pack_presets`].
    pub fn preset_table(&self) -> PackPresets {
        PackPresets::new(self.packs.iter().flat_map(|pack| &pack.animations).map(|animation| PackPreset::new(animation.reference.clone(), animation.group, animation.curve.clone()).with_effect(animation.effect.clone())).collect())
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

fn library_pack(manifest: ClipAnimationManifest, effects: &mut HashMap<String, PresetEffect>) -> ClipAnimationPack {
    let id = manifest.id.to_ascii_lowercase();
    let animations = manifest
        .items
        .into_iter()
        .filter_map(|item| {
            let group = item.group()?;
            let reference = pack_reference(&id, &item.id);
            let effect = effects.remove(&reference);
            // An item with only an effect has nothing to play without it, as for an export.
            if effect.is_none() && item.curve.tracks.is_empty() {
                return None;
            }
            Some(LibraryAnimation {
                effect,
                effect_package: item.effect.map(|effect| effect.package),
                reference,
                name: item.name,
                group,
                default_duration: from_seconds(item.default_duration).max(Time::from_ticks(1)),
                applies_to: item.applies_to.map(|kinds| kinds.iter().filter_map(|kind| AnimatedClipKind::from_name(kind)).collect()),
                curve: item.curve,
            })
        })
        .collect();
    ClipAnimationPack { id, name: manifest.name, animations }
}

/// Reads the packs under every root with the reader an export uses
/// ([`d3d12_backend::scan_clip_animation_folders`]): a pack is accepted whole or reported, and a
/// pack in an earlier root hides one of the same id in a later one only when it is accepted.
pub fn scan_clip_animation_packs(roots: &[PathBuf]) -> ClipAnimationLibrary {
    let scan = scan_clip_animation_folders(roots);
    // An item whose effect package is missing still plays its curve; the problem says why.
    let effects = resolve_item_effects(roots, &scan.packs);
    let mut ready = effects.ready;
    let problems = scan.problems.into_iter().chain(effects.problems).map(|problem| LibraryProblem { pack: problem.pack, message: problem.message }).collect();
    let mut packs: Vec<ClipAnimationPack> = scan.packs.into_iter().map(|manifest| library_pack(manifest, &mut ready)).collect();
    packs.sort_by_key(|pack| pack.name.to_lowercase());
    ClipAnimationLibrary { packs, problems, loaded: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
    fn the_editor_and_an_export_read_the_same_packs() {
        let dir = scratch("same-reader");
        let (a, b) = (dir.join("a"), dir.join("b"));
        // The pack in root a has one item whose curve cannot play, so it is refused whole and the
        // pack of the same id in root b is the one both readers use.
        write_pack(&b, "shared", "From B");
        write_pack(&a, "shared", "From A");
        let manifest = a.join("packages/clip-animation/shared/manifest.json");
        let broken = std::fs::read_to_string(&manifest).unwrap().replace(r#""t":1,"value":0"#, r#""t":2,"value":0"#);
        std::fs::write(&manifest, broken).unwrap();
        write_pack(&a, "only-a", "Only A");
        // An item with only an effect whose package is not installed is offered by neither.
        let effect_only = a.join("packages/clip-animation/effect-only");
        std::fs::create_dir_all(&effect_only).unwrap();
        std::fs::write(effect_only.join("manifest.json"), r#"{"format":1,"type":"clip-animation","id":"effect-only","name":"E","version":"1.0.0","license":{"spdx":"MIT","files":["LICENSE.txt"]},"items":[{"id":"wipe","name":"Wipe","kind":"in","default_duration":1,"effect":{"package":"test.absent"}}]}"#).unwrap();

        let roots = [a, b];
        let library = scan_clip_animation_packs(&roots);
        let exported = d3d12_backend::read_pack_presets(&roots);
        let mut from_library: Vec<String> = library.preset_table().presets().iter().map(|preset| preset.reference.clone()).collect();
        let mut from_export: Vec<String> = exported.presets().iter().map(|preset| preset.reference.clone()).collect();
        from_library.sort();
        from_export.sort();
        assert_eq!(from_library, vec!["only-a/rise", "only-a/wave", "shared/rise", "shared/wave"]);
        assert_eq!(from_library, from_export);
        assert!(library.packs.iter().all(|pack| pack.animations.iter().all(|animation| animation.reference != "effect-only/wipe")));
        assert_eq!(library.problems.len(), 2, "the refused pack and the missing package: {:?}", library.problems);
        assert!(library.packs.iter().any(|pack| pack.name == "From B"));
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
        let table = library.preset_table();
        assert_eq!(table.presets().len(), 2);
        assert!(table.find(AnimationGroup::Repeat, "p/wave").is_some_and(engine::PackPreset::rotates));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
