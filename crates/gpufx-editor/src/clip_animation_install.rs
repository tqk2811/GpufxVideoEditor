//! Installing a clip animation pack from a `.zip`. No window here.
//!
//! A zip with `"type": "clip-animation"` is unpacked once to
//! `<user root>/packages/clip-animation/<id lower-cased>/` through the same path as a sticker pack.
//! It holds no media: only the manifest, the file of each item under `items/` (see
//! [`d3d12_backend::clip_animation_pack`] for the layout) and its licence files.
//! A clip names an item as `"<pack id>/<item id>"` (see [`crate::clip_animation_library`]).
//!
//! A shared module for every pack type was considered; the user chose one install module per pack
//! type, which share only the helpers of [`crate::asset_install`].

use std::fs::{self, File};
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use d3d12_backend as shared;
pub use d3d12_backend::{is_clip_animation_type, ClipAnimationManifest, ClipAnimationManifestFile, ClipAnimationManifestItem, CLIP_KINDS, MAX_DEFAULT_DURATION};
use d3d12_backend::{ManifestProblem, CLIP_ANIMATION_FOLDER, CLIP_ANIMATION_ITEMS_FOLDER};
use zip::ZipArchive;

use crate::asset_install::{list_pack_folders, ListedPack, PackSummary, check_entry_extensions, check_named_files, clean_leftovers, lock_installs, rename_with_retries, unpack_into_place, ExtractLimits, Unpacked, MAX_COMPRESSION_RATIO};
use crate::package_import::{entries_of, read_manifest, validate_entries, EntryKind, ImportOutcome, ImportProblem, Limits, ZipEntry, ARCHIVE_LIMITS, MIB};

/// What the `type` of a clip animation pack says, and the folder it goes to.
pub const CLIP_ANIMATION_TYPE: &str = CLIP_ANIMATION_FOLDER;
/// A pack is a manifest, one small file per item and a few licence files.
pub const CLIP_ANIMATION_LIMITS: Limits = Limits { files: ARCHIVE_LIMITS.files, total_bytes: 64 * MIB, file_bytes: 8 * MIB };
const CLIP_ANIMATION_EXTRACT: ExtractLimits = ExtractLimits { limits: CLIP_ANIMATION_LIMITS, max_ratio: MAX_COMPRESSION_RATIO };

/// A clip animation pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledClipAnimationPack {
    pub id: String,
    pub name: String,
    pub replaced: bool,
}

/// Parses `manifest.json`. The fields are exactly those of the format.
pub fn parse_clip_animation_manifest(bytes: &[u8]) -> Result<ClipAnimationManifestFile, ImportProblem> {
    shared::parse_clip_animation_manifest(bytes).map_err(import_problem)
}

/// Checks what a manifest says, apart from the files. It is the check the reader of installed packs
/// makes (see [`d3d12_backend::scan_clip_animation_folders`]), so a pack that installs reads.
pub fn check_clip_animation_fields(manifest: &ClipAnimationManifest) -> Result<(), ImportProblem> {
    shared::check_clip_animation_fields(manifest).map_err(import_problem)
}

fn import_problem(problem: ManifestProblem) -> ImportProblem {
    match problem {
        ManifestProblem::UnknownType(kind) => ImportProblem::UnknownType(kind),
        ManifestProblem::BadId(id) => ImportProblem::BadId(id),
        ManifestProblem::BadManifest(message) => ImportProblem::BadManifest(message),
    }
}

/// A pack holds no media file, only the files of its items besides the manifest and documents.
fn is_item_file(name: &str) -> bool {
    name.strip_prefix(CLIP_ANIMATION_ITEMS_FOLDER).and_then(|rest| rest.strip_prefix('/')).is_some_and(|file| file.to_ascii_lowercase().ends_with(".json"))
}

/// The files of the zip under `items/`: the name relative to that folder, and the entry's own name.
fn item_entries(entries: &[ZipEntry]) -> Vec<(String, String)> {
    let prefix = format!("{CLIP_ANIMATION_ITEMS_FOLDER}/");
    entries.iter().filter(|entry| entry.kind == EntryKind::File).filter_map(|entry| entry.name.replace('\\', "/").strip_prefix(&prefix).map(|file| (file.to_owned(), entry.name.clone()))).collect()
}

/// The bytes of the entry `name`, at most `limit` of them.
fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut entry = zip.by_name(name).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    entry.by_ref().take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("it is too large".to_owned());
    }
    Ok(bytes)
}

