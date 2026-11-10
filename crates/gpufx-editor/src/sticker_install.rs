//! Installing a sticker pack from a `.zip`. No window here.
//!
//! A zip with `"type": "sticker"` is unpacked once to `<user root>/packages/sticker/<id lower-cased>/`
//! through the same path as an audio pack, with the same checks on its entries and on what is really
//! unpacked; only the files allowed and the manifest differ. Its files are named by a clip as
//! `asset:sticker/<id>/<file>`.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

use d3d12_backend::ASSET_IMAGE_EXTENSIONS;
use serde::Deserialize;
use zip::ZipArchive;

use crate::asset_install::{check_entry_extensions, check_item_ids, check_named_files, check_pack_header, clean_leftovers, extension, lock_installs, rename_with_retries, unpack_into_place, AudioLicense, ExtractLimits, Unpacked, MANIFEST, MAX_COMPRESSION_RATIO};
use crate::package_import::{entries_of, read_manifest, validate_entries, ImportOutcome, ImportProblem, Limits, MIB};

/// What the `type` of a sticker pack says.
pub const STICKER_TYPE: &str = "sticker";
/// What the zip of a sticker pack may hold.
pub const STICKER_LIMITS: Limits = Limits { files: 16_384, total_bytes: 1024 * MIB, file_bytes: 64 * MIB };
const STICKER_EXTRACT: ExtractLimits = ExtractLimits { limits: STICKER_LIMITS, max_ratio: MAX_COMPRESSION_RATIO };

/// A sticker pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledStickerPack {
    pub id: String,
    pub name: String,
    pub replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StickerManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: AudioLicense,
    pub items: Vec<StickerManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StickerManifestItem {
    pub id: String,
    pub file: String,
    pub name: String,
    pub tags: Vec<String>,
    pub category: String,
}

fn is_image_file(name: &str) -> bool {
    ASSET_IMAGE_EXTENSIONS.contains(&extension(name).as_str())
}

/// Parses a manifest. The fields are exactly those of the format, all required.
pub fn parse_sticker_manifest(bytes: &[u8]) -> Result<StickerManifest, ImportProblem> {
    serde_json::from_slice(bytes).map_err(|e| ImportProblem::BadManifest(e.to_string()))
}

/// Checks what a manifest says, apart from the files.
pub fn check_sticker_fields(manifest: &StickerManifest) -> Result<(), ImportProblem> {
    if manifest.kind != STICKER_TYPE {
        return Err(ImportProblem::UnknownType(manifest.kind.clone()));
    }
    check_pack_header(manifest.format, &manifest.id, &manifest.name, &manifest.version, &manifest.license, manifest.items.len(), "stickers")?;
    check_item_ids(manifest.items.iter().map(|item| (item.id.as_str(), item.name.as_str())))?;
    // A file listed twice would look like two stickers but only one clip source.
    let mut files = HashSet::new();
    for item in &manifest.items {
        if !files.insert(item.file.to_lowercase()) {
            return Err(ImportProblem::BadManifest(format!("the file \"{}\" is listed twice", item.file)));
        }
    }
    Ok(())
}

