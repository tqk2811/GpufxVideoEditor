//! The one strict reader of the clip animation packs installed on a machine, for the editor and for
//! an export alike.
//!
//! A pack is a folder `<root>/packages/clip-animation/<id lower-cased>/` with a `manifest.json`. A
//! pack is taken whole or not at all: any field the format does not allow, an id that is not the
//! folder name, one item with a curve that cannot play, and the pack is refused. Only an accepted
//! pack takes its id, so a refused pack in an earlier root lets a pack of the same id in a later
//! root be used, the same in the editor and in an export. The installer checks a zip with
//! [`check_clip_animation_fields`] too, so what installs is what reads.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine::{check_curve, AnimationCurve, AnimationGroup};
use serde::Deserialize;

use crate::paths::is_safe_id;

/// What the `type` of a clip animation pack says, and the folder under `packages` it goes to.
pub const CLIP_ANIMATION_FOLDER: &str = "clip-animation";
/// The older name of the type, still accepted.
pub const CLIP_ANIMATION_TYPE_ALIAS: &str = "text-animation";
/// The longest default duration an item may ask for, in seconds.
pub const MAX_DEFAULT_DURATION: f64 = 60.0;
/// The kinds of clip an item may apply to.
pub const CLIP_KINDS: [&str; 4] = ["text", "image", "video", "sticker"];
/// The format of a manifest this build reads.
pub const SUPPORTED_FORMAT: u32 = 1;
/// The most items a manifest may list; a pack is read whole and listed in the window.
pub const MAX_MANIFEST_ITEMS: usize = 16_384;

/// Whether `kind` is the type of a clip animation pack.
pub fn is_clip_animation_type(kind: &str) -> bool {
    kind == CLIP_ANIMATION_FOLDER || kind == CLIP_ANIMATION_TYPE_ALIAS
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackLicense {
    pub spdx: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipAnimationManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: PackLicense,
    pub items: Vec<ClipAnimationManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipAnimationManifestItem {
    pub id: String,
    pub name: String,
    /// `in`, `out` or `loop`.
    pub kind: String,
    /// Seconds; for a loop item, one cycle.
    pub default_duration: f64,
    /// The kinds of clip (see [`CLIP_KINDS`]) the item is offered for; every kind when absent.
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    pub curve: AnimationCurve,
    /// Reserved for a gpufx shader the item will run on top of its curve. It is read and kept, but
    /// not used yet.
    #[serde(default)]
    pub effect: Option<serde_json::Value>,
}

impl ClipAnimationManifestItem {
    /// The group of the item, `None` for a kind that is not one.
    pub fn group(&self) -> Option<AnimationGroup> {
        [AnimationGroup::Enter, AnimationGroup::Exit, AnimationGroup::Repeat].into_iter().find(|group| group.name() == self.kind)
    }
}

/// Why a manifest is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestProblem {
    UnknownType(String),
    BadId(String),
    BadManifest(String),
}

impl std::fmt::Display for ManifestProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownType(kind) => write!(f, "the type \"{kind}\" is not a clip animation pack"),
            Self::BadId(id) => write!(f, "the id \"{id}\" cannot be a folder name"),
            Self::BadManifest(message) => f.write_str(message),
        }
    }
}

/// Parses a manifest. The fields are exactly those of the format.
pub fn parse_clip_animation_manifest(bytes: &[u8]) -> Result<ClipAnimationManifest, ManifestProblem> {
    serde_json::from_slice(bytes).map_err(|e| ManifestProblem::BadManifest(e.to_string()))
}

fn bad<T>(message: String) -> Result<T, ManifestProblem> {
    Err(ManifestProblem::BadManifest(message))
}

