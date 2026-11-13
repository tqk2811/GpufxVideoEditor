//! Installing a clip animation pack from a `.zip`. No window here.
//!
//! A zip with `"type": "clip-animation"` (or its older name `text-animation`) is unpacked once to
//! `<user root>/packages/clip-animation/<id lower-cased>/` through the same path as a sticker pack.
//! It holds no media: only the manifest, whose items are animation curves, and its licence files.
//! A clip names an item as `"<pack id>/<item id>"` (see [`crate::clip_animation_library`]).
//!
//! A shared module for every pack type was considered; the user chose one install module per pack
//! type, which share only the helpers of [`crate::asset_install`].

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

use d3d12_backend::is_safe_id;
use engine::{check_curve, AnimationCurve, AnimationGroup};
use serde::Deserialize;
use zip::ZipArchive;

use crate::asset_install::{check_entry_extensions, check_item_ids, check_named_files, check_pack_header, clean_leftovers, lock_installs, rename_with_retries, unpack_into_place, AudioLicense, ExtractLimits, Unpacked, MANIFEST, MAX_COMPRESSION_RATIO};
use crate::package_import::{entries_of, read_manifest, validate_entries, ImportOutcome, ImportProblem, Limits, MIB};

/// What the `type` of a clip animation pack says, and the folder it goes to.
pub const CLIP_ANIMATION_TYPE: &str = "clip-animation";
/// The older name of the type, still accepted.
pub const CLIP_ANIMATION_TYPE_ALIAS: &str = "text-animation";
/// A pack is a manifest and a few licence files.
pub const CLIP_ANIMATION_LIMITS: Limits = Limits { files: 64, total_bytes: 16 * MIB, file_bytes: 8 * MIB };
const CLIP_ANIMATION_EXTRACT: ExtractLimits = ExtractLimits { limits: CLIP_ANIMATION_LIMITS, max_ratio: MAX_COMPRESSION_RATIO };
/// The longest default duration an item may ask for, in seconds.
pub const MAX_DEFAULT_DURATION: f64 = 60.0;
/// The kinds of clip an item may apply to.
pub const CLIP_KINDS: [&str; 4] = ["text", "image", "video", "sticker"];

/// Whether `kind` is the type of a clip animation pack.
pub fn is_clip_animation_type(kind: &str) -> bool {
    kind == CLIP_ANIMATION_TYPE || kind == CLIP_ANIMATION_TYPE_ALIAS
}

/// A clip animation pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledClipAnimationPack {
    pub id: String,
    pub name: String,
    pub replaced: bool,
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
    pub license: AudioLicense,
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

/// Parses a manifest. The fields are exactly those of the format.
pub fn parse_clip_animation_manifest(bytes: &[u8]) -> Result<ClipAnimationManifest, ImportProblem> {
    serde_json::from_slice(bytes).map_err(|e| ImportProblem::BadManifest(e.to_string()))
}

fn bad<T>(message: String) -> Result<T, ImportProblem> {
    Err(ImportProblem::BadManifest(message))
}

