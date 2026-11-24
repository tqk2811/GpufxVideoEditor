//! The installed packs of every type, for the package manager window and the agent. No window here.
//!
//! Each type keeps its own reader: `list_installed` of its `*_install` module, and
//! [`crate::packages::list_installed`] for shader packages. This module only gathers what they
//! read, and turns packs off, removes them and copies the packs of a project next to it.
//!
//! Merging the per-type install modules into one generic module was considered. The user chose to
//! keep one module per type, since each type checks its manifest and its files in its own way.
//!
//! A pack is known by its key, its path under `packages/` (see [`d3d12_backend::pack_key`]):
//! `sticker/my-pack`, `gpufx/ffmpeg/box-blur`, `gpufx/user/io.example.iris.zip`.
//!
//! The copy saved with a project lives in `<folder>/<project stem>.bundle/packages/<key>` (see [`crate::project_bundle`]), laid
//! out as a package root, so restoring a pack is copying it to the same key under the user's root.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use d3d12_backend::{check_path_part, pack_key, pack_key_of, parse_asset_source, read_disabled_packs, PackageInfo, ProjectPaths, DISABLED_PACKS_FILE, FONT_ASSET_TYPE};
use engine::{FontRef, Project};

use crate::asset_install::{lock_installs, rename_with_retries, AudioPackKind, ListedPack};
use crate::clip_animation_install::CLIP_ANIMATION_TYPE;
use crate::font_install::FONT_TYPE;
use crate::i18n::pack::{Localized, PackCollection, PackTranslations};
use crate::packages::{classify, PackageKind};
use crate::sticker_install::STICKER_TYPE;
use crate::subtitle_style_install::SUBTITLE_STYLE_TYPE;
use crate::text_style_install::TEXT_STYLE_TYPE;

/// The folder under `packages` that a removed pack is moved to before it is deleted.
const REMOVING_FOLDER: &str = ".remove-tmp";
/// The folder under `packages` that a restored pack is copied to before it is renamed into place.
const RESTORING_FOLDER: &str = ".restore-tmp";
/// What is added to the stem of a project file to name the folder of its pack copies.
pub const BACKUP_SUFFIX: &str = ".bundle";
/// The folder of shader packages under `packages`.
const GPUFX_FOLDER: &str = "gpufx";

/// The tabs of the package manager, one per kind of pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum PackTab {
    #[default]
    Effect,
    Transition,
    TextStyle,
    SubtitleStyle,
    ClipAnimation,
    Sticker,
    Sfx,
    Music,
    Font,
}

impl PackTab {
    pub const ALL: [PackTab; 9] = [Self::Effect, Self::Transition, Self::TextStyle, Self::SubtitleStyle, Self::ClipAnimation, Self::Sticker, Self::Sfx, Self::Music, Self::Font];

    /// The name the agent uses for the tab.
    pub fn code(self) -> &'static str {
        match self {
            Self::Effect => "effect",
            Self::Transition => "transition",
            Self::TextStyle => TEXT_STYLE_TYPE,
            Self::SubtitleStyle => SUBTITLE_STYLE_TYPE,
            Self::ClipAnimation => CLIP_ANIMATION_TYPE,
            Self::Sticker => STICKER_TYPE,
            Self::Sfx => AudioPackKind::Sfx.name(),
            Self::Music => AudioPackKind::Music.name(),
            Self::Font => FONT_TYPE,
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.code() == code)
    }
}

/// Where a pack was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSource {
    /// A root that comes with the app (next to the executable, or the debug packages): read only.
    Shipped,
    /// The user's root under `%LOCALAPPDATA%`, where packs are installed and removed.
    User,
}

/// One installed pack as the package manager shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPack {
    pub tab: PackTab,
    /// The pack's path under `packages/`, lower-cased; see the module documentation.
    pub key: String,
    /// The folder name of the pack, or the shader package path under `packages/gpufx`.
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: PackSource,
    pub path: PathBuf,
    /// How many items it offers: sounds, stickers, styles, fonts; a shader package is one.
    pub items: usize,
    /// Its size on disk; `None` until it is measured (see [`pack_sizes`]), after the list is shown.
    pub bytes: Option<u64>,
    pub enabled: bool,
    /// A shader package kept out of the pickers, used by other packs.
    pub hidden: bool,
    /// Why it cannot be used, when it cannot.
    pub problem: Option<String>,
    /// A root searched earlier holds a pack of the same key, which is the one used.
    pub shadowed: bool,
    /// The English search tags of a wrapped shader package's `manifest.json`; empty for other packs.
    pub tags: Vec<String>,
    /// The pack's name, description and tags in the languages it brings (`i18n/<code>.json`).
    pub i18n: Localized,
    /// The collection the package manager groups it under (see [`PackCollection`]).
    pub collection: PackCollection,
}

