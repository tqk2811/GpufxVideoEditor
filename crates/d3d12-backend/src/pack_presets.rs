//! The curves of the installed clip animation packs, for a process that renders without the editor
//! (an export). The packs are read by [`crate::clip_animation_pack`], the same reader as the
//! editor's, so both see the same packs.

use std::path::PathBuf;

use engine::{PackPreset, PackPresets, PACK_PRESET_SEPARATOR};

use crate::clip_animation_pack::scan_clip_animation_folders;
use crate::preset_effects::resolve_item_effects;

/// The presets of the accepted packs under every root. A pack in an earlier root hides one of the
/// same id in a later one, as for the editor. An item effect whose gpufx package is under the
/// roots plays with the item; one that is not is left out (see [`crate::preset_effects`]), and so
/// is an item that has nothing else.
pub fn read_pack_presets(roots: &[PathBuf]) -> PackPresets {
    let packs = scan_clip_animation_folders(roots).packs;
    let mut effects = resolve_item_effects(roots, &packs).ready;
    let mut presets = Vec::new();
    for manifest in packs {
        let id = manifest.id.to_ascii_lowercase();
        for item in manifest.items {
            let Some(group) = item.group() else { continue };
            let reference = format!("{id}{PACK_PRESET_SEPARATOR}{}", item.id);
            let effect = effects.remove(&reference);
            // An item with only an effect has nothing to play without it; the problem says why.
            if effect.is_none() && item.curve.tracks.is_empty() && item.per_unit.is_none() {
                continue;
            }
            presets.push(PackPreset::new(reference, group, item.curve).with_effect(effect).with_per_unit(item.per_unit));
        }
    }
    PackPresets::new(presets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::AnimationGroup;

    fn manifest(id: &str, items: &str) -> String {
        format!(r#"{{"format":1,"type":"clip-animation","id":"{id}","name":"N","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{items}]}}"#)
    }

    fn write(dir: &std::path::Path, root: &str, folder: &str, text: &str) {
        let path = dir.join(root).join("packages/clip-animation").join(folder);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("manifest.json"), text).unwrap();
    }

    const GOOD: &str = r#"{"id":"rise","name":"Rise","kind":"in","default_duration":1,"curve":{"tracks":{"opacity":[{"t":0,"value":0},{"t":1,"value":1}]}}}"#;
    const BAD_T: &str = r#"{"id":"bad","name":"Bad","kind":"in","default_duration":1,"curve":{"tracks":{"opacity":[{"t":2,"value":0}]}}}"#;

    #[test]
    fn the_first_accepted_pack_wins_and_a_broken_pack_is_refused_whole() {
        let dir = std::env::temp_dir().join(format!("gpufx-pack-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Root a: "moves" has one item that cannot play, so the whole pack is refused and root b's
        // "moves" is the one used. "good" is accepted in a and hides b's.
        write(&dir, "a", "Moves", &manifest("Moves", &format!("{GOOD},{BAD_T}")));
        write(&dir, "a", "good", &manifest("good", GOOD));
        write(&dir, "b", "moves", &manifest("moves", r#"{"id":"other","name":"O","kind":"out","default_duration":1,"curve":{"tracks":{"dx":[{"t":0,"value":0}]}}}"#));
        write(&dir, "b", "good", &manifest("good", &GOOD.replace("rise", "other")));
        // A folder whose manifest names another id is refused.
        write(&dir, "b", "elsewhere", &manifest("mismatch", GOOD));

        let scan = scan_clip_animation_folders(&[dir.join("a"), dir.join("b")]);
        assert_eq!(scan.problems.iter().map(|problem| problem.pack.as_str()).collect::<Vec<_>>(), vec!["Moves", "elsewhere"]);
        let presets = read_pack_presets(&[dir.join("a"), dir.join("b")]);
        let mut names: Vec<&str> = presets.presets().iter().map(|preset| preset.reference.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["good/rise", "moves/other"]);
        assert!(presets.find(AnimationGroup::Exit, "moves/other").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_per_unit_item_is_read_for_text_only() {
        let dir = std::env::temp_dir().join(format!("gpufx-pack-presets-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let per_unit = r#""per_unit":{"unit":"word","order":"random","seed":3,"selector":{"offset":[{"t":0,"value":-1},{"t":1,"value":1}],"shape":"ramp_down"},"curve":{"tracks":{"opacity":[{"t":0,"value":0}],"dy":[{"t":0,"value":0.5}]}}}"#;
        write(&dir, "a", "words", &manifest("words", &format!(r#"{{"id":"rise","name":"Rise","kind":"in","default_duration":1,"applies_to":["text"],{per_unit}}}"#)));
        write(&dir, "a", "notext", &manifest("notext", &format!(r#"{{"id":"rise","name":"Rise","kind":"in","default_duration":1,{per_unit}}}"#)));
        write(&dir, "a", "unknown", &manifest("unknown", r#"{"id":"rise","name":"Rise","kind":"in","default_duration":1,"applies_to":["text"],"per_unit":{"stagger":0.5,"wobble":1,"curve":{"tracks":{"opacity":[{"t":0,"value":0}]}}}}"#));
        let scan = scan_clip_animation_folders(&[dir.join("a")]);
        let mut refused: Vec<&str> = scan.problems.iter().map(|problem| problem.pack.as_str()).collect();
        refused.sort_unstable();
        assert_eq!(refused, vec!["notext", "unknown"], "applies_to must be text, and a field the format does not know is refused");
        let presets = read_pack_presets(&[dir.join("a")]);
        let preset = presets.find(AnimationGroup::Enter, "words/rise").expect("an item with only per_unit plays");
        let per_unit = preset.per_unit.as_ref().expect("its per_unit");
        assert_eq!((per_unit.unit, per_unit.order, per_unit.seed), (engine::TextUnit::Word, engine::UnitOrder::Random, 3));
        assert_eq!(per_unit.selector.as_ref().map(|selector| selector.shape), Some(engine::SelectorShape::RampDown));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
