//! Installing an audio pack from a `.zip`. No window here.
//!
//! A zip with `"type": "sfx"` or `"music"` is unpacked once, because FFmpeg opens files by path, to
//! `<user root>/packages/<type>/<id lower-cased>/`. It is unpacked to a temporary folder on the same
//! drive, checked there and renamed into place once. Replacing a pack goes through a backup folder,
//! so a failure, or a crash, leaves the old pack and never a half-installed one.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use d3d12_backend::{is_safe_id, ASSET_AUDIO_EXTENSIONS};
use serde::Deserialize;
use zip::ZipArchive;

use crate::package_import::{entries_of, name_parts, read_manifest, validate_entries, EntryKind, EntryProblem, ImportOutcome, ImportProblem, Limits, ZipEntry, MIB};

/// What the zip of an audio pack may hold.
pub const AUDIO_LIMITS: Limits = Limits { files: 16_384, total_bytes: 1024 * MIB, file_bytes: 128 * MIB };
/// How much a file of a pack may grow when unpacked.
pub const MAX_COMPRESSION_RATIO: u64 = 200;
const MANIFEST: &str = "manifest.json";
/// Beside the type folders: what an install is working on, each ignored by the library.
const TEMPORARY_FOLDER: &str = ".install-tmp";
const BACKUP_FOLDER: &str = ".install-old";
const LOCK_FILE: &str = ".install.lock";
/// A file the pack holds besides its audio.
const DOCUMENT_EXTENSIONS: [&str; 2] = ["txt", "md"];
const SUPPORTED_FORMAT: u32 = 1;
/// The most sounds a manifest may list; a pack is read whole and listed in the window.
pub const MAX_MANIFEST_ITEMS: usize = 16_384;
/// The longest a sound may say it lasts; only keeps the time in range.
const MAX_ITEM_SECONDS: f64 = 86_400.0;
/// A folder that is briefly held open, by an indexer or a player, cannot be renamed yet.
const RENAME_ATTEMPTS: u32 = 5;
const RENAME_PAUSE: Duration = Duration::from_millis(100);
const BUFFER_BYTES: usize = 64 * 1024;

/// The types of audio pack, with their folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioPackKind {
    Sfx,
    Music,
}

impl AudioPackKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "sfx" => Some(Self::Sfx),
            "music" => Some(Self::Music),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sfx => "sfx",
            Self::Music => "music",
        }
    }
}

