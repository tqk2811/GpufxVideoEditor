//! Installing a font pack from a `.zip`. No window here.
//!
//! A zip with `"type": "font"` is unpacked once to `<user root>/packages/font/<id lower-cased>/`
//! through the same path as an audio pack, with the same checks on its entries and on what is really
//! unpacked; only the files allowed, the manifest and the licences accepted differ. A text clip names
//! one of its fonts as `FontRef::Pack { pack, file }`.
//!
//! Only fonts under an open licence may be packed (see [`OPEN_FONT_LICENSES`]), since a pack is
//! shared and its fonts end up in exported videos.
//!
//! A shared module for every asset pack type was considered; the user chose one module per pack type.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

use d3d12_backend::{ASSET_FONT_EXTENSIONS, FONT_ASSET_TYPE};
use serde::Deserialize;
use zip::ZipArchive;

use crate::asset_install::{list_pack_folders, ListedPack, PackSummary, check_entry_extensions, check_item_ids, check_named_files, check_pack_header, clean_leftovers, extension, lock_installs, rename_with_retries, unpack_into_place, AudioLicense, ExtractLimits, Unpacked, MANIFEST, MAX_COMPRESSION_RATIO};
use crate::package_import::{entries_of, read_manifest, validate_entries, ImportOutcome, ImportProblem, Limits, MIB};

/// What the `type` of a font pack says, and the folder it goes to.
pub const FONT_TYPE: &str = FONT_ASSET_TYPE;
/// What the zip of a font pack may hold. A font file is never larger than the rasterizer reads.
pub const FONT_LIMITS: Limits = Limits { files: 4096, total_bytes: 1024 * MIB, file_bytes: gpufx_text::MAX_FONT_FILE_BYTES };
const FONT_EXTRACT: ExtractLimits = ExtractLimits { limits: FONT_LIMITS, max_ratio: MAX_COMPRESSION_RATIO };
/// The SPDX ids a font pack may carry, compared ignoring case: open font licences only.
pub const OPEN_FONT_LICENSES: [&str; 9] = ["OFL-1.0", "OFL-1.1", "OFL-1.1-RFN", "OFL-1.1-no-RFN", "Apache-2.0", "MIT", "CC0-1.0", "Ubuntu-font-1.0", "BSD-3-Clause"];

/// A font pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledFontPack {
    pub id: String,
    pub name: String,
    pub replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: AudioLicense,
    pub items: Vec<FontManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontManifestItem {
    pub id: String,
    /// The font file, a path with forward slashes inside the pack.
    pub file: String,
    /// The name the font list shows, such as the family and style.
    pub name: String,
}

pub(crate) fn is_font_file(name: &str) -> bool {
    ASSET_FONT_EXTENSIONS.contains(&extension(name).as_str())
}

/// Parses a manifest. The fields are exactly those of the format, all required.
pub fn parse_font_manifest(bytes: &[u8]) -> Result<FontManifest, ImportProblem> {
    serde_json::from_slice(bytes).map_err(|e| ImportProblem::BadManifest(e.to_string()))
}

/// Checks what a manifest says, apart from the files.
pub fn check_font_fields(manifest: &FontManifest) -> Result<(), ImportProblem> {
    if manifest.kind != FONT_TYPE {
        return Err(ImportProblem::UnknownType(manifest.kind.clone()));
    }
    check_pack_header(manifest.format, &manifest.id, &manifest.name, &manifest.version, &manifest.license, manifest.items.len(), "fonts")?;
    let spdx = manifest.license.spdx.trim();
    if !OPEN_FONT_LICENSES.iter().any(|open| open.eq_ignore_ascii_case(spdx)) {
        return Err(ImportProblem::BadManifest(format!("the licence \"{spdx}\" is not an open font licence ({})", OPEN_FONT_LICENSES.join(", "))));
    }
    check_item_ids(manifest.items.iter().map(|item| (item.id.as_str(), item.name.as_str())))?;
    // A file listed twice would show as two fonts that draw the same.
    let mut files = HashSet::new();
    for item in &manifest.items {
        if !files.insert(item.file.to_lowercase()) {
            return Err(ImportProblem::BadManifest(format!("the file \"{}\" is listed twice", item.file)));
        }
    }
    Ok(())
}

/// The font packs installed under `root`, as the package manager lists them.
pub fn list_installed(root: &Path) -> Vec<ListedPack> {
    list_pack_folders(root, FONT_TYPE, |bytes| {
        let manifest = parse_font_manifest(bytes)?;
        check_font_fields(&manifest)?;
        Ok(PackSummary { id: manifest.id, name: manifest.name, version: manifest.version, items: manifest.items.len() })
    })
}