impl InstalledPack {
    /// Only what the user installed can be removed; what comes with the app is read only.
    pub fn removable(&self) -> bool {
        self.source == PackSource::User
    }

    /// The name in language `code`, else the manifest's.
    pub fn display_name(&self, code: &str) -> &str {
        self.i18n.name(code, &self.name)
    }
}

/// Every installed pack of every root, turned off or not, sorted by tab and name.
pub fn list_installed(roots: &[PathBuf], user_root: Option<&Path>) -> Vec<InstalledPack> {
    list_installed_with(roots, user_root, d3d12_backend::read_package_info)
}

/// [`list_installed`] with `read` opening a shader package.
pub fn list_installed_with(roots: &[PathBuf], user_root: Option<&Path>, read: impl Fn(&Path) -> Result<PackageInfo, String>) -> Vec<InstalledPack> {
    let disabled = read_disabled_packs(roots);
    let mut packs = Vec::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        let source = if user_root.is_some_and(|user| d3d12_backend::same_file(user, root) || user == root) { PackSource::User } else { PackSource::Shipped };
        let mut found = Vec::new();
        for shader in crate::packages::list_installed(root, &read) {
            let key = pack_key_of(root, &shader.path).unwrap_or_else(|| pack_key(&shader.package));
            let id = shader.package.strip_prefix("packages/gpufx/").unwrap_or(&shader.package).to_owned();
            let pack = match shader.read {
                Ok(info) => {
                    let (tab, problem) = match classify(&info) {
                        Some(PackageKind::Transition) => (PackTab::Transition, None),
                        Some(PackageKind::Effect) => (PackTab::Effect, None),
                        None => (PackTab::Effect, Some(format!("it takes {} inputs, which no clip can give it", info.inputs))),
                    };
                    InstalledPack { tab, key, id, name: info.name, version: info.version, source, path: shader.path, items: 1, bytes: None, enabled: true, hidden: info.hidden, problem, shadowed: false, tags: Vec::new(), i18n: Localized::default(), collection: PackCollection::default() }
                }
                Err(message) => InstalledPack { tab: PackTab::Effect, name: id.clone(), key, id, version: String::new(), source, path: shader.path, items: 0, bytes: None, enabled: true, hidden: false, problem: Some(message), shadowed: false, tags: Vec::new(), i18n: Localized::default(), collection: PackCollection::default() },
            };
            found.push(pack);
        }
        let folder_types: [(PackTab, &str, Vec<ListedPack>); 7] = [
            (PackTab::TextStyle, TEXT_STYLE_TYPE, crate::text_style_install::list_installed(root)),
            (PackTab::SubtitleStyle, SUBTITLE_STYLE_TYPE, crate::subtitle_style_install::list_installed(root)),
            (PackTab::ClipAnimation, CLIP_ANIMATION_TYPE, crate::clip_animation_install::list_installed(root)),
            (PackTab::Sticker, STICKER_TYPE, crate::sticker_install::list_installed(root)),
            (PackTab::Sfx, AudioPackKind::Sfx.name(), crate::asset_install::list_installed(root, AudioPackKind::Sfx)),
            (PackTab::Music, AudioPackKind::Music.name(), crate::asset_install::list_installed(root, AudioPackKind::Music)),
            (PackTab::Font, FONT_TYPE, crate::font_install::list_installed(root)),
        ];
        for (tab, type_folder, listed) in folder_types {
            for pack in listed {
                let key = pack_key(&format!("{type_folder}/{}", pack.id));
                found.push(match pack.read {
                    Ok(summary) => InstalledPack { tab, key, id: pack.id, name: summary.name, version: summary.version, source, path: pack.folder, items: summary.items, bytes: None, enabled: true, hidden: false, problem: None, shadowed: false, tags: Vec::new(), i18n: Localized::default(), collection: PackCollection::default() },
                    Err(message) => InstalledPack { tab, key, name: pack.id.clone(), id: pack.id, version: String::new(), source, path: pack.folder, items: 0, bytes: None, enabled: true, hidden: false, problem: Some(message), shadowed: false, tags: Vec::new(), i18n: Localized::default(), collection: PackCollection::default() },
                });
            }
        }
        for mut pack in found {
            let translations = PackTranslations::read_package(&pack.path);
            pack.i18n = translations.pack();
            if is_shader_key(&pack.key) {
                pack.tags = d3d12_backend::read_wrapper(&pack.path).and_then(Result::ok).map(|wrapper| wrapper.tags).unwrap_or_default();
            }
            // A content pack holds one item; its manifest names the set it was cut from.
            pack.collection = if pack.path.is_dir() && !is_shader_key(&pack.key) {
                PackCollection::read_folder(&pack.path, &pack.id, &pack.name, &translations)
            } else {
                PackCollection { id: pack.id.to_ascii_lowercase(), name: pack.name.clone(), i18n: pack.i18n.clone() }
            };
            pack.enabled = !disabled.contains(&pack.key);
            pack.shadowed = !seen.insert(pack.key.clone());
            packs.push(pack);
        }
    }
    packs.sort_by_cached_key(|pack| (pack.tab, pack.collection.name.to_lowercase(), pack.collection.id.clone(), pack.name.to_lowercase(), pack.shadowed));
    packs
}