/// A pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAudioPack {
    pub kind: AudioPackKind,
    pub id: String,
    pub name: String,
    pub replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: AudioLicense,
    pub items: Vec<AudioManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioLicense {
    pub spdx: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioManifestItem {
    pub id: String,
    pub file: String,
    pub name: String,
    pub duration: f64,
    pub tags: Vec<String>,
    pub category: String,
    #[serde(rename = "loop")]
    pub looped: bool,
}

/// The extension of `name`, lower-cased.
fn extension(name: &str) -> String {
    name.rsplit_once('.').map_or_else(String::new, |(_, extension)| extension.to_ascii_lowercase())
}

fn is_audio_file(name: &str) -> bool {
    ASSET_AUDIO_EXTENSIONS.contains(&extension(name).as_str())
}

/// Parses a manifest. The fields are exactly those of the format, all required.
pub fn parse_audio_manifest(bytes: &[u8]) -> Result<AudioManifest, ImportProblem> {
    serde_json::from_slice(bytes).map_err(|e| ImportProblem::BadManifest(e.to_string()))
}

/// Checks what a manifest says, apart from the files. Gives the type of the pack.
pub fn check_manifest_fields(manifest: &AudioManifest) -> Result<AudioPackKind, ImportProblem> {
    let bad = |detail: String| Err(ImportProblem::BadManifest(detail));
    if manifest.format != SUPPORTED_FORMAT {
        return bad(format!("format {} is not supported", manifest.format));
    }
    let Some(kind) = AudioPackKind::from_name(&manifest.kind) else { return Err(ImportProblem::UnknownType(manifest.kind.clone())) };
    if !is_safe_id(&manifest.id) {
        return Err(ImportProblem::BadId(manifest.id.clone()));
    }
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() || manifest.license.spdx.trim().is_empty() {
        return bad("name, version and license.spdx must not be empty".to_owned());
    }
    if manifest.license.files.is_empty() {
        return bad("license.files is empty".to_owned());
    }
    if manifest.items.is_empty() {
        return bad("items is empty".to_owned());
    }
    if manifest.items.len() > MAX_MANIFEST_ITEMS {
        return bad(format!("items lists {} sounds, more than the {MAX_MANIFEST_ITEMS} allowed", manifest.items.len()));
    }
    let mut ids = HashSet::new();
    for item in &manifest.items {
        if item.id.trim().is_empty() || item.name.trim().is_empty() {
            return bad("an item has an empty id or name".to_owned());
        }
        if !ids.insert(item.id.to_lowercase()) {
            return bad(format!("the item id \"{}\" is used twice", item.id));
        }
        if !item.duration.is_finite() || item.duration <= 0.0 || item.duration > MAX_ITEM_SECONDS {
            return bad(format!("item \"{}\" has an invalid duration", item.id));
        }
    }
    Ok(kind)
}

/// `name` when it is a path a manifest may use: forward slashes and only safe parts.
fn checked_manifest_path(name: &str) -> Result<(), ImportProblem> {
    if name.contains('\\') {
        return Err(ImportProblem::BadManifest(format!("\"{name}\" uses a backslash")));
    }
    name_parts(name).map(|_| ()).map_err(|problem| ImportProblem::Entry { entry: name.to_owned(), problem })
}

/// Checks that the files a manifest names are files of the zip, and of the right kind.
fn check_manifest_files(manifest: &AudioManifest, entries: &[ZipEntry]) -> Result<(), ImportProblem> {
    let files: HashSet<String> = entries.iter().filter(|entry| entry.kind == EntryKind::File).map(|entry| entry.name.replace('\\', "/")).collect();
    let exists = |name: &str| -> Result<(), ImportProblem> {
        checked_manifest_path(name)?;
        if files.contains(name) {
            Ok(())
        } else {
            Err(ImportProblem::BadManifest(format!("\"{name}\" is not in the zip")))
        }
    };
    for name in &manifest.license.files {
        exists(name)?;
        if !DOCUMENT_EXTENSIONS.contains(&extension(name).as_str()) {
            return Err(ImportProblem::BadManifest(format!("\"{name}\" is not a text file")));
        }
    }
    for item in &manifest.items {
        exists(&item.file)?;
        if !is_audio_file(&item.file) {
            return Err(ImportProblem::BadManifest(format!("\"{}\" is not an audio file", item.file)));
        }
    }
    Ok(())
}

/// Only the audio, the documents and the manifest at the root may be in a pack.
fn check_extensions(entries: &[ZipEntry]) -> Result<(), ImportProblem> {
    for entry in entries.iter().filter(|entry| entry.kind == EntryKind::File) {
        let name = entry.name.replace('\\', "/");
        let allowed = name == MANIFEST || is_audio_file(&name) || DOCUMENT_EXTENSIONS.contains(&extension(&name).as_str());
        if !allowed {
            return Err(ImportProblem::Entry { entry: entry.name.clone(), problem: EntryProblem::ExtensionNotAllowed });
        }
    }
    Ok(())
}

/// How much a pack may hold, as really read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExtractLimits {
    pub limits: Limits,
    pub max_ratio: u64,
}

/// Unpacks every entry of `entries` under `folder`, counting the bytes as they come out: the sizes
/// a zip declares are never trusted.
pub(crate) fn extract_all<R: Read + Seek>(zip: &mut ZipArchive<R>, entries: &[ZipEntry], folder: &Path, limits: ExtractLimits) -> Result<(), ImportProblem> {
    let write_failed = |e: std::io::Error| ImportProblem::WriteFailed(e.to_string());
    let mut total = 0u64;
    let mut buffer = vec![0u8; BUFFER_BYTES];
    for (index, entry) in entries.iter().enumerate() {
        let parts = name_parts(&entry.name).map_err(|problem| ImportProblem::Entry { entry: entry.name.clone(), problem })?;
        let target = parts.iter().fold(folder.to_path_buf(), |path, part| path.join(part));
        if entry.kind == EntryKind::Directory {
            fs::create_dir_all(&target).map_err(write_failed)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(write_failed)?;
        }
        let mut input = zip.by_index(index).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
        let compressed = input.compressed_size().max(1);
        let declared = input.size();
        // create_new: two entries can never end up in one file, however the checks above went.
        let mut output = BufWriter::new(File::create_new(&target).map_err(write_failed)?);
        let mut real = 0u64;
        loop {
            let count = input.read(&mut buffer).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
            if count == 0 {
                break;
            }
            real = real.saturating_add(count as u64);
            total = total.saturating_add(count as u64);
            if real > limits.limits.file_bytes {
                return Err(ImportProblem::TooLarge { entry: Some(entry.name.clone()), limit_mib: limits.limits.file_bytes / MIB });
            }
            if total > limits.limits.total_bytes {
                return Err(ImportProblem::TooLarge { entry: None, limit_mib: limits.limits.total_bytes / MIB });
            }
            if real > compressed.saturating_mul(limits.max_ratio) {
                return Err(ImportProblem::RatioTooHigh { entry: entry.name.clone(), limit: limits.max_ratio });
            }
            output.write_all(&buffer[..count]).map_err(write_failed)?;
        }
        output.flush().map_err(write_failed)?;
        // On disk before the folder is renamed into place, as a project save does.
        output.into_inner().map_err(|e| write_failed(e.into_error()))?.sync_all().map_err(write_failed)?;
        if real != declared {
            return Err(ImportProblem::SizeMismatch { entry: entry.name.clone() });
        }
    }
    Ok(())
}

