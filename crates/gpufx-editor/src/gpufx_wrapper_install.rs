//! Installing the editor's wrapper around a gpufx effect package (see
//! [`d3d12_backend::gpufx_wrapper`]) from a `.zip` or from a folder. Both are checked the same way
//! and end up unpacked one level at `<user root>/packages/gpufx/user/<name>/`: `manifest.json`,
//! `i18n/<code>.json` and the inner `<name>.zip`, which stays a zip.
//!
//! A wrapper holds nothing else, its manifest is strict, and the id of the inner package must be the
//! one the wrapper names.

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use d3d12_backend::{is_wrapper_manifest, parse_wrapper_manifest, read_package_info, read_zip_package_info, WrapperManifest};
use serde_json::Value;
use zip::ZipArchive;

use crate::i18n::pack::language_file_code;
use crate::package_import::{entries_of, name_parts, read_manifest, validate_entries, EntryKind, ImportOutcome, ImportProblem, InstalledPackage, Limits, ZipEntry, MIB, SHADER_LIMITS, USER_GROUP};
use crate::packages::classify;

const MANIFEST: &str = "manifest.json";
/// A manifest, up to 64 language files with their folder, and the inner zip.
pub const WRAPPER_LIMITS: Limits = Limits { files: 80, total_bytes: 272 * MIB, file_bytes: 264 * MIB };
/// What a wrapper zip may weigh, headers included.
pub(crate) const MAX_WRAPPER_ARCHIVE_BYTES: u64 = 280 * MIB;

/// The files of a wrapper, by their name with forward slashes.
type Files = Vec<(String, Vec<u8>)>;

fn joined(name: &str) -> Result<String, ImportProblem> {
    name_parts(name).map(|parts| parts.join("/")).map_err(|problem| ImportProblem::Entry { entry: name.to_owned(), problem })
}

/// Checks `files` as a wrapper: its manifest, that nothing else is there, and the inner package.
fn check(files: &Files) -> Result<(WrapperManifest, d3d12_backend::PackageInfo, crate::packages::PackageKind), ImportProblem> {
    let file = |name: &str| files.iter().find(|(file, _)| file == name).map(|(_, bytes)| bytes);
    let manifest = parse_wrapper_manifest(file(MANIFEST).ok_or(ImportProblem::NoManifest)?).map_err(ImportProblem::BadManifest)?;
    for (name, _) in files {
        let allowed = name == MANIFEST || *name == manifest.package || language_file_code(name).is_some_and(|_| !name[5..].contains('/'));
        if !allowed {
            return Err(ImportProblem::WrapperStrayFile(name.clone()));
        }
    }
    let inner = file(&manifest.package).ok_or_else(|| ImportProblem::WrapperMissingPackage(manifest.package.clone()))?;
    let mut zip = ZipArchive::new(Cursor::new(inner.as_slice())).map_err(|e| ImportProblem::BadPackage(e.to_string()))?;
    validate_entries(&entries_of(&mut zip, SHADER_LIMITS)?, SHADER_LIMITS)?;
    let inner_manifest: Value = serde_json::from_slice(&read_manifest(&mut zip)?).map_err(|e| ImportProblem::BadPackage(e.to_string()))?;
    let inner_id = inner_manifest.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
    let info = read_zip_package_info(inner).map_err(ImportProblem::BadPackage)?;
    if !inner_id.eq_ignore_ascii_case(&manifest.id) {
        return Err(ImportProblem::WrapperIdMismatch { wrapper: manifest.id.clone(), inner: inner_id });
    }
    let Some(kind) = classify(&info) else { return Err(ImportProblem::NotUsable { inputs: info.inputs }) };
    Ok((manifest, info, kind))
}

/// Installs the wrapper zip whose bytes are `bytes`.
pub(crate) fn install_wrapper_zip(bytes: &[u8], user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, WRAPPER_LIMITS)?;
    validate_entries(&entries, WRAPPER_LIMITS)?;
    let mut files = Files::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.kind != EntryKind::File {
            continue;
        }
        let mut content = Vec::new();
        let reader = zip.by_index(index).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
        reader.take(entry.size + 1).read_to_end(&mut content).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
        if content.len() as u64 != entry.size {
            return Err(ImportProblem::SizeMismatch { entry: entry.name.clone() });
        }
        files.push((joined(&entry.name)?, content));
    }
    install_files(&files, user_root, overwrite)
}