/// The packs of `tab` whose name (in English or any language the pack brings), id, version, tags or
/// description holds every word of `filter`, ignoring case and accents, in their order.
pub fn filter_packs<'a>(packs: &'a [InstalledPack], tab: PackTab, filter: &str) -> Vec<&'a InstalledPack> {
    let query = crate::search::Query::new(filter);
    packs.iter().filter(|pack| pack.tab == tab).filter(|pack| query.score(&pack.i18n.search(crate::search::SearchFields::new(&pack.name).tags(pack.tags.iter().map(String::as_str)).other([pack.id.as_str(), pack.version.as_str()]))).is_some()).collect()
}

/// The sizes of `paths` on disk, in their order. Slow for many packs: run it off the UI thread.
pub fn pack_sizes(paths: &[PathBuf]) -> Vec<u64> {
    paths.iter().map(|path| disk_size(path)).collect()
}

/// The size of a file, or of everything in a folder; links are not followed.
pub fn disk_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path).map(|entries| entries.flatten().map(|entry| disk_size(&entry.path())).sum()).unwrap_or(0)
}

/// The list of packs turned off, in the user's root.
pub fn disabled_file(user_root: &Path) -> PathBuf {
    user_root.join("packages").join(DISABLED_PACKS_FILE)
}

pub(crate) fn next_unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Writes `bytes` to `target` through a temporary file beside it, so a reader never sees half of it.
fn write_atomically(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = target.parent().ok_or_else(|| std::io::Error::other("the file has no folder"))?;
    fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(".{}.{}", target.file_name().unwrap_or_default().to_string_lossy(), next_unique()));
    fs::write(&temporary, bytes)?;
    rename_with_retries(&temporary, target).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

/// Turns the pack of `key` on or off for the editor, the agent and an export, in the user's
/// [`DISABLED_PACKS_FILE`]. A pack turned off in a shipped root's file stays off.
pub fn set_enabled(user_root: &Path, key: &str, enabled: bool) -> Result<(), String> {
    set_enabled_many(user_root, &[key], enabled)
}

/// [`set_enabled`] for every key of `wanted` (a whole collection), with one write of the file.
pub fn set_enabled_many(user_root: &Path, wanted: &[&str], enabled: bool) -> Result<(), String> {
    let file = disabled_file(user_root);
    let mut keys: BTreeSet<String> = fs::read(&file).ok().and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok()).unwrap_or_default().iter().map(|key| pack_key(key)).collect();
    let mut changed = false;
    for key in wanted {
        let key = pack_key(key);
        changed |= if enabled { keys.remove(&key) } else { keys.insert(key) };
    }
    if !changed {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&keys).map_err(|e| e.to_string())?;
    write_atomically(&file, &bytes).map_err(|e| e.to_string())
}

/// Removes a pack the user installed. It is first renamed into a working folder, so it leaves its
/// place at once and whole, then deleted; what cannot be deleted yet (a file still open) goes on
/// the next removal. The install lock keeps an install of another editor out meanwhile.
pub fn uninstall(roots: &[PathBuf], user_root: &Path, pack: &InstalledPack) -> Result<(), String> {
    let packages = user_root.join("packages");
    if !pack.removable() || !pack.path.starts_with(&packages) {
        return Err("the pack came with the app and cannot be removed".to_owned());
    }
    let _lock = lock_installs(&packages).map_err(|e| format!("another install is running ({e})"))?;
    let removing = packages.join(REMOVING_FOLDER);
    let _ = fs::remove_dir_all(&removing);
    fs::create_dir_all(&removing).map_err(|e| e.to_string())?;
    let moved = removing.join(next_unique());
    rename_with_retries(&pack.path, &moved).map_err(|e| format!("the pack is in use ({e})"))?;
    let _ = if moved.is_dir() { fs::remove_dir_all(&moved) } else { fs::remove_file(&moved) };
    let _ = fs::remove_dir(&removing);
    // A pack installed again later starts turned on; while another root still holds a pack of the
    // key (a shipped one the removed pack hid), the user's choice for it stays.
    if installed_path(roots, &pack.key).is_some() {
        return Ok(());
    }
    set_enabled(user_root, &pack.key, true)
}

/// Where a pack is used in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UseSite {
    Clip { track: usize, clip: usize },
    Transition { track: usize, after_clip: usize },
}

