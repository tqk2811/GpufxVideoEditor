//! Installing a package from an external `.zip` the user picked. No window here.
//!
//! A zip has `manifest.json` at its root. A gpufx shader package has no `type` in it, since its
//! manifest refuses unknown fields; it becomes an effect or a transition from its inputs, and is
//! kept whole at `<user root>/packages/gpufx/user/<manifest id>.zip`, read by `Package::open`. The
//! other packages name their `type`: `sfx` and `music` go to [`crate::asset_install`], which uses
//! the checks here; the editor cannot use the rest yet, so nothing is installed.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::Path;

use d3d12_backend::{check_path_part, is_safe_id, read_zip_package_info, PathPartProblem};
use serde_json::Value;
use zip::result::ZipError;
use zip::ZipArchive;

use crate::clip_animation_install::{install_clip_animation_pack, is_clip_animation_type, InstalledClipAnimationPack};
use crate::asset_install::{install_audio_pack, AudioPackKind, InstalledAudioPack};
use crate::packages::{classify, PackageKind};
use crate::sticker_install::{install_sticker_pack, InstalledStickerPack, STICKER_TYPE};

pub(crate) const MIB: u64 = 1 << 20;
/// The folder under `packages/gpufx` that imported shader packages go to.
pub const USER_GROUP: &str = "user";
const MANIFEST: &str = "manifest.json";
/// A manifest is a few KiB; this only stops a zip bomb from filling the memory.
const MAX_MANIFEST_BYTES: u64 = 16 * MIB;
/// Anything larger is not worth opening, whatever is inside it.
const MAX_ARCHIVE_BYTES: u64 = 2048 * MIB;
/// A shader zip is read whole: its files, within [`SHADER_LIMITS`], and the zip's own headers.
const MAX_SHADER_ARCHIVE_BYTES: u64 = 264 * MIB;
/// The types of the packages the editor cannot use yet.
const NON_SHADER_TYPES: [&str; 3] = ["font", "text-style", "subtitle-style"];

/// What a zip may declare. Sizes are as declared: what is really read is counted where it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub files: usize,
    pub total_bytes: u64,
    pub file_bytes: u64,
}

/// For any zip, large enough for the audio and sticker packs that are told apart by their manifest.
pub const ARCHIVE_LIMITS: Limits = Limits { files: 16_384, total_bytes: 1024 * MIB, file_bytes: 1024 * MIB };
/// For a shader zip: what gpufx-core itself accepts.
pub const SHADER_LIMITS: Limits = Limits { files: 1024, total_bytes: 256 * MIB, file_bytes: 256 * MIB };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    /// A device, pipe or socket.
    Special,
}

/// One entry of a zip, as far as checking it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub size: u64,
    pub kind: EntryKind,
    pub encrypted: bool,
}

/// What is wrong with the name or kind of one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryProblem {
    /// Absolute, empty, or with a `.` or `..` part.
    UnsafePath,
    Symlink,
    NotAFile,
    /// A `:` opens a stream of an NTFS file, or names a drive.
    Colon,
    InvalidCharacter,
    /// `CON`, `NUL`, `COM1`..., whatever follows them.
    DeviceName,
    /// Windows drops the dot or space, so the name would mean another file.
    TrailingDotOrSpace,
    /// The same path twice, or two that differ only in case, as one file on Windows.
    Duplicate,
    Encrypted,
    /// Not a file this type of pack may hold.
    ExtensionNotAllowed,
}

