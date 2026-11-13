//! The curves of the installed clip animation packs, for a process that renders without the editor
//! (an export). The editor reads the same packs, with every check of their manifest, for its
//! inspector; here only what a curve needs is read, and an item that cannot play is left out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine::{check_curve, AnimationCurve, AnimationGroup, PackPreset, PackPresets, PACK_PRESET_SEPARATOR};
use serde::Deserialize;

use crate::paths::is_safe_id;

/// The folder under `packages` the clip animation packs are installed to.
pub const CLIP_ANIMATION_FOLDER: &str = "clip-animation";

#[derive(Deserialize)]
struct Manifest {
    id: String,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    kind: String,
    curve: AnimationCurve,
}

fn group_of(kind: &str) -> Option<AnimationGroup> {
    [AnimationGroup::Enter, AnimationGroup::Exit, AnimationGroup::Repeat].into_iter().find(|group| group.name() == kind)
}

fn read_pack(folder: &Path) -> Option<(String, Vec<PackPreset>)> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(folder.join("manifest.json")).ok()?).ok()?;
    let id = manifest.id.to_ascii_lowercase();
    let label = folder.file_name()?.to_string_lossy().to_ascii_lowercase();
    if !is_safe_id(&id) || id != label {
        return None;
    }
    let presets = manifest
        .items
        .into_iter()
        .filter(|item| is_safe_id(&item.id) && check_curve(&item.curve).is_ok())
        .filter_map(|item| Some(PackPreset::new(format!("{id}{PACK_PRESET_SEPARATOR}{}", item.id), group_of(&item.kind)?, item.curve)))
        .collect();
    Some((id, presets))
}

/// The presets of the packs under every root. A pack in an earlier root hides one of the same id
/// in a later one, as for the editor.
pub fn read_pack_presets(roots: &[PathBuf]) -> PackPresets {
    let mut seen = HashSet::new();
    let mut presets = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("packages").join(CLIP_ANIMATION_FOLDER)) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        folders.sort();
        for folder in folders {
            if let Some((id, pack)) = read_pack(&folder) {
                if seen.insert(id) {
                    presets.extend(pack);
                }
            }
        }
    }
    PackPresets::new(presets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_root_wins_and_a_broken_item_is_left_out() {
        let dir = std::env::temp_dir().join(format!("gpufx-pack-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let write = |root: &str, folder: &str, items: &str| {
            let path = dir.join(root).join("packages/clip-animation").join(folder);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("manifest.json"), format!(r#"{{"format":1,"type":"clip-animation","id":"{folder}","name":"N","items":[{items}]}}"#)).unwrap();
        };
        let good = r#"{"id":"rise","name":"Rise","kind":"in","default_duration":1,"curve":{"tracks":{"opacity":[{"t":0,"value":0},{"t":1,"value":1}]}}}"#;
        let bad_t = r#"{"id":"bad","name":"Bad","kind":"in","default_duration":1,"curve":{"tracks":{"opacity":[{"t":2,"value":0}]}}}"#;
        let bad_kind = r#"{"id":"odd","name":"Odd","kind":"middle","default_duration":1,"curve":{"tracks":{"opacity":[{"t":0,"value":0}]}}}"#;
        write("a", "Moves", &format!("{good},{bad_t},{bad_kind}"));
        write("b", "moves", r#"{"id":"other","name":"O","kind":"out","default_duration":1,"curve":{"tracks":{"dx":[{"t":0,"value":0}]}}}"#);
        write("b", "mismatch", "");
        std::fs::rename(dir.join("b/packages/clip-animation/mismatch"), dir.join("b/packages/clip-animation/elsewhere")).unwrap();

        let presets = read_pack_presets(&[dir.join("a"), dir.join("b")]);
        let names: Vec<&str> = presets.presets().iter().map(|preset| preset.reference.as_str()).collect();
        assert_eq!(names, vec!["moves/rise"]);
        assert!(presets.find(AnimationGroup::Enter, "moves/rise").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