/// The key of the shader package a project names `package`: found where the project would load it
/// (turned off or not), or, when it is not installed, read from a name under `packages/`.
fn shader_key(paths: &ProjectPaths, package: &str) -> Option<String> {
    if let Some(path) = paths.package_path_even_disabled(package) {
        if let Some(key) = paths.package_roots().iter().find_map(|root| pack_key_of(root, &path)) {
            return Some(key);
        }
    }
    let inside = package.replace('\\', "/");
    let inside = inside.strip_prefix("packages/")?;
    safe_key(inside).then(|| pack_key(inside))
}

/// Whether `key` stays under the folder it is joined to: plain parts only, each one a valid name.
fn safe_key(key: &str) -> bool {
    let path = Path::new(key);
    !key.is_empty() && path.components().all(|part| matches!(part, Component::Normal(_))) && key.split(['/', '\\']).all(|part| check_path_part(part).is_ok())
}

/// Every pack `project` uses, by key, with where it is used. A text or subtitle style is copied
/// into the clip, so its pack is not needed; an effect a clip animation item names is reached
/// through the animation pack and not listed on its own. A key made from a project string that
/// could leave its folder (`../x`) is dropped, so nothing is copied, renamed or deleted for it.
pub fn pack_uses(project: &Project, paths: &ProjectPaths) -> Vec<(String, UseSite)> {
    let mut uses = Vec::new();
    for (track_index, track) in project.tracks.iter().enumerate() {
        for (clip_index, clip) in track.clips.iter().enumerate() {
            let site = UseSite::Clip { track: track_index, clip: clip_index };
            if let Ok(asset) = parse_asset_source(&clip.source) {
                uses.push((pack_key(&format!("{}/{}", asset.kind, asset.id)), site));
            }
            if let Some(FontRef::Pack { pack, .. }) = clip.text.as_ref().map(|text| &text.font) {
                uses.push((pack_key(&format!("{FONT_ASSET_TYPE}/{pack}")), site));
            }
            for effect in &clip.effects {
                if let Some(key) = shader_key(paths, &effect.package) {
                    uses.push((key, site));
                }
            }
            for animation in [&clip.animation.enter, &clip.animation.exit, &clip.animation.repeat].into_iter().flatten() {
                if let Some((pack, _)) = animation.preset.split_once('/') {
                    uses.push((pack_key(&format!("{CLIP_ANIMATION_TYPE}/{pack}")), site));
                }
            }
        }
        for transition in &track.transitions {
            if let Some(key) = shader_key(paths, &transition.package) {
                uses.push((key, UseSite::Transition { track: track_index, after_clip: transition.after_clip }));
            }
        }
    }
    uses.retain(|(key, _)| safe_key(key));
    uses.sort();
    uses.dedup();
    uses
}

/// Where `project` uses the pack of `key`.
pub fn uses_of(project: &Project, paths: &ProjectPaths, key: &str) -> Vec<UseSite> {
    pack_uses(project, paths).into_iter().filter(|(used, _)| used == key).map(|(_, site)| site).collect()
}

/// The keys of the packs `project` uses, each once.
fn used_keys(project: &Project, paths: &ProjectPaths) -> BTreeSet<String> {
    pack_uses(project, paths).into_iter().map(|(key, _)| key).collect()
}

/// The installed pack of `key`, in the first root holding it, turned off or not.
fn installed_path(roots: &[PathBuf], key: &str) -> Option<PathBuf> {
    roots.iter().map(|root| root.join("packages").join(key)).find(|path| is_pack(path))
}

/// A folder with a manifest, or a zipped shader package.
fn is_pack(path: &Path) -> bool {
    path.join("manifest.json").is_file() || (path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
}

/// `<folder>/<project stem>.bundle`: the copies of the packs and the media a project uses.
pub fn backup_folder(project_file: &Path) -> PathBuf {
    let stem = crate::project_zip::project_stem(project_file);
    project_file.with_file_name(format!("{stem}{BACKUP_SUFFIX}"))
}

/// Hard links the file `from` at `to`, which must not exist, when both are on one volume; else
/// copies it, keeping its modified time so a later save sees the copy as up to date.
pub(crate) fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    if let Ok(modified) = fs::metadata(from).and_then(|meta| meta.modified()) {
        let _ = fs::OpenOptions::new().write(true).open(to).and_then(|file| file.set_modified(modified));
    }
    Ok(())
}

/// Copies `from`, a file or a folder, to `to`; links are skipped.
fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                copy_all(&entry.path(), &to.join(entry.file_name()))?;
            }
        }
        Ok(())
    } else if meta.is_file() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        link_or_copy(from, to)
    } else {
        Ok(())
    }
}