/// Why a zip was not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportProblem {
    Unreadable(String),
    NotAZip(String),
    Entry { entry: String, problem: EntryProblem },
    TooManyFiles { limit: usize },
    TooLarge { entry: Option<String>, limit_mib: u64 },
    NoManifest,
    BadManifest(String),
    UnknownType(String),
    /// A known type the editor cannot use yet.
    NotSupportedYet(String),
    /// gpufx refused the package.
    BadPackage(String),
    /// The package is neither an effect nor a transition.
    NotUsable { inputs: usize },
    /// The installed package of this id is another kind, so projects using it would break.
    KindChanged { existing: PackageKind, new: PackageKind },
    /// The manifest id cannot be a file name.
    BadId(String),
    NoUserFolder,
    WriteFailed(String),
    /// A file unpacked to another size than the zip declares.
    SizeMismatch { entry: String },
    /// A file grew by more than the limit when unpacked, as a zip bomb does.
    RatioTooHigh { entry: String, limit: u64 },
}

/// What installing a zip came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Installed(InstalledPackage),
    AudioInstalled(InstalledAudioPack),
    StickerInstalled(InstalledStickerPack),
    ClipAnimationInstalled(InstalledClipAnimationPack),
    /// A package with this name is there, and overwriting was not allowed.
    AlreadyInstalled { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    /// The name a project stores.
    pub package: String,
    pub name: String,
    pub kind: PackageKind,
    pub replaced: bool,
}

/// Checks one part of a path. An empty part, `.` and `..` are the caller's.
fn check_part(part: &str) -> Result<(), EntryProblem> {
    check_path_part(part).map_err(|problem| match problem {
        PathPartProblem::Colon => EntryProblem::Colon,
        PathPartProblem::InvalidCharacter => EntryProblem::InvalidCharacter,
        PathPartProblem::DeviceName => EntryProblem::DeviceName,
        PathPartProblem::TrailingDotOrSpace => EntryProblem::TrailingDotOrSpace,
    })
}

/// The parts of an entry name, with both kinds of slash, or why they are unsafe.
pub(crate) fn name_parts(name: &str) -> Result<Vec<&str>, EntryProblem> {
    let name = name.strip_suffix(['/', '\\']).unwrap_or(name);
    let parts: Vec<&str> = name.split(['/', '\\']).collect();
    if parts.iter().any(|part| part.is_empty() || *part == "." || *part == "..") {
        return Err(EntryProblem::UnsafePath);
    }
    for part in &parts {
        check_part(part)?;
    }
    Ok(parts)
}

/// Checks every entry against what is safe on Windows and `limits`. Touches no file.
pub fn validate_entries(entries: &[ZipEntry], limits: Limits) -> Result<(), ImportProblem> {
    if entries.len() > limits.files {
        return Err(ImportProblem::TooManyFiles { limit: limits.files });
    }
    // What each path is spelled like, lower-cased, and whether a file ends there.
    let mut seen: HashMap<String, (String, bool)> = HashMap::new();
    let mut total = 0u64;
    let refuse = |entry: &ZipEntry, problem| ImportProblem::Entry { entry: entry.name.clone(), problem };
    for entry in entries {
        if entry.encrypted {
            return Err(refuse(entry, EntryProblem::Encrypted));
        }
        match entry.kind {
            EntryKind::File | EntryKind::Directory => {}
            EntryKind::Symlink => return Err(refuse(entry, EntryProblem::Symlink)),
            EntryKind::Special => return Err(refuse(entry, EntryProblem::NotAFile)),
        }
        let parts = name_parts(&entry.name).map_err(|problem| refuse(entry, problem))?;
        let mut path = String::new();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                path.push('/');
            }
            path.push_str(part);
            let is_file = entry.kind == EntryKind::File && index + 1 == parts.len();
            match seen.get(&path.to_lowercase()) {
                Some((spelled, file_there)) if *spelled != path || is_file || *file_there => return Err(refuse(entry, EntryProblem::Duplicate)),
                Some(_) => {}
                None => {
                    seen.insert(path.to_lowercase(), (path.clone(), is_file));
                }
            }
        }
        if entry.size > limits.file_bytes {
            return Err(ImportProblem::TooLarge { entry: Some(entry.name.clone()), limit_mib: limits.file_bytes / MIB });
        }
        total = total.saturating_add(entry.size);
    }
    if total > limits.total_bytes {
        return Err(ImportProblem::TooLarge { entry: None, limit_mib: limits.total_bytes / MIB });
    }
    Ok(())
}

