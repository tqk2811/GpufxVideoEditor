//! The curves of the installed clip animation packs, for a process that renders without the editor
//! (an export). The packs are read by [`crate::clip_animation_pack`], the same reader as the
//! editor's, so both see the same packs.

use std::path::PathBuf;

use engine::{PackPreset, PackPresets, PACK_PRESET_SEPARATOR};

use crate::clip_animation_pack::scan_clip_animation_folders;

/// The presets of the accepted packs under every root. A pack in an earlier root hides one of the
/// same id in a later one, as for the editor.
pub fn read_pack_presets(roots: &[PathBuf]) -> PackPresets {
    let mut presets = Vec::new();
    for manifest in scan_clip_animation_folders(roots).packs {
        let id = manifest.id.to_ascii_lowercase();
        for item in manifest.items {
            let Some(group) = item.group() else { continue };
            presets.push(PackPreset::new(format!("{id}{PACK_PRESET_SEPARATOR}{}", item.id), group, item.curve));
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
}