fn rename_with_retries(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = Ok(());
    for attempt in 0..RENAME_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(RENAME_PAUSE);
        }
        last = fs::rename(from, to);
        if last.is_ok() {
            return last;
        }
    }
    last
}

/// Puts `new` at `target`, moving what is there to `backup` first. A failure puts the old folder
/// back, so `target` is never left half done.
pub(crate) fn replace_folder(new: &Path, target: &Path, backup: &Path, mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let replacing = target.exists();
    if replacing {
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent)?;
        }
        rename(target, backup)?;
    }
    if let Err(error) = rename(new, target) {
        if replacing {
            rename(backup, target)?;
        }
        return Err(error);
    }
    if replacing {
        let _ = fs::remove_dir_all(backup);
    }
    Ok(())
}

/// Removes what a crashed install left. A backup whose pack is gone is put back, so a crash during
/// a replace loses nothing; only called with the install lock held.
fn clean_leftovers(packages: &Path) {
    clean_leftovers_with(packages, &mut rename_with_retries);
}

fn clean_leftovers_with(packages: &Path, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) {
    let _ = fs::remove_dir_all(packages.join(TEMPORARY_FOLDER));
    let backups = packages.join(BACKUP_FOLDER);
    let Ok(types) = fs::read_dir(&backups) else { return };
    for kind in types.flatten() {
        let Ok(packs) = fs::read_dir(kind.path()) else { continue };
        for pack in packs.flatten() {
            let target = packages.join(kind.file_name()).join(pack.file_name());
            // Only a whole pack comes back: a backup with no manifest was never one.
            let whole = pack.path().join(MANIFEST).is_file();
            if !target.exists() && whole {
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = rename(&pack.path(), &target);
            }
            // A backup whose restore failed may be the only copy of the pack, so it stays.
            if target.exists() || !whole {
                let _ = fs::remove_dir_all(pack.path());
            }
        }
        let _ = fs::remove_dir(kind.path());
    }
    let _ = fs::remove_dir(backups);
}

/// Takes the install lock: held for a whole install or recovery, so no second editor works in the
/// working folders at the same time.
fn lock_installs(packages: &Path) -> std::io::Result<File> {
    fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(packages.join(LOCK_FILE))
}

/// Puts back what an install that was cut off left, once per process, so a pack that was being
/// replaced when the editor died is back before the library is first read. Skipped, silently, when
/// another editor holds the install lock: it is installing, or doing this itself.
pub fn recover_interrupted_install(user_root: &Path) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if !DONE.swap(true, Ordering::SeqCst) {
        recover_interrupted_install_with(user_root, &mut rename_with_retries);
    }
}

pub(crate) fn recover_interrupted_install_with(user_root: &Path, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) {
    let packages = user_root.join("packages");
    if !packages.join(BACKUP_FOLDER).exists() && !packages.join(TEMPORARY_FOLDER).exists() {
        return;
    }
    // With the lock held no install is running, so nothing in the working folders is in use.
    let Ok(_lock) = lock_installs(&packages) else { return };
    clean_leftovers_with(&packages, rename);
}