/// What a manifest says the package is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Declared {
    /// A gpufx shader package, which has no `type`, with its id.
    Shader(Option<String>),
    Audio(AudioPackKind),
    Sticker,
    ClipAnimation,
}

/// What the manifest declares. A `type` the editor cannot use is an error here, so that it is
/// reported before anything is installed.
fn declared_package(manifest: &[u8]) -> Result<Declared, ImportProblem> {
    let value: Value = serde_json::from_slice(manifest).map_err(|e| ImportProblem::BadManifest(e.to_string()))?;
    let Some(object) = value.as_object() else { return Err(ImportProblem::BadManifest("it is not a JSON object".to_owned())) };
    match object.get("type") {
        None => Ok(Declared::Shader(object.get("id").and_then(Value::as_str).map(str::to_owned))),
        Some(Value::String(kind)) if AudioPackKind::from_name(kind).is_some() => Ok(Declared::Audio(AudioPackKind::from_name(kind).unwrap_or(AudioPackKind::Sfx))),
        Some(Value::String(kind)) if kind == STICKER_TYPE => Ok(Declared::Sticker),
        Some(Value::String(kind)) if is_clip_animation_type(kind) => Ok(Declared::ClipAnimation),
        Some(Value::String(kind)) if NON_SHADER_TYPES.contains(&kind.as_str()) => Err(ImportProblem::NotSupportedYet(kind.clone())),
        Some(Value::String(kind)) => Err(ImportProblem::UnknownType(kind.clone())),
        Some(_) => Err(ImportProblem::BadManifest("`type` is not a string".to_owned())),
    }
}

pub(crate) fn entries_of<R: Read + Seek>(zip: &mut ZipArchive<R>, limits: Limits) -> Result<Vec<ZipEntry>, ImportProblem> {
    if zip.len() > limits.files {
        return Err(ImportProblem::TooManyFiles { limit: limits.files });
    }
    let not_a_zip = |e: ZipError| ImportProblem::NotAZip(e.to_string());
    let mut entries = Vec::with_capacity(zip.len());
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(not_a_zip)?;
        let name = entry.name().map_err(not_a_zip)?.into_owned();
        let kind = if entry.is_symlink() {
            EntryKind::Symlink
        } else if entry.unix_mode().is_some_and(|mode| !matches!(mode & 0o170_000, 0 | 0o100_000 | 0o040_000)) {
            EntryKind::Special
        } else if entry.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        entries.push(ZipEntry { name, size: entry.size(), kind, encrypted: entry.encrypted() });
    }
    Ok(entries)
}

pub(crate) fn read_manifest<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Vec<u8>, ImportProblem> {
    let mut entry = match zip.by_name(MANIFEST) {
        Ok(entry) => entry,
        Err(ZipError::FileNotFound) => return Err(ImportProblem::NoManifest),
        Err(e) => return Err(ImportProblem::NotAZip(e.to_string())),
    };
    let mut bytes = Vec::new();
    entry.by_ref().take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ImportProblem::BadManifest("it is too large".to_owned()));
    }
    Ok(bytes)
}

/// Checks a zip and gives what its manifest declares.
fn inspect<R: Read + Seek>(reader: R, limits: Limits) -> Result<Declared, ImportProblem> {
    let mut zip = ZipArchive::new(reader).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    validate_entries(&entries_of(&mut zip, limits)?, limits)?;
    declared_package(&read_manifest(&mut zip)?)
}

fn read_bounded(source: &Path, limit: u64) -> Result<Vec<u8>, ImportProblem> {
    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(ImportProblem::TooLarge { entry: None, limit_mib: limit / MIB });
    }
    Ok(bytes)
}