/// Checks the zip at `source` and installs it under `user_root`. An existing pack of the same id is
/// replaced only when `overwrite` allows it.
pub fn install_sticker_pack(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    // Held for the whole install, as for an audio pack: the working folders are shared.
    let _lock = lock_installs(&packages).map_err(|e| ImportProblem::WriteFailed(format!("another install is running ({e})")))?;
    clean_leftovers(&packages);

    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, STICKER_LIMITS)?;
    validate_entries(&entries, STICKER_LIMITS)?;
    check_entry_extensions(&entries, is_image_file)?;
    let manifest = parse_sticker_manifest(&read_manifest(&mut zip)?)?;
    check_sticker_fields(&manifest)?;
    check_named_files(&manifest.license.files, manifest.items.iter().map(|item| item.file.as_str()), &entries, is_image_file, "an image")?;

    let id = manifest.id.to_ascii_lowercase();
    let check_unpacked = |temporary: &Path| {
        let unpacked = parse_sticker_manifest(&fs::read(temporary.join(MANIFEST)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?)?;
        if unpacked == manifest {
            Ok(())
        } else {
            Err(ImportProblem::BadManifest("it changed while the zip was being read".to_owned()))
        }
    };
    let unpacked = unpack_into_place(&mut zip, &entries, &packages, STICKER_TYPE, &id, &manifest.name, overwrite, STICKER_EXTRACT, &mut rename_with_retries, check_unpacked)?;
    Ok(match unpacked {
        Unpacked::AlreadyThere { name } => ImportOutcome::AlreadyInstalled { name },
        Unpacked::Done { replaced } => ImportOutcome::StickerInstalled(InstalledStickerPack { id, name: manifest.name, replaced }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_import::{install_zip, EntryProblem};
    use std::io::{Cursor, Write};
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-sticker-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MANIFEST_JSON: &str = r#"{"format":1,"type":"sticker","id":"Avs-Emoji","name":"Emoji","version":"1.0.0","license":{"spdx":"MIT","files":["LICENSE.txt"]},"items":[{"id":"smile","file":"images/smile.png","name":"Smile","tags":["face"],"category":"people"}]}"#;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn pack(manifest: &str, image: &[u8]) -> Vec<u8> {
        zip_of(&[("manifest.json", manifest.as_bytes()), ("LICENSE.txt", b"license"), ("images/smile.png", image)])
    }

    fn written(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn installed_folder(root: &Path) -> PathBuf {
        root.join("packages/sticker/avs-emoji")
    }

    #[test]
    fn a_pack_goes_through_install_zip_to_its_folder_under_a_lower_cased_id() {
        let dir = scratch("ok");
        let root = dir.join("root");
        let outcome = install_zip(&written(&dir, &pack(MANIFEST_JSON, b"png")), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::StickerInstalled(InstalledStickerPack { id: "avs-emoji".to_owned(), name: "Emoji".to_owned(), replaced: false }));
        assert_eq!(fs::read(installed_folder(&root).join("images/smile.png")).unwrap(), b"png");
        assert!(installed_folder(&root).join("manifest.json").is_file());
        assert!(!root.join("packages/.install-tmp").exists(), "no working folder stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_that_is_not_exactly_the_format_is_refused_and_installs_nothing() {
        let full = MANIFEST_JSON;
        for (name, text, wanted) in [
            ("an unknown field", full.replacen(r#""id":"Avs-Emoji""#, r#""extra":1,"id":"Avs-Emoji""#, 1), "BadManifest"),
            ("a missing tag list", full.replace(r#","tags":["face"]"#, ""), "BadManifest"),
            ("an audio field", full.replace(r#","category":"people""#, r#","category":"people","duration":1.0"#), "BadManifest"),
            ("another format", full.replacen(r#""format":1"#, r#""format":2"#, 1), "BadManifest"),
            ("no items", full.split(r#""items""#).next().unwrap().to_owned() + r#""items":[]}"#, "BadManifest"),
            ("a file not in the zip", full.replace("images/smile.png", "images/gone.png"), "BadManifest"),
            ("a licence file not in the zip", full.replace(r#""LICENSE.txt""#, r#""GONE.txt""#), "BadManifest"),
            ("a non-image item", full.replace("images/smile.png", "LICENSE.txt"), "BadManifest"),
            ("a traversal in file", full.replace("images/smile.png", "../smile.png"), "Entry"),
            ("a colon in file", full.replace("images/smile.png", "images/smile.png:x"), "Entry"),
            ("a backslash in file", full.replace("images/smile.png", r"images\\smile.png"), "BadManifest"),
            ("an unsafe id", full.replace("Avs-Emoji", "../x"), "BadId"),
        ] {
            let dir = scratch("manifest");
            let error = install_zip(&written(&dir, &pack(&text, b"png")), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(!dir.join("root/packages/sticker").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_escaping_entry_or_a_file_of_another_kind_is_refused() {
        let dir = scratch("entries");
        let root = dir.join("root");
        let escaping = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("LICENSE.txt", b"l"), ("images/smile.png", b"p"), ("../evil.png", b"p")]);
        assert!(matches!(install_zip(&written(&dir, &escaping), &root, false), Err(ImportProblem::Entry { problem: EntryProblem::UnsafePath, .. })));
        let wrong_kind = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("LICENSE.txt", b"l"), ("images/smile.png", b"p"), ("run.exe", b"MZ")]);
        assert_eq!(install_zip(&written(&dir, &wrong_kind), &root, false), Err(ImportProblem::Entry { entry: "run.exe".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        let sound = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("LICENSE.txt", b"l"), ("images/smile.png", b"p"), ("beep.flac", b"f")]);
        assert_eq!(install_zip(&written(&dir, &sound), &root, false), Err(ImportProblem::Entry { entry: "beep.flac".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        assert!(!root.join("packages/sticker").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_a_zip_bomb_and_an_existing_pack_are_handled_like_an_audio_pack() {
        let dir = scratch("rest");
        let root = dir.join("root");
        let missing = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("LICENSE.txt", b"l")]);
        assert!(matches!(install_zip(&written(&dir, &missing), &root, false), Err(ImportProblem::BadManifest(message)) if message.contains("not in the zip")));
        let bomb = pack(MANIFEST_JSON, &vec![0u8; 8 * MIB as usize]);
        assert_eq!(install_zip(&written(&dir, &bomb), &root, false), Err(ImportProblem::RatioTooHigh { entry: "images/smile.png".to_owned(), limit: MAX_COMPRESSION_RATIO }));
        assert!(!installed_folder(&root).exists());

        install_zip(&written(&dir, &pack(MANIFEST_JSON, b"old")), &root, false).unwrap();
        let newer = written(&dir, &pack(&MANIFEST_JSON.replace("\"Emoji\"", "\"New Name\""), b"new"));
        assert_eq!(install_zip(&newer, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "Emoji".to_owned() }), "the name already installed");
        assert_eq!(fs::read(installed_folder(&root).join("images/smile.png")).unwrap(), b"old");
        assert!(matches!(install_zip(&newer, &root, true), Ok(ImportOutcome::StickerInstalled(InstalledStickerPack { replaced: true, .. }))));
        assert_eq!(fs::read(installed_folder(&root).join("images/smile.png")).unwrap(), b"new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_items_cannot_share_a_file() {
        let doubled = MANIFEST_JSON.replace("}]}", r#"},{"id":"again","file":"IMAGES/smile.png","name":"Again","tags":[],"category":"x"}]}"#);
        let manifest = parse_sticker_manifest(doubled.as_bytes()).unwrap();
        assert!(matches!(check_sticker_fields(&manifest), Err(ImportProblem::BadManifest(message)) if message.contains("twice")));
    }
}