/// Takes away the working folders once they are empty; one still in use stays.
fn remove_empty_working_folders(packages: &Path, kind: AudioPackKind) {
    let _ = fs::remove_dir(packages.join(TEMPORARY_FOLDER));
    let backups = packages.join(BACKUP_FOLDER);
    let _ = fs::remove_dir(backups.join(kind.name()));
    let _ = fs::remove_dir(backups);
}

/// Unique within this process; the process id keeps two processes apart.
fn next_temporary_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The manifest of the installed pack at `target`, if it can be read.
fn installed_manifest(target: &Path) -> Option<AudioManifest> {
    let bytes = fs::read(target.join(MANIFEST)).ok()?;
    parse_audio_manifest(&bytes).ok()
}

/// Checks the zip at `source` and installs it under `user_root`. An existing pack of the same id
/// is replaced only when `overwrite` allows it.
pub fn install_audio_pack(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    install_audio_pack_with(source, user_root, overwrite, ExtractLimits { limits: AUDIO_LIMITS, max_ratio: MAX_COMPRESSION_RATIO }, &mut rename_with_retries)
}

pub(crate) fn install_audio_pack_with(source: &Path, user_root: &Path, overwrite: bool, limits: ExtractLimits, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) -> Result<ImportOutcome, ImportProblem> {
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    // Held for the whole install, so a second editor cannot clean away the folder this one works in.
    let _lock = lock_installs(&packages).map_err(|e| ImportProblem::WriteFailed(format!("another install is running ({e})")))?;
    clean_leftovers(&packages);

    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, limits.limits)?;
    validate_entries(&entries, limits.limits)?;
    check_extensions(&entries)?;
    let manifest = parse_audio_manifest(&read_manifest(&mut zip)?)?;
    let kind = check_manifest_fields(&manifest)?;
    check_manifest_files(&manifest, &entries)?;

    let id = manifest.id.to_ascii_lowercase();
    let target = packages.join(kind.name()).join(&id);
    let replaced = target.exists();
    if replaced && !overwrite {
        return Ok(ImportOutcome::AlreadyInstalled { name: installed_manifest(&target).map_or_else(|| manifest.name.clone(), |installed| installed.name) });
    }
    let temporary = packages.join(TEMPORARY_FOLDER).join(next_temporary_name());
    let result = fs::create_dir_all(&temporary).map_err(|e| ImportProblem::WriteFailed(e.to_string())).and_then(|()| {
        extract_all(&mut zip, &entries, &temporary, limits)?;
        // What was unpacked is what is installed: the manifest is read back from the folder.
        let unpacked = parse_audio_manifest(&fs::read(temporary.join(MANIFEST)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?)?;
        if unpacked != manifest {
            return Err(ImportProblem::BadManifest("it changed while the zip was being read".to_owned()));
        }
        let backup = packages.join(BACKUP_FOLDER).join(kind.name()).join(&id);
        replace_folder(&temporary, &target, &backup, |from, to| rename(from, to)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    remove_empty_working_folders(&packages, kind);
    result?;
    Ok(ImportOutcome::AudioInstalled(InstalledAudioPack { kind, id, name: manifest.name, replaced }))
}

/// The folder an installed pack of `kind` and `id` lives in.
pub fn pack_folder(user_root: &Path, kind: AudioPackKind, id: &str) -> PathBuf {
    user_root.join("packages").join(kind.name()).join(id.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-asset-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest_json(id: &str, kind: &str) -> String {
        format!(r#"{{"format":1,"type":"{kind}","id":"{id}","name":"SFX: Impact","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt","NOTICE.txt"]}},"items":[{{"id":"boom","file":"audio/boom.flac","name":"Boom","duration":1.2,"tags":["hit"],"category":"impact","loop":false}}]}}"#)
    }

    fn pack_zip(manifest: &str, audio: &[u8]) -> Vec<u8> {
        zip_of(&[("manifest.json", manifest.as_bytes()), ("LICENSE.txt", b"license"), ("NOTICE.txt", b"notice"), ("audio/boom.flac", audio)])
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn written(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn folders(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root.join("packages")).map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).filter(|name| name != LOCK_FILE).collect()).unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn a_pack_is_unpacked_under_its_type_and_lower_cased_id() {
        let dir = scratch("ok");
        let root = dir.join("root");
        let outcome = install_audio_pack(&written(&dir, &pack_zip(&manifest_json("AVS-SFX-Impact", "sfx"), b"sound")), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::AudioInstalled(InstalledAudioPack { kind: AudioPackKind::Sfx, id: "avs-sfx-impact".to_owned(), name: "SFX: Impact".to_owned(), replaced: false }));
        let pack = pack_folder(&root, AudioPackKind::Sfx, "AVS-SFX-Impact");
        assert_eq!(fs::read(pack.join("audio/boom.flac")).unwrap(), b"sound");
        assert!(pack.join("manifest.json").is_file() && pack.join("LICENSE.txt").is_file());
        assert_eq!(folders(&root), vec!["sfx"], "no temporary folder stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_that_is_not_exactly_the_format_is_refused() {
        let full = manifest_json("p", "sfx");
        for (name, text, wanted) in [
            ("a missing field", full.replace(r#","version":"1.0.0""#, ""), "BadManifest"),
            ("an unknown field", full.replacen(r#"{"format":1,"#, r#"{"format":1,"extra":1,"#, 1), "BadManifest"),
            ("an item missing loop", full.replace(r#","loop":false"#, ""), "BadManifest"),
            ("another format", full.replacen(r#""format":1"#, r#""format":2"#, 1), "BadManifest"),
            ("no items", full.split(r#""items""#).next().unwrap().to_owned() + r#""items":[]}"#, "BadManifest"),
            ("a file not in the zip", full.replace("audio/boom.flac", "audio/gone.flac"), "BadManifest"),
            ("a licence file not in the zip", full.replace("NOTICE.txt", "GONE.txt"), "BadManifest"),
            ("a non-audio item", full.replace("audio/boom.flac", "LICENSE.txt"), "BadManifest"),
            ("a traversal in file", full.replace("audio/boom.flac", "../boom.flac"), "Entry"),
            ("a colon in file", full.replace("audio/boom.flac", "audio/boom.flac:x"), "Entry"),
            ("a backslash in file", full.replace("audio/boom.flac", r"audio\\boom.flac"), "BadManifest"),
            ("an empty duration", full.replace(r#""duration":1.2"#, r#""duration":0"#), "BadManifest"),
            ("an unsafe id", full.replace(r#""id":"p""#, r#""id":"../p""#), "BadId"),
        ] {
            let dir = scratch("manifest");
            let error = install_audio_pack(&written(&dir, &pack_zip(&text, b"x")), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(folders(&dir.join("root")).iter().all(|folder| !matches!(folder.as_str(), "sfx" | "music")), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_extension_the_type_does_not_allow_is_refused() {
        let dir = scratch("extension");
        let bytes = zip_of(&[("manifest.json", manifest_json("p", "sfx").as_bytes()), ("LICENSE.txt", b"l"), ("NOTICE.txt", b"n"), ("audio/boom.flac", b"x"), ("audio/run.exe", b"MZ")]);
        assert_eq!(install_audio_pack(&written(&dir, &bytes), &dir.join("root"), false), Err(ImportProblem::Entry { entry: "audio/run.exe".to_owned(), problem: EntryProblem::ExtensionNotAllowed }));
        let _ = fs::remove_dir_all(&dir);
    }

    fn huge_compressible() -> Vec<u8> {
        vec![0u8; 8 * MIB as usize]
    }

    #[test]
    fn a_zip_bomb_is_stopped_by_the_ratio_of_the_real_bytes() {
        let dir = scratch("bomb");
        let root = dir.join("root");
        let outcome = install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), &huge_compressible())), &root, false);
        assert_eq!(outcome, Err(ImportProblem::RatioTooHigh { entry: "audio/boom.flac".to_owned(), limit: 200 }));
        assert!(!pack_folder(&root, AudioPackKind::Sfx, "p").exists());
        assert_eq!(fs::read_dir(root.join("packages").join(TEMPORARY_FOLDER)).map_or(0, Iterator::count), 0, "the temporary folder is gone");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Rewrites the size a zip declares for `name`, in the local and the central header.
    fn declare_size(bytes: &mut [u8], name: &str, size: u32) {
        const LOCAL_SIGNATURE: &[u8; 4] = b"PK\x03\x04";
        const CENTRAL_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
        let name = name.as_bytes();
        for at in 0..bytes.len().saturating_sub(46) {
            let (signature, name_at, size_at) = if &bytes[at..at + 4] == LOCAL_SIGNATURE {
                (true, at + 30, at + 22)
            } else if &bytes[at..at + 4] == CENTRAL_SIGNATURE {
                (true, at + 46, at + 24)
            } else {
                (false, 0, 0)
            };
            if signature && bytes.get(name_at..name_at + name.len()) == Some(name) {
                bytes[size_at..size_at + 4].copy_from_slice(&size.to_le_bytes());
            }
        }
    }

    #[test]
    fn a_declared_size_smaller_than_the_real_one_is_not_believed() {
        let dir = scratch("lying");
        let root = dir.join("root");
        let mut bytes = pack_zip(&manifest_json("p", "sfx"), &[7u8; 1024]);
        declare_size(&mut bytes, "audio/boom.flac", 4);
        let error = install_audio_pack(&written(&dir, &bytes), &root, false).unwrap_err();
        assert!(matches!(error, ImportProblem::SizeMismatch { .. } | ImportProblem::NotAZip(_)), "{error:?}");
        assert!(!pack_folder(&root, AudioPackKind::Sfx, "p").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_and_a_pack_over_the_real_limits_are_refused_as_they_unpack() {
        let dir = scratch("limits");
        let root = dir.join("root");
        let bytes = pack_zip(&manifest_json("p", "sfx"), &[1u8; 4096]);
        let refused = |limits: Limits| install_audio_pack_with(&written(&dir, &bytes), &root, false, ExtractLimits { limits, max_ratio: u64::MAX }, &mut |from, to| fs::rename(from, to));
        let one_file = refused(Limits { files: 16, total_bytes: 1024 * MIB, file_bytes: 1024 });
        assert!(matches!(&one_file, Err(ImportProblem::TooLarge { entry: Some(entry), .. }) if entry == "audio/boom.flac"), "{one_file:?}");
        let too_many = refused(Limits { files: 2, total_bytes: 1024 * MIB, file_bytes: 1024 * MIB });
        assert_eq!(too_many, Err(ImportProblem::TooManyFiles { limit: 2 }));
        assert!(!pack_folder(&root, AudioPackKind::Sfx, "p").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_total_limit_counts_the_real_bytes_even_with_a_lying_declared_size() {
        let dir = scratch("total");
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file("manifest.json", stored).unwrap();
        writer.write_all(manifest_json("p", "sfx").as_bytes()).unwrap();
        for name in ["LICENSE.txt", "NOTICE.txt"] {
            writer.start_file(name, stored).unwrap();
            writer.write_all(b"text").unwrap();
        }
        writer.start_file("audio/boom.flac", stored).unwrap();
        writer.write_all(&[3u8; 3000]).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let limits = Limits { files: 16, total_bytes: 2000, file_bytes: 1024 * MIB };
        // The declared total is over the limit too here; real counting is the extract test below.
        let source = written(&dir, &bytes);
        let error = install_audio_pack_with(&source, &dir.join("root"), false, ExtractLimits { limits, max_ratio: u64::MAX }, &mut |from, to| fs::rename(from, to)).unwrap_err();
        assert_eq!(error, ImportProblem::TooLarge { entry: None, limit_mib: 0 });
        let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let entries = entries_of(&mut zip, AUDIO_LIMITS).unwrap();
        let target = dir.join("extract");
        fs::create_dir_all(&target).unwrap();
        let error = extract_all(&mut zip, &entries, &target, ExtractLimits { limits, max_ratio: u64::MAX }).unwrap_err();
        assert_eq!(error, ImportProblem::TooLarge { entry: None, limit_mib: 0 }, "counted while unpacking, not from the headers");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_pack_asks_first_and_is_replaced_when_allowed() {
        let dir = scratch("overwrite");
        let root = dir.join("root");
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"old")), &root, false).unwrap();
        let newer = pack_zip(&manifest_json("p", "sfx").replace("SFX: Impact", "New Name"), b"new");
        let source = written(&dir, &newer);
        assert_eq!(install_audio_pack(&source, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "SFX: Impact".to_owned() }), "the name already installed");
        let pack = pack_folder(&root, AudioPackKind::Sfx, "p");
        assert_eq!(fs::read(pack.join("audio/boom.flac")).unwrap(), b"old");
        assert!(matches!(install_audio_pack(&source, &root, true), Ok(ImportOutcome::AudioInstalled(InstalledAudioPack { replaced: true, .. }))));
        assert_eq!(fs::read(pack.join("audio/boom.flac")).unwrap(), b"new");
        assert_eq!(folders(&root), vec!["sfx"], "no backup stays");
        let _ = fs::remove_dir_all(&dir);
    }

    fn assert_old_pack_is_whole(root: &Path) {
        let pack = pack_folder(root, AudioPackKind::Sfx, "p");
        assert_eq!(fs::read(pack.join("audio/boom.flac")).unwrap(), b"old");
        assert_eq!(installed_manifest(&pack).unwrap().name, "SFX: Impact");
        assert_eq!(fs::read_dir(pack).unwrap().count(), 4, "exactly the old files");
    }

    #[test]
    fn a_failure_while_unpacking_keeps_the_old_pack() {
        let dir = scratch("broken-new");
        let root = dir.join("root");
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"old")), &root, false).unwrap();
        let mut broken = pack_zip(&manifest_json("p", "sfx"), &[9u8; 2048]);
        declare_size(&mut broken, "audio/boom.flac", 8);
        assert!(install_audio_pack(&written(&dir, &broken), &root, true).is_err());
        assert_old_pack_is_whole(&root);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pack_that_cannot_be_moved_keeps_the_old_one() {
        let dir = scratch("held");
        let root = dir.join("root");
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"old")), &root, false).unwrap();
        let held = pack_folder(&root, AudioPackKind::Sfx, "p").join("audio/boom.flac");
        // Opened without sharing, as a player may hold it: the folder cannot be renamed.
        let lock = fs::OpenOptions::new().read(true).share_mode(0).open(&held).unwrap();
        let outcome = install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"new")), &root, true);
        drop(lock);
        assert!(matches!(outcome, Err(ImportProblem::WriteFailed(_))), "{outcome:?}");
        assert_old_pack_is_whole(&root);
        assert_eq!(fs::read_dir(root.join("packages").join(TEMPORARY_FOLDER)).map_or(0, Iterator::count), 0, "what was unpacked is gone");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failure_after_the_old_pack_moved_away_puts_it_back() {
        let dir = scratch("second-rename");
        let (new, target, backup) = (dir.join("new"), dir.join("target"), dir.join("backup"));
        for (folder, content) in [(&new, "new"), (&target, "old")] {
            fs::create_dir_all(folder).unwrap();
            fs::write(folder.join("a"), content).unwrap();
        }
        let mut calls = 0;
        let result = replace_folder(&new, &target, &backup, |from, to| {
            calls += 1;
            if calls == 2 {
                return Err(std::io::Error::other("disk full"));
            }
            fs::rename(from, to)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(target.join("a")).unwrap(), b"old");
        assert!(!backup.exists());
        assert!(new.exists(), "the caller removes what it unpacked");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_earlier_crash_leaves_nothing_and_a_backup_whose_pack_is_gone_comes_back() {
        let dir = scratch("crash");
        let root = dir.join("root");
        let packages = root.join("packages");
        let stale = packages.join(TEMPORARY_FOLDER).join("old-run");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("half.flac"), b"x").unwrap();
        let backup = packages.join(BACKUP_FOLDER).join("sfx").join("q");
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("manifest.json"), manifest_json("q", "sfx")).unwrap();
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"new")), &root, false).unwrap();
        let spare = packages.join(BACKUP_FOLDER).join("sfx").join("p");
        fs::create_dir_all(&spare).unwrap();
        fs::write(spare.join("manifest.json"), b"{}").unwrap();
        let empty = packages.join(BACKUP_FOLDER).join("sfx").join("s");
        fs::create_dir_all(&empty).unwrap();
        fs::create_dir_all(&stale).unwrap();
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("r", "sfx"), b"other")), &root, false).unwrap();
        assert!(!stale.exists());
        assert!(!pack_folder(&root, AudioPackKind::Sfx, "s").exists(), "a backup with no manifest was never a pack");
        assert!(!packages.join(BACKUP_FOLDER).exists());
        assert!(pack_folder(&root, AudioPackKind::Sfx, "q").join("manifest.json").is_file(), "a crash during a replace loses nothing");
        assert_eq!(fs::read(pack_folder(&root, AudioPackKind::Sfx, "p").join("audio/boom.flac")).unwrap(), b"new", "a backup is not put over a pack");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A pack of `root` moved to where a replace that was cut off leaves it.
    fn move_to_backup(root: &Path, id: &str) -> PathBuf {
        let backup = root.join("packages").join(BACKUP_FOLDER).join("sfx").join(id);
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::rename(pack_folder(root, AudioPackKind::Sfx, id), &backup).unwrap();
        backup
    }

    #[test]
    fn a_restore_that_fails_keeps_the_backup_which_is_the_only_copy() {
        let dir = scratch("restore-fails");
        let root = dir.join("root");
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"only copy")), &root, false).unwrap();
        let backup = move_to_backup(&root, "p");
        clean_leftovers_with(&root.join("packages"), &mut |_, _| Err(std::io::Error::other("held by an indexer")));
        assert_eq!(fs::read(backup.join("audio/boom.flac")).unwrap(), b"only copy", "the backup stays");
        assert!(!pack_folder(&root, AudioPackKind::Sfx, "p").exists());
        // Once the restore can go through, the next clean-up brings it back.
        clean_leftovers_with(&root.join("packages"), &mut |from, to| fs::rename(from, to));
        assert_eq!(fs::read(pack_folder(&root, AudioPackKind::Sfx, "p").join("audio/boom.flac")).unwrap(), b"only copy");
        assert!(!root.join("packages").join(BACKUP_FOLDER).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_puts_back_a_pack_that_a_cut_off_replace_left_in_the_backup_and_lists_it() {
        let dir = scratch("recover");
        let root = dir.join("root");
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"old")), &root, false).unwrap();
        move_to_backup(&root, "p");
        let stale = root.join("packages").join(TEMPORARY_FOLDER).join("half");
        fs::create_dir_all(&stale).unwrap();
        assert!(crate::audio_library::scan_audio_packs(&[root.clone()]).packs.is_empty(), "missing until recovery runs");
        recover_interrupted_install_with(&root, &mut |from, to| fs::rename(from, to));
        let library = crate::audio_library::scan_audio_packs(&[root.clone()]);
        assert_eq!(library.packs.len(), 1, "{:?}", library.problems);
        assert_eq!(library.packs[0].sounds.len(), 1);
        assert!(!stale.exists() && !root.join("packages").join(BACKUP_FOLDER).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_does_nothing_while_another_editor_holds_the_install_lock() {
        let dir = scratch("recover-locked");
        let root = dir.join("root");
        install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"old")), &root, false).unwrap();
        let backup = move_to_backup(&root, "p");
        let working = root.join("packages").join(TEMPORARY_FOLDER).join("live-install");
        fs::create_dir_all(&working).unwrap();
        let lock = lock_installs(&root.join("packages")).unwrap();
        recover_interrupted_install_with(&root, &mut |from, to| fs::rename(from, to));
        assert!(backup.exists() && working.exists(), "a live install's folders are not touched");
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_with_more_sounds_than_the_cap_is_refused() {
        let item = |index: usize| AudioManifestItem { id: format!("s{index}"), file: "audio/a.flac".to_owned(), name: "S".to_owned(), duration: 1.0, tags: Vec::new(), category: "c".to_owned(), looped: false };
        let manifest = |count: usize| AudioManifest { format: 1, kind: "sfx".to_owned(), id: "p".to_owned(), name: "P".to_owned(), version: "1".to_owned(), license: AudioLicense { spdx: "MIT".to_owned(), files: vec!["L.txt".to_owned()] }, items: (0..count).map(item).collect() };
        assert_eq!(check_manifest_fields(&manifest(MAX_MANIFEST_ITEMS)), Ok(AudioPackKind::Sfx));
        let refused = check_manifest_fields(&manifest(MAX_MANIFEST_ITEMS + 1));
        assert!(matches!(&refused, Err(ImportProblem::BadManifest(message)) if message.contains("16384")), "{refused:?}");
    }

    #[test]
    fn a_second_install_at_the_same_time_is_refused_and_does_not_touch_the_first() {
        let dir = scratch("lock");
        let root = dir.join("root");
        fs::create_dir_all(root.join("packages")).unwrap();
        let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(root.join("packages").join(LOCK_FILE)).unwrap();
        let working = root.join("packages").join(TEMPORARY_FOLDER).join("other-run");
        fs::create_dir_all(&working).unwrap();
        let outcome = install_audio_pack(&written(&dir, &pack_zip(&manifest_json("p", "sfx"), b"x")), &root, false);
        assert!(matches!(outcome, Err(ImportProblem::WriteFailed(_))), "{outcome:?}");
        assert!(working.exists(), "another install's folder is not cleaned away");
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }
}