/// Writes `bytes` to `target` through a file beside it that no scan takes for a package.
fn write_replacing(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = target.parent().ok_or_else(|| std::io::Error::other("the target has no folder"))?;
    fs::create_dir_all(folder)?;
    let mut temporary_name = target.file_name().unwrap_or_default().to_os_string();
    temporary_name.push(format!(".importing-{}", std::process::id()));
    let temporary = folder.join(temporary_name);
    let written = (|| {
        let mut output = File::create(&temporary)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Checks the zip at `source` and installs it under `user_root`. The checks and the install use the
/// same bytes, so a file changed meanwhile cannot get past them.
pub fn install_zip(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let size = fs::metadata(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?.len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(ImportProblem::TooLarge { entry: None, limit_mib: MAX_ARCHIVE_BYTES / MIB });
    }
    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    // A large zip may well be an audio pack, which is told apart without reading it whole.
    match inspect(BufReader::new(file), ARCHIVE_LIMITS)? {
        Declared::Audio(_) => return install_audio_pack(source, user_root, overwrite),
        Declared::Sticker => return install_sticker_pack(source, user_root, overwrite),
        Declared::ClipAnimation => return install_clip_animation_pack(source, user_root, overwrite),
        Declared::Shader(_) => {}
    }
    let bytes = read_bounded(source, MAX_SHADER_ARCHIVE_BYTES)?;
    let Declared::Shader(id) = inspect(Cursor::new(bytes.as_slice()), SHADER_LIMITS)? else { return Err(ImportProblem::BadManifest("it changed while it was being read".to_owned())) };
    let info = read_zip_package_info(&bytes).map_err(ImportProblem::BadPackage)?;
    let id = id.unwrap_or_default();
    if !is_safe_id(&id) {
        return Err(ImportProblem::BadId(id));
    }
    let Some(kind) = classify(&info) else { return Err(ImportProblem::NotUsable { inputs: info.inputs }) };
    // Ids that differ only in case are one package, as Windows file names are.
    let id = id.to_ascii_lowercase();
    let file_name = format!("{id}.zip");
    let target = user_root.join("packages").join("gpufx").join(USER_GROUP).join(&file_name);
    let replaced = target.exists();
    if replaced {
        // An installed zip that cannot be read does not stop its replacement.
        let installed = read_bounded(&target, MAX_SHADER_ARCHIVE_BYTES).ok().and_then(|existing| read_zip_package_info(&existing).ok());
        if let Some(existing_kind) = installed.as_ref().and_then(classify) {
            if existing_kind != kind {
                return Err(ImportProblem::KindChanged { existing: existing_kind, new: kind });
            }
        }
        if !overwrite {
            return Ok(ImportOutcome::AlreadyInstalled { name: installed.map_or(info.name, |installed| installed.name) });
        }
    }
    write_replacing(&target, &bytes).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    Ok(ImportOutcome::Installed(InstalledPackage { package: format!("packages/gpufx/{USER_GROUP}/{file_name}"), name: info.name, kind, replaced }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    fn file(name: &str) -> ZipEntry {
        ZipEntry { name: name.to_owned(), size: 1, kind: EntryKind::File, encrypted: false }
    }

    fn refused_for(name: &str) -> EntryProblem {
        match validate_entries(&[file(name)], SHADER_LIMITS) {
            Err(ImportProblem::Entry { problem, .. }) => problem,
            other => panic!("{name}: {other:?}"),
        }
    }

    #[test]
    fn ordinary_paths_are_accepted() {
        let entries = [file("manifest.json"), file("shaders/main.wgsl"), file("shaders\\windows.wgsl"), file(".hidden"), file("console/a.txt")];
        validate_entries(&entries, SHADER_LIMITS).unwrap();
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused() {
        for name in ["../evil", "a/../../evil", "..\\evil", "/etc/passwd", "\\windows\\system32", "a//b", "a/./b", ".", ".."] {
            assert_eq!(refused_for(name), EntryProblem::UnsafePath, "{name}");
        }
    }

    #[test]
    fn a_colon_is_refused_for_streams_and_drives() {
        for name in ["a.txt:stream", "C:\\evil", "C:evil"] {
            assert_eq!(refused_for(name), EntryProblem::Colon, "{name}");
        }
    }

    #[test]
    fn windows_device_names_are_refused_with_any_case_or_extension() {
        for name in ["CON", "con.txt", "a/Nul", "AUX.tar.gz", "prn", "COM1", "lpt9.png", "NUL .txt", "CONIN$"] {
            assert_eq!(refused_for(name), EntryProblem::DeviceName, "{name}");
        }
        assert!(validate_entries(&[file("console"), file("com10"), file("auxiliary.txt"), file("nullable")], SHADER_LIMITS).is_ok(), "names that only begin like one");
    }

    #[test]
    fn a_trailing_dot_or_space_is_refused_in_any_part() {
        for name in ["a.", "a ", "folder./b", "folder /b"] {
            assert_eq!(refused_for(name), EntryProblem::TrailingDotOrSpace, "{name}");
        }
    }

    #[test]
    fn other_characters_windows_rejects_are_refused() {
        for name in ["a?b", "a*b", "a<b", "a>b", "a|b", "a\"b", "a\u{1}b"] {
            assert_eq!(refused_for(name), EntryProblem::InvalidCharacter, "{name:?}");
        }
    }

    #[test]
    fn links_special_files_and_encryption_are_refused() {
        let with = |kind, encrypted| vec![ZipEntry { name: "a".to_owned(), size: 1, kind, encrypted }];
        assert_eq!(validate_entries(&with(EntryKind::Symlink, false), SHADER_LIMITS), Err(ImportProblem::Entry { entry: "a".to_owned(), problem: EntryProblem::Symlink }));
        assert_eq!(validate_entries(&with(EntryKind::Special, false), SHADER_LIMITS), Err(ImportProblem::Entry { entry: "a".to_owned(), problem: EntryProblem::NotAFile }));
        assert_eq!(validate_entries(&with(EntryKind::File, true), SHADER_LIMITS), Err(ImportProblem::Entry { entry: "a".to_owned(), problem: EntryProblem::Encrypted }));
    }

    #[test]
    fn names_that_differ_only_in_case_are_one_file_and_refused() {
        let duplicate = |entries: &[ZipEntry]| matches!(validate_entries(entries, SHADER_LIMITS), Err(ImportProblem::Entry { problem: EntryProblem::Duplicate, .. }));
        assert!(duplicate(&[file("a.wgsl"), file("A.WGSL")]));
        assert!(duplicate(&[file("a"), file("a")]), "the same name twice");
        assert!(duplicate(&[file("Shaders/a"), file("shaders/b")]), "a folder spelled two ways");
        assert!(duplicate(&[file("a"), file("a/b")]), "a file and a folder");
        assert!(duplicate(&[file("a/b"), file("A")]), "a folder and a file spelled differently");
        let directory = ZipEntry { name: "a/".to_owned(), size: 0, kind: EntryKind::Directory, encrypted: false };
        validate_entries(&[directory.clone(), directory, file("a/b"), file("a/c")], SHADER_LIMITS).unwrap();
    }

    #[test]
    fn limits_are_checked_on_count_total_and_each_file() {
        let limits = Limits { files: 2, total_bytes: 10 * MIB, file_bytes: 8 * MIB };
        let sized = |name: &str, size| ZipEntry { name: name.to_owned(), size, kind: EntryKind::File, encrypted: false };
        assert_eq!(validate_entries(&[file("a"), file("b"), file("c")], limits), Err(ImportProblem::TooManyFiles { limit: 2 }));
        assert_eq!(validate_entries(&[sized("a", 9 * MIB)], limits), Err(ImportProblem::TooLarge { entry: Some("a".to_owned()), limit_mib: 8 }));
        assert_eq!(validate_entries(&[sized("a", 8 * MIB), sized("b", 3 * MIB)], limits), Err(ImportProblem::TooLarge { entry: None, limit_mib: 10 }));
        validate_entries(&[sized("a", 8 * MIB), sized("b", 2 * MIB)], limits).unwrap();
    }

    #[test]
    fn the_type_decides_before_anything_else_is_read() {
        assert_eq!(declared_package(br#"{"id":"a.b"}"#), Ok(Declared::Shader(Some("a.b".to_owned()))));
        for kind in NON_SHADER_TYPES {
            let manifest = format!(r#"{{"type":"{kind}"}}"#);
            assert_eq!(declared_package(manifest.as_bytes()), Err(ImportProblem::NotSupportedYet(kind.to_owned())));
        }
        assert_eq!(declared_package(br#"{"type":"sfx"}"#), Ok(Declared::Audio(AudioPackKind::Sfx)));
        assert_eq!(declared_package(br#"{"type":"clip-animation"}"#), Ok(Declared::ClipAnimation));
        assert_eq!(declared_package(br#"{"type":"text-animation"}"#), Ok(Declared::ClipAnimation));
        assert_eq!(declared_package(br#"{"type":"music"}"#), Ok(Declared::Audio(AudioPackKind::Music)));
        assert_eq!(declared_package(br#"{"type":"effect"}"#), Err(ImportProblem::UnknownType("effect".to_owned())), "a shader package has no type");
        assert!(matches!(declared_package(br#"{"type":3}"#), Err(ImportProblem::BadManifest(_))));
        assert!(matches!(declared_package(b"nope"), Err(ImportProblem::BadManifest(_))));
        assert!(matches!(declared_package(b"[]"), Err(ImportProblem::BadManifest(_))));
    }

    #[test]
    fn an_id_must_be_a_safe_file_name() {
        for id in ["", "../evil", "a/b", "a\\b", "con", "NUL", "a b", "a.", "."] {
            assert!(!is_safe_id(id), "{id}");
        }
        assert!(is_safe_id("io.github.tqk2811.box-blur"));
        assert!(!is_safe_id(&"a".repeat(d3d12_backend::MAX_ASSET_ID_LENGTH + 1)));
    }

    /// `(name, content)`: a name ending in `/` is a directory.
    fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in files {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(content).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn manifest(id: &str) -> Vec<u8> {
        format!(r#"{{"id":"{id}","name":"My Effect","used_for":["gpufx"],"groups":[{{"passes":[{{"shader":"a.wgsl"}}]}}]}}"#).into_bytes()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn written(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn installed_files(root: &Path) -> Vec<String> {
        let folder = root.join("packages/gpufx").join(USER_GROUP);
        let Ok(entries) = fs::read_dir(folder) else { return Vec::new() };
        entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()
    }

    fn transition_manifest(id: &str) -> Vec<u8> {
        format!(r#"{{"id":"{id}","name":"My Transition","used_for":["gpufx"],"inputs":[{{"name":"first"}},{{"name":"second","same_size":true}}],"params":[{{"name":"progress","type":"float","default":0.5,"min":0,"max":1}}],"groups":[{{"passes":[{{"shader":"a.wgsl"}}]}}]}}"#).into_bytes()
    }

    fn effect_zip(shader: &[u8]) -> Vec<u8> {
        zip_bytes(&[("manifest.json", &manifest("t.demo")), ("a.wgsl", shader)])
    }

    #[test]
    fn a_shader_zip_is_installed_under_its_manifest_id() {
        let dir = scratch("ok");
        let root = dir.join("root");
        let outcome = install_zip(&written(&dir, &effect_zip(b"fn main() {}")), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::Installed(InstalledPackage { package: "packages/gpufx/user/t.demo.zip".to_owned(), name: "My Effect".to_owned(), kind: PackageKind::Effect, replaced: false }));
        assert_eq!(installed_files(&root), vec!["t.demo.zip"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_already_installed_asks_first_and_overwrites_when_allowed() {
        let dir = scratch("overwrite");
        let root = dir.join("root");
        install_zip(&written(&dir, &effect_zip(b"fn first() {}")), &root, false).unwrap();
        let second = effect_zip(b"fn second() {}");
        let source = written(&dir, &second);
        assert_eq!(install_zip(&source, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "My Effect".to_owned() }));
        let target = root.join("packages/gpufx/user/t.demo.zip");
        assert_ne!(fs::read(&target).unwrap(), second, "nothing is replaced before the user agrees");
        assert!(matches!(install_zip(&source, &root, true), Ok(ImportOutcome::Installed(InstalledPackage { replaced: true, .. }))));
        assert_eq!(fs::read(&target).unwrap(), second);
        assert_eq!(installed_files(&root), vec!["t.demo.zip"], "no temporary file stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_file_inside_a_zip_is_within_the_archive_limits() {
        let entry = ZipEntry { name: "model.bin".to_owned(), size: 200 * MIB, kind: EntryKind::File, encrypted: false };
        validate_entries(&[entry], ARCHIVE_LIMITS).unwrap();
    }

    #[test]
    fn replacing_with_another_kind_is_refused_even_when_overwriting() {
        let dir = scratch("kind");
        let root = dir.join("root");
        install_zip(&written(&dir, &effect_zip(b"fn first() {}")), &root, false).unwrap();
        let target = root.join("packages/gpufx/user/t.demo.zip");
        let before = fs::read(&target).unwrap();
        let transition = zip_bytes(&[("manifest.json", &transition_manifest("t.demo")), ("a.wgsl", b"fn t() {}")]);
        let source = written(&dir, &transition);
        let expected = Err(ImportProblem::KindChanged { existing: PackageKind::Effect, new: PackageKind::Transition });
        assert_eq!(install_zip(&source, &root, false), expected);
        assert_eq!(install_zip(&source, &root, true), expected);
        assert_eq!(fs::read(&target).unwrap(), before, "nothing is replaced");
        fs::write(&target, b"not a zip").unwrap();
        assert!(matches!(install_zip(&source, &root, true), Ok(ImportOutcome::Installed(InstalledPackage { replaced: true, .. }))), "an unreadable installed zip may be replaced");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_prompt_names_the_installed_package_and_ids_differing_in_case_are_one_package() {
        let dir = scratch("case");
        let root = dir.join("root");
        let named = |id: &str, name: &str| {
            let manifest = format!(r#"{{"id":"{id}","name":"{name}","used_for":["gpufx"],"groups":[{{"passes":[{{"shader":"a.wgsl"}}]}}]}}"#);
            zip_bytes(&[("manifest.json", manifest.as_bytes()), ("a.wgsl", b"fn main() {}")])
        };
        let first = install_zip(&written(&dir, &named("T.Demo", "Old Name")), &root, false).unwrap();
        assert!(matches!(&first, ImportOutcome::Installed(InstalledPackage { package, .. }) if package == "packages/gpufx/user/t.demo.zip"), "{first:?}");
        assert_eq!(installed_files(&root), vec!["t.demo.zip"]);
        let outcome = install_zip(&written(&dir, &named("t.DEMO", "New Name")), &root, false);
        assert_eq!(outcome, Ok(ImportOutcome::AlreadyInstalled { name: "Old Name".to_owned() }), "the name already installed, found through another case");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_zip_with_an_escaping_name_or_a_symlink_installs_nothing() {
        let dir = scratch("bad");
        let root = dir.join("root");
        let escaping = zip_bytes(&[("manifest.json", &manifest("t.demo")), ("a.wgsl", b"x"), ("../evil.wgsl", b"x")]);
        assert!(matches!(install_zip(&written(&dir, &escaping), &root, false), Err(ImportProblem::Entry { problem: EntryProblem::UnsafePath, .. })));
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        writer.start_file("manifest.json", options).unwrap();
        writer.write_all(&manifest("t.demo")).unwrap();
        writer.add_symlink("link", "../outside", options).unwrap();
        let linking = writer.finish().unwrap().into_inner();
        assert!(matches!(install_zip(&written(&dir, &linking), &root, false), Err(ImportProblem::Entry { problem: EntryProblem::Symlink, .. })));
        assert!(installed_files(&root).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_known_but_unsupported_type_installs_nothing_and_a_bad_zip_says_so() {
        let dir = scratch("unsupported");
        let root = dir.join("root");
        let font = zip_bytes(&[("manifest.json", br#"{"type":"font","name":"Sans"}"#), ("sans.ttf", b"x")]);
        assert_eq!(install_zip(&written(&dir, &font), &root, false), Err(ImportProblem::NotSupportedYet("font".to_owned())));
        let sticker = zip_bytes(&[("manifest.json", br#"{"type":"sticker","name":"Smile"}"#), ("smile.png", b"x")]);
        assert!(matches!(install_zip(&written(&dir, &sticker), &root, false), Err(ImportProblem::BadManifest(_))), "a sticker pack goes to its own, stricter reader");
        let audio = zip_bytes(&[("manifest.json", br#"{"type":"sfx","name":"Bells"}"#), ("bell.flac", b"x")]);
        assert!(matches!(install_zip(&written(&dir, &audio), &root, false), Err(ImportProblem::BadManifest(_))), "an audio pack goes to its own, stricter reader");
        let unknown = zip_bytes(&[("manifest.json", br#"{"type":"hologram"}"#)]);
        assert_eq!(install_zip(&written(&dir, &unknown), &root, false), Err(ImportProblem::UnknownType("hologram".to_owned())));
        let bare = zip_bytes(&[("readme.txt", b"x")]);
        assert_eq!(install_zip(&written(&dir, &bare), &root, false), Err(ImportProblem::NoManifest));
        assert!(matches!(install_zip(&written(&dir, b"not a zip at all"), &root, false), Err(ImportProblem::NotAZip(_))));
        let wrong_id = zip_bytes(&[("manifest.json", &manifest("CON")), ("a.wgsl", b"x")]);
        assert_eq!(install_zip(&written(&dir, &wrong_id), &root, false), Err(ImportProblem::BadId("CON".to_owned())));
        let broken_shader = zip_bytes(&[("manifest.json", &manifest("t.demo"))]);
        assert!(matches!(install_zip(&written(&dir, &broken_shader), &root, false), Err(ImportProblem::BadPackage(_))), "a pass names a shader the zip does not have");
        assert!(installed_files(&root).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    fn shipped(name: &str) -> Vec<u8> {
        let folder = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter/packages/gpufx")).join(name);
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut stack = vec![folder.clone()];
        while let Some(current) = stack.pop() {
            for entry in fs::read_dir(&current).unwrap_or_else(|e| panic!("{}: {e}", current.display())).flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let relative = path.strip_prefix(&folder).unwrap().to_string_lossy().replace('\\', "/");
                    files.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())).collect();
        zip_bytes(&borrowed)
    }

    #[test]
    fn a_shipped_transition_and_effect_land_in_their_own_list() {
        let dir = scratch("shipped");
        let root = dir.join("root");
        for (name, kind) in [("ffmpeg/invert", PackageKind::Effect), ("ffmpeg/fade-grays", PackageKind::Transition)] {
            match install_zip(&written(&dir, &shipped(name)), &root, false).unwrap() {
                ImportOutcome::Installed(installed) => assert_eq!(installed.kind, kind, "{name}"),
                other => panic!("{other:?}"),
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