fn check_item(item: &ClipAnimationManifestItem) -> Result<(), ManifestProblem> {
    let id = &item.id;
    if !is_safe_id(id) {
        return bad(format!("the item id \"{id}\" may only hold letters, digits, '.', '-' and '_'"));
    }
    if item.group().is_none() {
        return bad(format!("the item \"{id}\" has the kind \"{}\", not in, out or loop", item.kind));
    }
    if !(item.default_duration.is_finite() && item.default_duration > 0.0 && item.default_duration <= MAX_DEFAULT_DURATION) {
        return bad(format!("the item \"{id}\" must last more than 0 and at most {MAX_DEFAULT_DURATION} seconds"));
    }
    if let Some(kinds) = &item.applies_to {
        if kinds.is_empty() {
            return bad(format!("applies_to of the item \"{id}\" is empty"));
        }
        let mut seen = HashSet::new();
        for kind in kinds {
            if !CLIP_KINDS.contains(&kind.as_str()) {
                return bad(format!("the item \"{id}\" applies to \"{kind}\", which is not text, image, video or sticker"));
            }
            if !seen.insert(kind) {
                return bad(format!("the item \"{id}\" lists \"{kind}\" twice in applies_to"));
            }
        }
    }
    check_curve(&item.curve).or_else(|message| bad(format!("the item \"{id}\": {message}")))
}

/// Checks what a manifest says, apart from the files.
pub fn check_clip_animation_fields(manifest: &ClipAnimationManifest) -> Result<(), ManifestProblem> {
    if !is_clip_animation_type(&manifest.kind) {
        return Err(ManifestProblem::UnknownType(manifest.kind.clone()));
    }
    if manifest.format != SUPPORTED_FORMAT {
        return bad(format!("format {} is not supported", manifest.format));
    }
    if !is_safe_id(&manifest.id) {
        return Err(ManifestProblem::BadId(manifest.id.clone()));
    }
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() || manifest.license.spdx.trim().is_empty() {
        return bad("name, version and license.spdx must not be empty".to_owned());
    }
    if manifest.license.files.is_empty() {
        return bad("license.files is empty".to_owned());
    }
    let count = manifest.items.len();
    if count == 0 {
        return bad("items is empty".to_owned());
    }
    if count > MAX_MANIFEST_ITEMS {
        return bad(format!("items lists {count} animations, more than the {MAX_MANIFEST_ITEMS} allowed"));
    }
    let mut ids = HashSet::new();
    for item in &manifest.items {
        if item.id.trim().is_empty() || item.name.trim().is_empty() {
            return bad("an item has an empty id or name".to_owned());
        }
        if !ids.insert(item.id.to_lowercase()) {
            return bad(format!("the item id \"{}\" is used twice", item.id));
        }
    }
    manifest.items.iter().try_for_each(check_item)
}

/// A pack that was refused: its folder name and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackReadProblem {
    pub pack: String,
    pub message: String,
}

/// Every accepted pack under the roots, and every pack refused.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipAnimationScan {
    /// The manifests in the order they were read; the `id` of each is the folder name, any case.
    pub packs: Vec<ClipAnimationManifest>,
    pub problems: Vec<PackReadProblem>,
}

/// Reads and checks the pack in `folder`, whose name is the pack id.
fn read_pack(folder: &Path, label: &str) -> Result<ClipAnimationManifest, String> {
    let bytes = std::fs::read(folder.join("manifest.json")).map_err(|e| e.to_string())?;
    let manifest = parse_clip_animation_manifest(&bytes).map_err(|e| e.to_string())?;
    check_clip_animation_fields(&manifest).map_err(|e| e.to_string())?;
    if !manifest.id.eq_ignore_ascii_case(label) {
        return Err(format!("its manifest does not match the folder ({})", manifest.id));
    }
    Ok(manifest)
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, but only when it is accepted: a refused pack is reported and leaves its id free.
pub fn scan_clip_animation_folders(roots: &[PathBuf]) -> ClipAnimationScan {
    let mut scan = ClipAnimationScan::default();
    let mut taken: HashSet<String> = HashSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("packages").join(CLIP_ANIMATION_FOLDER)) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        folders.sort();
        for folder in folders {
            let label = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let id = label.to_ascii_lowercase();
            if taken.contains(&id) {
                continue;
            }
            match read_pack(&folder, &label) {
                Ok(manifest) => {
                    taken.insert(id);
                    scan.packs.push(manifest);
                }
                Err(message) => scan.problems.push(PackReadProblem { pack: label, message }),
            }
        }
    }
    scan
}