/// Checks the zip at `source` and installs it under `user_root`. An existing pack of the same id is
/// replaced only when `overwrite` allows it.
pub fn install_font_pack(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    // Held for the whole install, as for an audio pack: the working folders are shared.
    let _lock = lock_installs(&packages).map_err(|e| ImportProblem::WriteFailed(format!("another install is running ({e})")))?;
    clean_leftovers(&packages);

    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, FONT_LIMITS)?;
    validate_entries(&entries, FONT_LIMITS)?;
    check_entry_extensions(&entries, is_font_file)?;
    let manifest = parse_font_manifest(&read_manifest(&mut zip)?)?;
    check_font_fields(&manifest)?;
    check_named_files(&manifest.license.files, manifest.items.iter().map(|item| item.file.as_str()), &entries, is_font_file, "a font file")?;

    let id = manifest.id.to_ascii_lowercase();
    let check_unpacked = |temporary: &Path| {
        let unpacked = parse_font_manifest(&fs::read(temporary.join(MANIFEST)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?)?;
        if unpacked == manifest {
            Ok(())
        } else {
            Err(ImportProblem::BadManifest("it changed while the zip was being read".to_owned()))
        }
    };
    let unpacked = unpack_into_place(&mut zip, &entries, &packages, FONT_TYPE, &id, &manifest.name, overwrite, FONT_EXTRACT, &mut rename_with_retries, check_unpacked)?;
    Ok(match unpacked {
        Unpacked::AlreadyThere { name } => ImportOutcome::AlreadyInstalled { name },
        Unpacked::Done { replaced } => ImportOutcome::FontInstalled(InstalledFontPack { id, name: manifest.name, replaced }),
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
        let dir = std::env::temp_dir().join(format!("gpufx-font-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MANIFEST_JSON: &str = r#"{"format":1,"type":"font","id":"Open-Fonts","name":"Open Fonts","version":"1.0.0","license":{"spdx":"OFL-1.1","files":["OFL.txt"]},"items":[{"id":"sans","file":"fonts/Sans.ttf","name":"Sans Regular"}]}"#;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn pack(manifest: &str, font: &[u8]) -> Vec<u8> {
        zip_of(&[("manifest.json", manifest.as_bytes()), ("OFL.txt", b"license"), ("fonts/Sans.ttf", font)])
    }

    fn written(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn installed_folder(root: &Path) -> PathBuf {
        root.join("packages/font/open-fonts")
    }

    #[test]
    fn a_pack_goes_through_install_zip_to_its_folder_under_a_lower_cased_id() {
        let dir = scratch("ok");
        let root = dir.join("root");
        let outcome = install_zip(&written(&dir, &pack(MANIFEST_JSON, b"ttf")), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::FontInstalled(InstalledFontPack { id: "open-fonts".to_owned(), name: "Open Fonts".to_owned(), replaced: false }));
        assert_eq!(fs::read(installed_folder(&root).join("fonts/Sans.ttf")).unwrap(), b"ttf");
        assert!(installed_folder(&root).join("manifest.json").is_file());
        assert!(!root.join("packages/.install-tmp").exists(), "no working folder stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_that_is_not_exactly_the_format_or_not_an_open_licence_is_refused() {
        let full = MANIFEST_JSON;
        for (name, text, wanted) in [
            ("an unknown field", full.replacen(r#""id":"Open-Fonts""#, r#""extra":1,"id":"Open-Fonts""#, 1), "BadManifest"),
            ("a sticker field", full.replace(r#","name":"Sans Regular""#, r#","name":"Sans Regular","tags":[]"#), "BadManifest"),
            ("another format", full.replacen(r#""format":1"#, r#""format":2"#, 1), "BadManifest"),
            ("no items", full.split(r#""items""#).next().unwrap().to_owned() + r#""items":[]}"#, "BadManifest"),
            ("a closed licence", full.replace("OFL-1.1", "LicenseRef-Proprietary"), "BadManifest"),
            ("a file not in the zip", full.replace("fonts/Sans.ttf", "fonts/Gone.ttf"), "BadManifest"),
            ("a licence file not in the zip", full.replace(r#""OFL.txt""#, r#""GONE.txt""#), "BadManifest"),
            ("a non-font item", full.replace("fonts/Sans.ttf", "OFL.txt"), "BadManifest"),
            ("a traversal in file", full.replace("fonts/Sans.ttf", "../Sans.ttf"), "Entry"),
            ("an unsafe id", full.replace("Open-Fonts", "../x"), "BadId"),
        ] {
            let dir = scratch("manifest");
            let error = install_zip(&written(&dir, &pack(&text, b"ttf")), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(!dir.join("root/packages/font").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_open_licence_is_taken_in_any_case() {
        let manifest = parse_font_manifest(MANIFEST_JSON.replace("OFL-1.1", "ofl-1.1").as_bytes()).unwrap();
        assert_eq!(check_font_fields(&manifest), Ok(()));
    }

    #[test]
    fn a_file_of_another_kind_is_refused_and_an_existing_pack_is_kept_unless_overwritten() {
        let dir = scratch("rest");
        let root = dir.join("root");
        let wrong_kind = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("OFL.txt", b"l"), ("fonts/Sans.ttf", b"f"), ("run.exe", b"MZ")]);
        assert_eq!(install_zip(&written(&dir, &wrong_kind), &root, false), Err(ImportProblem::Entry { entry: "run.exe".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        let picture = zip_of(&[("manifest.json", MANIFEST_JSON.as_bytes()), ("OFL.txt", b"l"), ("fonts/Sans.ttf", b"f"), ("a.png", b"p")]);
        assert_eq!(install_zip(&written(&dir, &picture), &root, false), Err(ImportProblem::Entry { entry: "a.png".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        assert!(!root.join("packages/font").exists());

        install_zip(&written(&dir, &pack(MANIFEST_JSON, b"old")), &root, false).unwrap();
        let newer = written(&dir, &pack(MANIFEST_JSON, b"new"));
        assert_eq!(install_zip(&newer, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "Open Fonts".to_owned() }));
        assert_eq!(fs::read(installed_folder(&root).join("fonts/Sans.ttf")).unwrap(), b"old");
        assert!(matches!(install_zip(&newer, &root, true), Ok(ImportOutcome::FontInstalled(InstalledFontPack { replaced: true, .. }))));
        assert_eq!(fs::read(installed_folder(&root).join("fonts/Sans.ttf")).unwrap(), b"new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_items_cannot_share_a_file() {
        let doubled = MANIFEST_JSON.replace("}]}", r#"},{"id":"again","file":"FONTS/Sans.ttf","name":"Again"}]}"#);
        let manifest = parse_font_manifest(doubled.as_bytes()).unwrap();
        assert!(matches!(check_font_fields(&manifest), Err(ImportProblem::BadManifest(message)) if message.contains("twice")));
    }
}