/// Lists `folder` as entries of a zip would be, links included so they are refused.
fn walk(folder: &Path, prefix: &str, found: &mut Vec<(ZipEntry, PathBuf)>, limit: usize) -> Result<(), ImportProblem> {
    let unreadable = |e: std::io::Error| ImportProblem::Unreadable(e.to_string());
    let mut entries: Vec<_> = fs::read_dir(folder).map_err(unreadable)?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if found.len() > limit {
            return Err(ImportProblem::TooManyFiles { limit });
        }
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let meta = fs::symlink_metadata(entry.path()).map_err(unreadable)?;
        let kind = if meta.file_type().is_symlink() {
            EntryKind::Symlink
        } else if meta.is_dir() {
            EntryKind::Directory
        } else if meta.is_file() {
            EntryKind::File
        } else {
            EntryKind::Special
        };
        found.push((ZipEntry { name: if kind == EntryKind::Directory { format!("{name}/") } else { name.clone() }, size: if kind == EntryKind::File { meta.len() } else { 0 }, kind, encrypted: false }, entry.path()));
        if kind == EntryKind::Directory {
            walk(&entry.path(), &format!("{name}/"), found, limit)?;
        }
    }
    Ok(())
}

/// Checks the wrapper folder `source` and installs a copy of it under `user_root`.
pub fn install_wrapper_folder(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let manifest = fs::read(source.join(MANIFEST)).map_err(|_| ImportProblem::NoManifest)?;
    if !is_wrapper_manifest(&manifest) {
        return Err(ImportProblem::NotAWrapper);
    }
    let mut found = Vec::new();
    walk(source, "", &mut found, WRAPPER_LIMITS.files)?;
    let entries: Vec<ZipEntry> = found.iter().map(|(entry, _)| entry.clone()).collect();
    validate_entries(&entries, WRAPPER_LIMITS)?;
    let mut files = Files::new();
    for (entry, path) in found.into_iter().filter(|(entry, _)| entry.kind == EntryKind::File) {
        let file = fs::File::open(&path).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
        let mut content = Vec::new();
        file.take(WRAPPER_LIMITS.file_bytes + 1).read_to_end(&mut content).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
        if content.len() as u64 != entry.size {
            return Err(ImportProblem::SizeMismatch { entry: entry.name.clone() });
        }
        files.push((joined(&entry.name)?, content));
    }
    install_files(&files, user_root, overwrite)
}