/// Copies `from` to `to` through `working`, a folder on the same drive, and renames it into place,
/// replacing what was at `to` only once the copy is whole.
fn copy_into_place(from: &Path, to: &Path, working: &Path) -> std::io::Result<()> {
    copy_into_place_with(from, to, working, rename_with_retries)
}

fn copy_into_place_with(from: &Path, to: &Path, working: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> std::io::Result<()> {
    fs::create_dir_all(working)?;
    let copy = working.join(next_unique());
    let old = working.join(next_unique());
    let result = (|| {
        copy_all(from, &copy)?;
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let replacing = fs::symlink_metadata(to).is_ok();
        if replacing {
            rename(to, &old)?;
        }
        if let Err(error) = rename(&copy, to) {
            if replacing {
                if let Err(back) = rename(&old, to) {
                    // The old copy is all there is now: keep it where it is and say so.
                    let _ = if copy.is_dir() { fs::remove_dir_all(&copy) } else { fs::remove_file(&copy) };
                    return Err(std::io::Error::other(format!("{error}; the old copy could not go back and is kept at {} ({back})", old.display())));
                }
            }
            return Err(error);
        }
        Ok(())
    })();
    let old_kept = result.is_err() && fs::symlink_metadata(&old).is_ok() && fs::symlink_metadata(to).is_err();
    for leftover in [&copy, &old] {
        if old_kept && leftover == &old {
            continue;
        }
        let _ = if leftover.is_dir() { fs::remove_dir_all(leftover) } else { fs::remove_file(leftover) };
    }
    let _ = fs::remove_dir(working);
    result
}

/// Copies every installed pack `project` uses next to `project_file` (see [`backup_folder`]),
/// replacing older copies. Returns how many were copied.
pub fn backup_used_packs(project: &Project, paths: &ProjectPaths, project_file: &Path) -> Result<usize, String> {
    let folder = backup_folder(project_file);
    let packages = folder.join("packages");
    let working = folder.join(".copying");
    let mut copied = 0;
    for key in used_keys(project, paths) {
        let Some(from) = installed_path(paths.package_roots(), &key) else { continue };
        let to = packages.join(&key);
        if !safe_key(&key) || !to.starts_with(&packages) || to.components().any(|part| !matches!(part, Component::Normal(_) | Component::Prefix(_) | Component::RootDir)) {
            continue;
        }
        copy_into_place(&from, &to, &working).map_err(|e| format!("copying {key} failed: {e}"))?;
        copied += 1;
    }
    Ok(copied)
}

/// The packs `project` uses that no root holds, turned off or not, but its copy next to
/// `project_file` does.
pub fn packs_to_restore(project: &Project, paths: &ProjectPaths, project_file: &Path) -> Vec<String> {
    let packages = backup_folder(project_file).join("packages");
    if !packages.is_dir() {
        return Vec::new();
    }
    used_keys(project, paths).into_iter().filter(|key| safe_key(key) && installed_path(paths.package_roots(), key).is_none() && is_pack(&packages.join(key))).collect()
}

/// Installs the copies of `keys` next to `project_file` into the user's root; a pack the user's
/// root already holds is kept. Returns how many were installed.
pub fn restore_packs(project_file: &Path, keys: &[String], user_root: &Path) -> Result<usize, String> {
    let backup = backup_folder(project_file).join("packages");
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| e.to_string())?;
    let _lock = lock_installs(&packages).map_err(|e| format!("another install is running ({e})"))?;
    let mut restored = 0;
    for key in keys {
        let from = backup.join(key);
        let to = packages.join(key);
        if !safe_key(key) || !is_pack(&from) || fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        copy_into_place(&from, &to, &packages.join(RESTORING_FOLDER)).map_err(|e| format!("installing {key} failed: {e}"))?;
        restored += 1;
    }
    Ok(restored)
}

/// The pack of `key` at `path` in `packs`, found again after the list was read anew.
pub fn find_pack<'a>(packs: &'a [InstalledPack], key: &str, path: &Path) -> Option<&'a InstalledPack> {
    packs.iter().find(|pack| pack.key == key && pack.path == path)
}