fn check_item(item: &ClipAnimationManifestItem) -> Result<(), ImportProblem> {
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
pub fn check_clip_animation_fields(manifest: &ClipAnimationManifest) -> Result<(), ImportProblem> {
    if !is_clip_animation_type(&manifest.kind) {
        return Err(ImportProblem::UnknownType(manifest.kind.clone()));
    }
    check_pack_header(manifest.format, &manifest.id, &manifest.name, &manifest.version, &manifest.license, manifest.items.len(), "animations")?;
    check_item_ids(manifest.items.iter().map(|item| (item.id.as_str(), item.name.as_str())))?;
    manifest.items.iter().try_for_each(check_item)
}

/// A pack holds no media file.
fn no_media(_: &str) -> bool {
    false
}

/// Checks the zip at `source` and installs it under `user_root`. An existing pack of the same id is
/// replaced only when `overwrite` allows it.
pub fn install_clip_animation_pack(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    // Held for the whole install, as for the other packs: the working folders are shared.
    let _lock = lock_installs(&packages).map_err(|e| ImportProblem::WriteFailed(format!("another install is running ({e})")))?;
    clean_leftovers(&packages);

    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, CLIP_ANIMATION_LIMITS)?;
    validate_entries(&entries, CLIP_ANIMATION_LIMITS)?;
    check_entry_extensions(&entries, no_media)?;
    let manifest = parse_clip_animation_manifest(&read_manifest(&mut zip)?)?;
    check_clip_animation_fields(&manifest)?;
    check_named_files(&manifest.license.files, std::iter::empty(), &entries, no_media, "media")?;

    let id = manifest.id.to_ascii_lowercase();
    let check_unpacked = |temporary: &Path| {
        let unpacked = parse_clip_animation_manifest(&fs::read(temporary.join(MANIFEST)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?)?;
        if unpacked == manifest {
            Ok(())
        } else {
            Err(ImportProblem::BadManifest("it changed while the zip was being read".to_owned()))
        }
    };
    let unpacked = unpack_into_place(&mut zip, &entries, &packages, CLIP_ANIMATION_TYPE, &id, &manifest.name, overwrite, CLIP_ANIMATION_EXTRACT, &mut rename_with_retries, check_unpacked)?;
    Ok(match unpacked {
        Unpacked::AlreadyThere { name } => ImportOutcome::AlreadyInstalled { name },
        Unpacked::Done { replaced } => ImportOutcome::ClipAnimationInstalled(InstalledClipAnimationPack { id, name: manifest.name, replaced }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_install::recover_interrupted_install_with;
    use crate::package_import::{install_zip, EntryProblem};
    use std::io::{Cursor, Write};
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-clip-animation-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MANIFEST_JSON: &str = r#"{"format":1,"type":"clip-animation","id":"Avs-Moves","name":"Moves","version":"1.0.0","license":{"spdx":"MIT","files":["LICENSE.txt"]},"items":[{"id":"rise","name":"Rise","kind":"in","default_duration":0.5,"applies_to":["text","sticker"],"curve":{"tracks":{"dy":[{"t":0,"value":0.1,"ease":"easeOut"},{"t":1,"value":0}],"opacity":[{"t":0,"value":0},{"t":0.5,"value":1}]}}}]}"#;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn pack(manifest: &str) -> Vec<u8> {
        zip_of(&[("manifest.json", manifest.as_bytes()), ("LICENSE.txt", b"license")])
    }

    fn written(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn installed_folder(root: &Path) -> PathBuf {
        root.join("packages/clip-animation/avs-moves")
    }

    #[test]
    fn a_pack_goes_through_install_zip_to_its_folder_under_a_lower_cased_id() {
        let dir = scratch("ok");
        let root = dir.join("root");
        let outcome = install_zip(&written(&dir, &pack(MANIFEST_JSON)), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::ClipAnimationInstalled(InstalledClipAnimationPack { id: "avs-moves".to_owned(), name: "Moves".to_owned(), replaced: false }));
        let manifest = parse_clip_animation_manifest(&fs::read(installed_folder(&root).join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.items[0].group(), Some(AnimationGroup::Enter));
        assert!(!root.join("packages/.install-tmp").exists(), "no working folder stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_older_type_name_installs_to_the_same_folder() {
        let dir = scratch("alias");
        let root = dir.join("root");
        let manifest = MANIFEST_JSON.replace(r#""type":"clip-animation""#, r#""type":"text-animation""#);
        assert!(matches!(install_zip(&written(&dir, &pack(&manifest)), &root, false), Ok(ImportOutcome::ClipAnimationInstalled(_))));
        assert!(installed_folder(&root).join("manifest.json").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_that_is_not_exactly_the_format_is_refused_and_installs_nothing() {
        let full = MANIFEST_JSON;
        let item = r#"{"id":"rise","name":"Rise","kind":"in","default_duration":0.5,"applies_to":["text","sticker"],"curve":{"tracks":{"dy":[{"t":0,"value":0.1,"ease":"easeOut"},{"t":1,"value":0}],"opacity":[{"t":0,"value":0},{"t":0.5,"value":1}]}}}"#;
        for (name, text, wanted) in [
            ("an unknown field", full.replacen(r#""id":"Avs-Moves""#, r#""extra":1,"id":"Avs-Moves""#, 1), "BadManifest"),
            ("an unknown item field", full.replace(r#""kind":"in""#, r#""kind":"in","speed":2"#), "BadManifest"),
            ("an unknown kind", full.replace(r#""kind":"in""#, r#""kind":"middle""#), "BadManifest"),
            ("a zero duration", full.replace(r#""default_duration":0.5"#, r#""default_duration":0"#), "BadManifest"),
            ("a long duration", full.replace(r#""default_duration":0.5"#, r#""default_duration":600"#), "BadManifest"),
            ("an unknown clip kind", full.replace(r#"["text","sticker"]"#, r#"["audio"]"#), "BadManifest"),
            ("an empty applies_to", full.replace(r#"["text","sticker"]"#, "[]"), "BadManifest"),
            ("a repeated clip kind", full.replace(r#"["text","sticker"]"#, r#"["text","text"]"#), "BadManifest"),
            ("a t above 1", full.replace(r#"{"t":1,"value":0}"#, r#"{"t":1.5,"value":0}"#), "BadManifest"),
            ("keys out of order", full.replace(r#"{"t":0,"value":0},{"t":0.5,"value":1}"#, r#"{"t":0.5,"value":1},{"t":0,"value":0}"#), "BadManifest"),
            ("an empty track", full.replace(r#""opacity":[{"t":0,"value":0},{"t":0.5,"value":1}]"#, r#""opacity":[]"#), "BadManifest"),
            ("no track", full.replace(r#""tracks":{"dy""#, r#""tracks":{},"x":{"dy""#), "BadManifest"),
            ("an unknown property", full.replace(r#""opacity":"#, r#""hue":"#), "BadManifest"),
            ("an unsafe item id", full.replace(r#""id":"rise""#, r#""id":"a/b""#), "BadManifest"),
            ("a duplicate item id", full.replace(item, &format!("{item},{}", item.replace(r#""id":"rise""#, r#""id":"RISE""#))), "BadManifest"),
            ("another format", full.replacen(r#""format":1"#, r#""format":2"#, 1), "BadManifest"),
            ("no items", full.split(r#""items""#).next().unwrap().to_owned() + r#""items":[]}"#, "BadManifest"),
            ("a licence file not in the zip", full.replace(r#""LICENSE.txt""#, r#""GONE.txt""#), "BadManifest"),
            ("an unsafe id", full.replace("Avs-Moves", "../x"), "BadId"),
        ] {
            let dir = scratch("manifest");
            let error = install_zip(&written(&dir, &pack(&text)), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(!dir.join("root/packages/clip-animation").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_effect_is_kept_but_a_media_file_is_refused() {
        let with_effect = MANIFEST_JSON.replace(r#""kind":"in""#, r#""kind":"in","effect":{"package":"x.y"}"#);
        let manifest = parse_clip_animation_manifest(with_effect.as_bytes()).unwrap();
        check_clip_animation_fields(&manifest).unwrap();
        assert!(manifest.items[0].effect.is_some());

        let dir = scratch("entries");
        let root = dir.join("root");
        let image = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("LICENSE.txt", b"l"), ("smile.png", b"p")]);
        assert_eq!(install_zip(&written(&dir, &image), &root, false), Err(ImportProblem::Entry { entry: "smile.png".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        assert!(!root.join("packages/clip-animation").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_pack_is_replaced_only_when_allowed() {
        let dir = scratch("overwrite");
        let root = dir.join("root");
        install_zip(&written(&dir, &pack(MANIFEST_JSON)), &root, false).unwrap();
        let newer = written(&dir, &pack(&MANIFEST_JSON.replace(r#""name":"Moves""#, r#""name":"New Name""#)));
        assert_eq!(install_zip(&newer, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "Moves".to_owned() }));
        assert!(matches!(install_zip(&newer, &root, true), Ok(ImportOutcome::ClipAnimationInstalled(InstalledClipAnimationPack { replaced: true, .. }))));
        let manifest = parse_clip_animation_manifest(&fs::read(installed_folder(&root).join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.name, "New Name");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_install_cut_off_while_replacing_is_put_back() {
        let dir = scratch("interrupted");
        let root = dir.join("root");
        install_zip(&written(&dir, &pack(MANIFEST_JSON)), &root, false).unwrap();
        // What a replace leaves when it stops after moving the old pack aside.
        let backup = root.join("packages/.install-old/clip-animation");
        fs::create_dir_all(&backup).unwrap();
        fs::rename(installed_folder(&root), backup.join("avs-moves")).unwrap();
        recover_interrupted_install_with(&root, &mut |from, to| fs::rename(from, to));
        assert!(installed_folder(&root).join("manifest.json").is_file(), "the old pack is back");
        assert!(!root.join("packages/.install-old").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