/// The clip animation packs installed under `root`, as the package manager lists them. A pack whose
/// item files do not fit its manifest is listed with the problem, as the reader refuses it.
pub fn list_installed(root: &Path) -> Vec<ListedPack> {
    let mut listed = list_pack_folders(root, CLIP_ANIMATION_TYPE, |bytes| {
        let manifest = parse_clip_animation_manifest(bytes)?;
        shared::check_clip_animation_manifest_file(&manifest).map_err(import_problem)?;
        Ok(PackSummary { id: manifest.id, name: manifest.name, version: manifest.version, items: manifest.items.len() })
    });
    for pack in &mut listed {
        if pack.read.is_ok() {
            if let Err(message) = shared::read_clip_animation_pack(&pack.folder) {
                pack.read = Err(message);
            }
        }
    }
    listed
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
    check_entry_extensions(&entries, is_item_file)?;
    let file = parse_clip_animation_manifest(&read_manifest(&mut zip)?)?;
    let items = item_entries(&entries);
    let names: Vec<String> = items.iter().map(|(file, _)| file.clone()).collect();
    let manifest = shared::assemble_clip_animation_pack(file, &names, |file| {
        let entry = items.iter().find(|(name, _)| name == file).map(|(_, entry)| entry.as_str()).unwrap_or(file);
        read_entry(&mut zip, entry, CLIP_ANIMATION_LIMITS.file_bytes)
    })
    .map_err(import_problem)?;
    check_named_files(&manifest.license.files, std::iter::empty(), &entries, is_item_file, "an item")?;

    let id = manifest.id.to_ascii_lowercase();
    let check_unpacked = |temporary: &Path| {
        let unpacked = shared::read_clip_animation_pack(temporary).map_err(ImportProblem::BadManifest)?;
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
    use engine::AnimationGroup;
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

    /// The zip of a pack written with its items inside the manifest, split into the files of the
    /// format, with `extra` files added.
    fn pack_with(manifest: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let files = d3d12_backend::split_clip_animation_pack(manifest);
        let mut all: Vec<(&str, &[u8])> = files.iter().map(|(name, text)| (name.as_str(), text.as_bytes())).collect();
        all.push(("LICENSE.txt", b"license"));
        all.extend_from_slice(extra);
        zip_of(&all)
    }

    fn pack(manifest: &str) -> Vec<u8> {
        pack_with(manifest, &[])
    }

    fn installed(root: &Path) -> ClipAnimationManifest {
        d3d12_backend::read_clip_animation_pack(&installed_folder(root)).unwrap()
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
        let manifest = installed(&root);
        assert_eq!(manifest.items[0].group(), Some(AnimationGroup::Enter));
        assert!(installed_folder(&root).join("items/rise.json").is_file());
        assert!(!root.join("packages/.install-tmp").exists(), "no working folder stays");
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
            // items/rise.json and items/RISE.json are one name to the zip check, which refuses it first.
            ("a duplicate item id", full.replace(item, &format!("{item},{}", item.replace(r#""id":"rise""#, r#""id":"RISE""#))), "Entry"),
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
        let dir = scratch("entries");
        let root = dir.join("root");
        let with_effect = MANIFEST_JSON.replace(r#""kind":"in""#, r#""kind":"in","effect":{"package":"x.y"}"#);
        install_zip(&written(&dir, &pack(&with_effect)), &root, false).unwrap();
        let manifest = installed(&root);
        check_clip_animation_fields(&manifest).unwrap();
        assert!(manifest.items[0].effect.is_some());
        let _ = fs::remove_dir_all(&root);

        let image = pack_with(MANIFEST_JSON, &[("smile.png", b"p")]);
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
        assert_eq!(installed(&root).name, "New Name");
        let _ = fs::remove_dir_all(&dir);
    }

    const ITEM_A: &str = r#"{"id":"a","name":"A","kind":"in","default_duration":0.5,"curve":{"tracks":{"opacity":[{"t":0,"value":0},{"t":1,"value":1,"ease":"bounce"}]}}}"#;

    fn head(items: &str) -> String {
        format!(r#"{{"format":1,"type":"clip-animation","id":"Avs-Moves","name":"Moves","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{items}]}}"#)
    }

    fn item(id: &str) -> String {
        ITEM_A.replace(r#""id":"a""#, &format!(r#""id":"{id}""#))
    }

    #[test]
    fn a_pack_of_item_files_keeps_the_order_of_its_manifest() {
        let dir = scratch("order");
        let root = dir.join("root");
        // The zip holds the files in another order than the manifest lists them.
        let zip = zip_of(&[
            ("items/c.json", item("c").as_bytes()),
            ("manifest.json", head(r#""b","c","a""#).as_bytes()),
            ("items/a.json", item("a").as_bytes()),
            ("LICENSE.txt", b"l"),
            ("items/b.json", item("b").replace("\"bounce\"", r#"{"steps":{"count":3,"position":"start"}}"#).as_bytes()),
        ]);
        install_zip(&written(&dir, &zip), &root, false).unwrap();
        let manifest = installed(&root);
        assert_eq!(manifest.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), vec!["b", "c", "a"]);
        assert_eq!(list_installed(&root)[0].read.as_ref().map(|summary| summary.items), Ok(3));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn item_files_that_do_not_fit_the_manifest_are_refused() {
        let cases: [(&str, Vec<(String, String)>, &str); 6] = [
            ("a missing item file", vec![("manifest.json".into(), head(r#""a","b""#)), ("items/a.json".into(), item("a"))], "no file"),
            ("an id that is not the file name", vec![("manifest.json".into(), head(r#""a","b""#)), ("items/a.json".into(), item("a")), ("items/b.json".into(), item("c"))], "not \"b\""),
            ("a file the manifest does not list", vec![("manifest.json".into(), head(r#""a""#)), ("items/a.json".into(), item("a")), ("items/extra.json".into(), item("extra"))], "not an item the manifest lists"),
            ("an id listed twice", vec![("manifest.json".into(), head(r#""a","A""#)), ("items/a.json".into(), item("a"))], "used twice"),
            ("an item list of objects", vec![("manifest.json".into(), head(ITEM_A)), ("items/a.json".into(), item("a"))], "invalid type"),
            ("an ease with bad numbers", vec![("manifest.json".into(), head(r#""a""#)), ("items/a.json".into(), ITEM_A.replace("\"bounce\"", r#"{"bounce":{"bounces":0}}"#))], "bad numbers"),
        ];
        for (name, files, wanted) in cases {
            let dir = scratch("layout");
            let mut all: Vec<(&str, &[u8])> = files.iter().map(|(name, text)| (name.as_str(), text.as_bytes())).collect();
            all.push(("LICENSE.txt", b"l"));
            let error = install_zip(&written(&dir, &zip_of(&all)), &dir.join("root"), false).unwrap_err();
            assert!(matches!(&error, ImportProblem::BadManifest(message) if message.contains(wanted)), "{name}: {error:?}");
            assert!(!dir.join("root/packages/clip-animation").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
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