/// Whether a key names a shader package, which the effect catalog lists.
pub fn is_shader_key(key: &str) -> bool {
    key.split('/').next() == Some(GPUFX_FOLDER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Clip, Effect, Output, Time, Track, TrackKind};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-package-manager-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_sticker_pack(root: &Path, id: &str, name: &str) {
        let path = root.join("packages/sticker").join(id);
        fs::create_dir_all(path.join("images")).unwrap();
        for file in ["images/a.png", "images/b.png", "LICENSE.txt"] {
            fs::write(path.join(file), b"x").unwrap();
        }
        let manifest = format!(r#"{{"format":1,"type":"sticker","id":"{id}","name":"{name}","version":"1.2.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{{"id":"a","file":"images/a.png","name":"A","tags":["t"],"category":"c"}},{{"id":"b","file":"images/b.png","name":"B","tags":[],"category":"c"}}]}}"#);
        fs::write(path.join("manifest.json"), manifest).unwrap();
    }

    fn write_shader(root: &Path, package: &str) {
        let path = root.join("packages/gpufx").join(package);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("manifest.json"), "{}").unwrap();
    }

    fn fake_read(path: &Path) -> Result<PackageInfo, String> {
        let info = |name: &str, inputs, has_progress, hidden| PackageInfo { name: name.to_owned(), description: String::new(), version: "2.0".to_owned(), hidden, inputs, has_progress, params: Vec::new() };
        match path.file_name().unwrap().to_str().unwrap() {
            "blur" => Ok(info("Blur", 1, false, false)),
            "grow" => Ok(info("Grow", 1, true, true)),
            "wipe" => Ok(info("Wipe", 2, true, false)),
            _ => Err("bad manifest".to_owned()),
        }
    }

    fn fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = scratch(name);
        let (shipped, user) = (dir.join("shipped"), dir.join("user"));
        write_shader(&shipped, "g/blur");
        write_shader(&shipped, "g/grow");
        write_shader(&shipped, "g/wipe");
        write_shader(&user, "user/broken");
        write_sticker_pack(&shipped, "shared", "Shipped Stickers");
        write_sticker_pack(&user, "shared", "User Stickers");
        write_sticker_pack(&user, "mine", "My Stickers");
        fs::create_dir_all(user.join("packages/sticker/bad")).unwrap();
        fs::write(user.join("packages/sticker/bad/manifest.json"), "{").unwrap();
        (dir, shipped, user)
    }

    #[test]
    fn every_pack_of_every_root_is_listed_with_its_source_and_problem() {
        let (dir, shipped, user) = fixture("list");
        let packs = list_installed_with(&[shipped.clone(), user.clone()], Some(&user), fake_read);
        let find = |key: &str, source| packs.iter().find(|pack| pack.key == key && pack.source == source).unwrap_or_else(|| panic!("{key}"));
        let blur = find("gpufx/g/blur", PackSource::Shipped);
        assert_eq!((blur.tab, blur.name.as_str(), blur.version.as_str(), blur.items, blur.hidden), (PackTab::Effect, "Blur", "2.0", 1, false));
        assert!(find("gpufx/g/grow", PackSource::Shipped).hidden);
        assert_eq!(find("gpufx/g/wipe", PackSource::Shipped).tab, PackTab::Transition);
        assert!(find("gpufx/user/broken", PackSource::User).problem.is_some());
        let mine = find("sticker/mine", PackSource::User);
        assert_eq!((mine.tab, mine.name.as_str(), mine.version.as_str(), mine.items), (PackTab::Sticker, "My Stickers", "1.2.0", 2));
        assert!(disk_size(&mine.path) > 0 && mine.bytes.is_none() && mine.removable() && mine.enabled && mine.problem.is_none());
        assert!(!find("sticker/shared", PackSource::Shipped).shadowed);
        assert!(find("sticker/shared", PackSource::User).shadowed, "the shipped one is used");
        assert!(!find("sticker/shared", PackSource::Shipped).removable());
        assert!(find("sticker/bad", PackSource::User).problem.is_some(), "a broken pack is listed to be removed");
        assert_eq!(filter_packs(&packs, PackTab::Sticker, "my 1.2").len(), 1);
        assert_eq!(filter_packs(&packs, PackTab::Transition, "").len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_filter_finds_a_wrapped_package_by_its_manifest_tags() {
        let (dir, shipped, user) = fixture("wrapper-tags");
        let blur = shipped.join("packages/gpufx/g/blur");
        fs::write(blur.join("manifest.json"), r#"{"format":1,"type":"gpufx","id":"io.example.blur","version":"1.0.0","package":"blur.zip","tags":["soften"]}"#).unwrap();
        let packs = list_installed_with(&[shipped, user.clone()], Some(&user), fake_read);
        let found: Vec<&str> = filter_packs(&packs, PackTab::Effect, "soften").iter().map(|pack| pack.key.as_str()).collect();
        assert_eq!(found, ["gpufx/g/blur"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn turning_a_pack_off_and_on_again_is_kept_in_the_user_root() {
        let (dir, shipped, user) = fixture("toggle");
        let roots = [shipped.clone(), user.clone()];
        set_enabled(&user, "Sticker/Shared", false).unwrap();
        set_enabled(&user, "gpufx/g/blur", false).unwrap();
        let packs = list_installed_with(&roots, Some(&user), fake_read);
        assert!(packs.iter().filter(|pack| pack.key == "sticker/shared").all(|pack| !pack.enabled), "for the packs of every root");
        assert!(crate::sticker_library::scan_sticker_packs(&roots).packs.iter().all(|pack| pack.id != "shared"), "a pack turned off is not loaded");
        assert!(crate::packages::scan(&roots, fake_read).effects.iter().all(|entry| entry.name != "Blur"));
        set_enabled(&user, "sticker/shared", true).unwrap();
        assert_eq!(read_disabled_packs(&roots), BTreeSet::from(["gpufx/g/blur".to_owned()]).into_iter().collect());
        assert!(crate::sticker_library::scan_sticker_packs(&roots).packs.iter().any(|pack| pack.id == "shared"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_user_pack_is_removed_and_a_shipped_one_is_refused() {
        let (dir, shipped, user) = fixture("remove");
        let roots = [shipped.clone(), user.clone()];
        set_enabled(&user, "sticker/mine", false).unwrap();
        let packs = list_installed_with(&roots, Some(&user), fake_read);
        let mine = packs.iter().find(|pack| pack.key == "sticker/mine").unwrap();
        uninstall(&roots, &user, mine).unwrap();
        assert!(!mine.path.exists());
        assert!(!user.join("packages").join(REMOVING_FOLDER).exists(), "the working folder goes");
        assert!(read_disabled_packs(&roots).is_empty(), "installed again, it starts turned on");
        let shipped_pack = packs.iter().find(|pack| pack.key == "gpufx/g/blur").unwrap();
        assert!(uninstall(&roots, &user, shipped_pack).is_err());
        assert!(shipped_pack.path.exists());
        let broken = packs.iter().find(|pack| pack.key == "gpufx/user/broken").unwrap();
        uninstall(&roots, &user, broken).unwrap();
        assert!(!broken.path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    fn clip(source: &str) -> Clip {
        Clip { source: source.to_owned(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(900), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None, animation: Default::default(), denoise: Default::default(), voice: Default::default(), reverse: false, matte: Default::default(), follow: None, lock: None, censor: None }
    }

    fn project_using(effect: &str) -> Project {
        let mut sticker = clip("asset:sticker/mine/images/a.png");
        sticker.effects.push(Effect { package: effect.to_owned(), params: Default::default(), keyframes: Default::default(), region: None });
        let track = Track { kind: TrackKind::Video, clips: vec![clip("video.mp4"), sticker], transitions: Vec::new(), locked: false, hidden: false, subtitle: None };
        Project { version: engine::SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_rate: engine::FrameRate::whole(30), audio: None }, tracks: vec![track], markers: Vec::new(), beats: Default::default(), motion_tracks: Vec::new() }
    }

    #[test]
    fn the_clips_using_a_pack_are_found() {
        let (dir, shipped, user) = fixture("uses");
        let paths = ProjectPaths::new(&dir.join("p.json"), vec![shipped.clone(), user.clone()]);
        let project = project_using("packages/gpufx/g/blur");
        assert_eq!(uses_of(&project, &paths, "sticker/mine"), vec![UseSite::Clip { track: 0, clip: 1 }]);
        assert_eq!(uses_of(&project, &paths, "gpufx/g/blur"), vec![UseSite::Clip { track: 0, clip: 1 }]);
        set_enabled(&user, "gpufx/g/blur", false).unwrap();
        let paths = ProjectPaths::new(&dir.join("p.json"), vec![shipped.clone(), user.clone()]);
        assert_eq!(uses_of(&project, &paths, "gpufx/g/blur").len(), 1, "turned off, it is still used");
        assert!(uses_of(&project, &paths, "sticker/shared").is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_packs_of_a_project_are_copied_beside_it_and_installed_from_there() {
        let (dir, shipped, user) = fixture("backup");
        let project_file = dir.join("work").join("film.json");
        fs::create_dir_all(project_file.parent().unwrap()).unwrap();
        let project = project_using("packages/gpufx/g/blur");
        let paths = ProjectPaths::new(&project_file, vec![shipped.clone(), user.clone()]);
        assert_eq!(backup_used_packs(&project, &paths, &project_file).unwrap(), 2);
        let backup = dir.join("work").join("film.bundle").join("packages");
        assert!(backup.join("sticker/mine/manifest.json").is_file());
        assert!(backup.join("gpufx/g/blur/manifest.json").is_file());
        assert_eq!(backup_used_packs(&project, &paths, &project_file).unwrap(), 2, "a second save replaces the copies");
        assert!(packs_to_restore(&project, &paths, &project_file).is_empty(), "nothing is missing here");

        // Another machine: neither pack is installed.
        let other = dir.join("other");
        let other_user = other.join("user");
        fs::create_dir_all(&other_user).unwrap();
        let paths = ProjectPaths::new(&project_file, vec![other_user.clone()]);
        let missing = packs_to_restore(&project, &paths, &project_file);
        assert_eq!(missing, vec!["gpufx/g/blur".to_owned(), "sticker/mine".to_owned()]);
        assert_eq!(restore_packs(&project_file, &missing, &other_user).unwrap(), 2);
        let paths = ProjectPaths::new(&project_file, vec![other_user.clone()]);
        assert!(paths.package_path("packages/gpufx/g/blur").is_some());
        assert!(paths.asset_path("asset:sticker/mine/images/a.png").is_ok());
        assert!(packs_to_restore(&project, &paths, &project_file).is_empty());
        assert!(!other_user.join("packages").join(RESTORING_FOLDER).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_key_that_could_leave_its_folder_is_never_restored() {
        assert!(!safe_key("../evil"));
        assert!(!safe_key("gpufx/../../evil"));
        assert!(!safe_key("C:/evil"));
        assert!(!safe_key(""));
        assert!(safe_key("gpufx/user/io.example.iris.zip"));
    }

    #[test]
    fn a_pack_name_that_leaves_its_folder_is_never_copied_beside_the_project() {
        let (dir, shipped, user) = fixture("escape");
        let project_file = dir.join("work").join("film.json");
        fs::create_dir_all(project_file.parent().unwrap()).unwrap();
        // shipped/packages/font/../../../evil is dir/evil; the copy would go to dir/work/evil.
        fs::create_dir_all(dir.join("evil")).unwrap();
        fs::write(dir.join("evil/manifest.json"), "{}").unwrap();
        fs::create_dir_all(dir.join("work/evil")).unwrap();
        fs::write(dir.join("work/evil/keep.txt"), "mine").unwrap();
        let mut project = project_using("packages/gpufx/g/blur");
        let mut text = engine::TextContent::new("x", 10.0);
        text.font = engine::FontRef::Pack { pack: "../../../evil".to_owned(), file: "a.ttf".to_owned() };
        project.tracks[0].clips[0].text = Some(text);
        project.tracks[0].clips[0].animation.enter = Some(engine::AnimationRef { preset: "../x".to_owned(), duration: Time::from_ticks(30) });
        let paths = ProjectPaths::new(&project_file, vec![shipped.clone(), user.clone()]);
        assert!(pack_uses(&project, &paths).iter().all(|(key, _)| safe_key(key)), "{:?}", pack_uses(&project, &paths));
        assert_eq!(backup_used_packs(&project, &paths, &project_file).unwrap(), 2);
        assert_eq!(fs::read_to_string(dir.join("work/evil/keep.txt")).unwrap(), "mine", "a folder outside the copies is left alone");
        assert!(!dir.join("work/evil/manifest.json").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_a_hidden_user_pack_keeps_the_shipped_one_turned_off() {
        let (dir, shipped, user) = fixture("remove-shadowed");
        let roots = [shipped.clone(), user.clone()];
        set_enabled(&user, "sticker/shared", false).unwrap();
        let packs = list_installed_with(&roots, Some(&user), fake_read);
        let hidden = packs.iter().find(|pack| pack.key == "sticker/shared" && pack.source == PackSource::User).unwrap();
        uninstall(&roots, &user, hidden).unwrap();
        assert!(!hidden.path.exists());
        assert!(read_disabled_packs(&roots).contains("sticker/shared"), "the shipped pack the user turned off stays off");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_old_copy_that_cannot_go_back_is_kept() {
        let dir = scratch("keep-old");
        let (from, to, working) = (dir.join("from"), dir.join("to"), dir.join("working"));
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("manifest.json"), "new").unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("manifest.json"), "old").unwrap();
        // The first rename (to -> old) works; every later one fails.
        let calls = std::cell::Cell::new(0);
        let rename = |a: &Path, b: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 { fs::rename(a, b) } else { Err(std::io::Error::other("busy")) }
        };
        assert!(copy_into_place_with(&from, &to, &working, rename).is_err());
        let kept: Vec<_> = fs::read_dir(&working).map(|entries| entries.flatten().map(|entry| entry.path()).collect()).unwrap_or_default();
        assert!(kept.iter().any(|path| fs::read_to_string(path.join("manifest.json")).is_ok_and(|text| text == "old")), "the old copy is still there: {kept:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pack_is_found_again_by_key_and_path_in_a_new_list() {
        let (dir, shipped, user) = fixture("find");
        let roots = [shipped.clone(), user.clone()];
        let packs = list_installed_with(&roots, Some(&user), fake_read);
        let wanted = packs.iter().find(|pack| pack.key == "sticker/shared" && pack.source == PackSource::User).unwrap().clone();
        let mut reordered = packs.clone();
        reordered.reverse();
        let found = find_pack(&reordered, &wanted.key, &wanted.path).unwrap();
        assert_eq!((found.key.as_str(), &found.path, found.source), (wanted.key.as_str(), &wanted.path, PackSource::User));
        assert!(find_pack(&reordered, "sticker/gone", &wanted.path).is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