fn write_all(folder: &Path, files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    fs::create_dir_all(folder)?;
    for (name, bytes) in files {
        let target = name.split('/').fold(folder.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
    }
    Ok(())
}

/// Writes the checked `files` to `<user root>/packages/gpufx/user/<name>`, through a folder no
/// scan reads, replacing what was there only once the copy is whole.
fn install_files(files: &Files, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let (manifest, info, kind) = check(files)?;
    let name = manifest.name().to_owned();
    let packages = user_root.join("packages");
    let target = packages.join("gpufx").join(USER_GROUP).join(&name);
    let replaced = target.exists();
    if replaced {
        // An installed package that cannot be read does not stop its replacement.
        let installed = read_package_info(&target).ok();
        if let Some(existing) = installed.as_ref().and_then(classify) {
            if existing != kind {
                return Err(ImportProblem::KindChanged { existing, new: kind });
            }
        }
        if !overwrite {
            return Ok(ImportOutcome::AlreadyInstalled { name: installed.map_or(info.name, |installed| installed.name) });
        }
    }
    let unique = crate::package_manager::next_unique();
    let staging = packages.join(format!(".importing-{unique}"));
    let old = packages.join(format!(".replaced-{unique}"));
    let failed = |e: std::io::Error| ImportProblem::WriteFailed(e.to_string());
    let result = (|| {
        write_all(&staging, files)?;
        fs::create_dir_all(target.parent().expect("under the user group"))?;
        if replaced {
            crate::asset_install::rename_with_retries(&target, &old)?;
        }
        if let Err(error) = crate::asset_install::rename_with_retries(&staging, &target) {
            if replaced {
                let _ = crate::asset_install::rename_with_retries(&old, &target);
            }
            return Err(error);
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(&old);
    result.map_err(failed)?;
    Ok(ImportOutcome::Installed(InstalledPackage { package: format!("packages/gpufx/{USER_GROUP}/{name}"), name: info.name, kind, replaced }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in files {
            writer.start_file(*name, SimpleFileOptions::default()).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn inner(id: &str) -> Vec<u8> {
        let manifest = format!(r#"{{"id":"{id}","name":"Iris","used_for":["gpufx"],"groups":[{{"passes":[{{"shader":"a.wgsl"}}]}}]}}"#);
        zip_bytes(&[("manifest.json", manifest.as_bytes()), ("a.wgsl", b"fn main() {}"), ("i18n/vi.json", r#"{"name":"Trong gói gpufx"}"#.as_bytes())])
    }

    const WRAPPER: &str = r#"{"format":1,"type":"gpufx","id":"t.iris","version":"1.0.0","package":"iris.zip","tags":["circle reveal"]}"#;
    const VI: &str = r#"{"name":"Mống mắt","params":{"amount":{"label":"Lượng"}}}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-wrapper-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The wrapper's files: the manifest, `vi.json`, the inner zip of `inner_id`, and `extra`.
    fn files(inner_id: &str, with_inner: bool, extra: Option<(&str, &[u8])>) -> Vec<(String, Vec<u8>)> {
        let mut files = vec![("manifest.json".to_owned(), WRAPPER.as_bytes().to_vec()), ("i18n/vi.json".to_owned(), VI.as_bytes().to_vec())];
        if with_inner {
            files.push(("iris.zip".to_owned(), inner(inner_id)));
        }
        files.extend(extra.map(|(name, bytes)| (name.to_owned(), bytes.to_vec())));
        files
    }

    fn as_zip(dir: &Path, files: &[(String, Vec<u8>)]) -> PathBuf {
        let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())).collect();
        let path = dir.join("iris-wrapper.zip");
        fs::write(&path, zip_bytes(&borrowed)).unwrap();
        path
    }

    fn as_folder(dir: &Path, files: &[(String, Vec<u8>)]) -> PathBuf {
        let folder = dir.join("source").join("iris");
        let _ = fs::remove_dir_all(&folder);
        write_all(&folder, files).unwrap();
        folder
    }

    /// Installs `files` through `install`, a zip or a folder.
    fn install(dir: &Path, files: &[(String, Vec<u8>)], from_folder: bool, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
        let root = dir.join("root");
        if from_folder {
            crate::package_import::install_package(&as_folder(dir, files), &root, overwrite)
        } else {
            crate::package_import::install_package(&as_zip(dir, files), &root, overwrite)
        }
    }

    fn installed(dir: &Path) -> Vec<String> {
        let folder = dir.join("root/packages/gpufx/user");
        let mut names: Vec<String> = fs::read_dir(folder).map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn a_wrapper_zip_and_a_wrapper_folder_install_unpacked_one_level_and_translate_from_the_wrapper() {
        for from_folder in [false, true] {
            let dir = scratch(if from_folder { "good-folder" } else { "good-zip" });
            let outcome = install(&dir, &files("t.iris", true, None), from_folder, false).unwrap();
            assert_eq!(outcome, ImportOutcome::Installed(InstalledPackage { package: "packages/gpufx/user/iris".to_owned(), name: "Iris".to_owned(), kind: crate::packages::PackageKind::Effect, replaced: false }));
            let folder = dir.join("root/packages/gpufx/user/iris");
            assert!(folder.join("manifest.json").is_file() && folder.join("i18n/vi.json").is_file() && folder.join("iris.zip").is_file(), "{from_folder}");
            assert_eq!(fs::read(folder.join("iris.zip")).unwrap(), inner("t.iris"), "the inner zip is untouched");
            assert!(fs::read_dir(dir.join("root/packages")).unwrap().flatten().all(|entry| !entry.file_name().to_string_lossy().starts_with('.')), "no staging folder stays");
            let catalog = crate::packages::scan(&[dir.join("root")], read_package_info);
            let entry = catalog.effects.iter().find(|entry| entry.package == "packages/gpufx/user/iris").expect("listed by its wrapper folder");
            assert_eq!((entry.display_name("vi"), entry.display_name("en")), ("Mống mắt", "Iris"), "the wrapper's translation, not the inner one");
            assert_eq!(entry.param_i18n.label("vi", "amount"), Some("Lượng"));
            assert_eq!(entry.tags, vec!["circle reveal".to_owned()]);
            assert_eq!(crate::packages::filter_entries(&catalog.effects, "reveal").len(), 1, "the English tags are searched");
            let paths = d3d12_backend::ProjectPaths::roots_only(Path::new("p.json"), vec![dir.join("root")]);
            assert_eq!(paths.package_path("packages/gpufx/user/iris"), Some(dir.join("root/packages/gpufx/user/iris/iris.zip")), "a project's name opens the inner zip");
            assert_eq!(paths.package_path("iris"), Some(dir.join("root/packages/gpufx/user/iris/iris.zip")), "and so does the bare name");
            assert_eq!(install(&dir, &files("t.iris", true, None), from_folder, false), Ok(ImportOutcome::AlreadyInstalled { name: "Iris".to_owned() }));
            assert!(matches!(install(&dir, &files("t.iris", true, None), from_folder, true), Ok(ImportOutcome::Installed(InstalledPackage { replaced: true, .. }))));
            assert_eq!(installed(&dir), vec!["iris"]);
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_id_mismatch_a_missing_inner_zip_or_a_stray_file_installs_nothing() {
        for from_folder in [false, true] {
            let dir = scratch(if from_folder { "bad-folder" } else { "bad-zip" });
            assert_eq!(install(&dir, &files("t.other", true, None), from_folder, false), Err(ImportProblem::WrapperIdMismatch { wrapper: "t.iris".to_owned(), inner: "t.other".to_owned() }));
            assert_eq!(install(&dir, &files("t.iris", false, None), from_folder, false), Err(ImportProblem::WrapperMissingPackage("iris.zip".to_owned())));
            assert_eq!(install(&dir, &files("t.iris", true, Some(("readme.txt", b"x"))), from_folder, false), Err(ImportProblem::WrapperStrayFile("readme.txt".to_owned())));
            assert_eq!(install(&dir, &files("t.iris", true, Some(("i18n/sub/vi.json", b"{}"))), from_folder, false), Err(ImportProblem::WrapperStrayFile("i18n/sub/vi.json".to_owned())));
            assert_eq!(install(&dir, &files("t.iris", true, Some(("other.zip", b"x"))), from_folder, false), Err(ImportProblem::WrapperStrayFile("other.zip".to_owned())));
            let mut strict = files("t.iris", true, None);
            strict[0].1 = br#"{"format":1,"type":"gpufx","id":"t.iris","version":"1","package":"iris.zip","name":"Iris"}"#.to_vec();
            assert!(matches!(install(&dir, &strict, from_folder, false), Err(ImportProblem::BadManifest(_))), "an unknown field");
            assert!(installed(&dir).is_empty());
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_folder_that_is_not_a_wrapper_is_refused() {
        let dir = scratch("not-wrapper");
        let folder = dir.join("bare");
        write_all(&folder, &vec![("manifest.json".to_owned(), br#"{"id":"a","used_for":["gpufx"]}"#.to_vec())]).unwrap();
        assert_eq!(crate::package_import::install_package(&folder, &dir.join("root"), false), Err(ImportProblem::NotAWrapper));
        assert_eq!(crate::package_import::install_package(&dir, &dir.join("root"), false), Err(ImportProblem::NoManifest));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrapper_folder_is_turned_off_and_removed_as_one_pack() {
        let dir = scratch("disable");
        install(&dir, &files("t.iris", true, None), true, false).unwrap();
        let root = dir.join("root");
        let packs = crate::package_manager::list_installed(std::slice::from_ref(&root), Some(&root));
        let pack = packs.iter().find(|pack| pack.key == "gpufx/user/iris").expect("one pack, keyed by its folder");
        assert_eq!(pack.display_name("vi"), "Mống mắt");
        crate::package_manager::set_enabled(&root, &pack.key, false).unwrap();
        let paths = d3d12_backend::ProjectPaths::roots_only(Path::new("p.json"), vec![root.clone()]);
        assert_eq!(paths.package_path("packages/gpufx/user/iris"), None, "turned off");
        assert!(crate::packages::scan(std::slice::from_ref(&root), read_package_info).effects.is_empty());
        crate::package_manager::set_enabled(&root, &pack.key, true).unwrap();
        crate::package_manager::uninstall(std::slice::from_ref(&root), &root, pack).unwrap();
        assert!(installed(&dir).is_empty(), "the whole folder is gone");
        let _ = fs::remove_dir_all(&dir);
    }
}
