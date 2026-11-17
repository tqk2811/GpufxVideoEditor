//! Where the project's source files and effect packages live on disk.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::{Resolver, Time, TICKS_PER_SECOND};

/// A smaller copy of a video source that the preview decodes in its place, and what it needs to
/// show it as the original would be shown.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyFile {
    pub path: PathBuf,
    /// Width over height of the ORIGINAL as shown. The proxy's own sides are rounded to even
    /// numbers, so its shape is a little off; the layout uses this one.
    pub display_aspect: Option<f64>,
    /// How times of the original map to frames of the proxy; `None` reads the proxy at the same
    /// time as the original.
    pub timing: Option<ProxyTiming>,
}

impl ProxyFile {
    /// A proxy read at the original's times with its own shape.
    pub fn plain(path: PathBuf) -> Self {
        Self { path, display_aspect: None, timing: None }
    }
}

/// The frames of a constant-rate original and of its proxy. Both count time from their own first
/// frame (the decoder subtracts the stream's start time), so only the frame step differs: the
/// proxy samples the original every `frame_duration` ticks, a whole number of ticks at or below
/// one frame of the original, so every frame of the original is in some proxy frame. Proxy frame
/// `k` holds the frame the original's decoder showed at `k * frame_duration`, found the way that
/// decoder finds it (the time rounded to the stream's time base, then the last frame not after it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyTiming {
    /// Frames per second of the original, as `num / den`.
    pub rate: (i32, i32),
    /// The time base of the original's video stream, as `num / den` seconds.
    pub time_base: (i32, i32),
    /// One frame of the proxy.
    pub frame_duration: Time,
}

impl ProxyTiming {
    /// The frame of the original, counted from its first, that its decoder shows at `ticks`.
    fn original_frame(&self, ticks: i64) -> i128 {
        let (tb_num, tb_den) = self.time_base;
        let pts = i128::from(crate::decoder::stream_pts(ticks, tb_num, tb_den, 0));
        // Frame j has pts round(j * a / b), a muxer's rounding of its exact start: the last frame
        // not after `pts` is the largest j with j * a / b < pts + 1/2, ceil((2 pts + 1) b / 2a) - 1.
        let a = i128::from(tb_den) * i128::from(self.rate.1);
        let b = i128::from(tb_num) * i128::from(self.rate.0);
        ((2 * pts + 1) * b + 2 * a - 1).div_euclid(2 * a) - 1
    }

    /// The time to ask the proxy for so it shows the frame the original shows at `time`.
    pub fn proxy_time(&self, time: Time) -> Time {
        let (num, den) = (i128::from(self.rate.0), i128::from(self.rate.1));
        let step = i128::from(self.frame_duration.ticks());
        if num <= 0 || den <= 0 || step <= 0 || self.time_base.0 <= 0 || self.time_base.1 <= 0 || time.ticks() < 0 {
            return time;
        }
        let frame = self.original_frame(time.ticks());
        // Start just before the sample at frame * den * ticks / (num * step) and walk to the
        // first sample that holds the frame; the rounding to the time base moves it by one at most.
        let mut sample = (frame * den * i128::from(TICKS_PER_SECOND) / (num * step) - 2).max(0);
        for _ in 0..8 {
            let Ok(ticks) = i64::try_from(sample * step) else { return time };
            if self.original_frame(ticks) >= frame {
                return Time::from_ticks(ticks);
            }
            sample += 1;
        }
        // Not reachable for rates the proxies are made for; a hit would show the wrong frame.
        debug_assert!(false, "proxy_time found no sample for frame {frame} at {time:?}");
        time
    }
}

/// What starts a clip source naming a file of an installed asset pack: `asset:<type>/<id>/<file>`.
pub const ASSET_PREFIX: &str = "asset:";
/// The pack types a clip source may name, each a folder under `<root>/packages`.
pub const ASSET_TYPES: [&str; 3] = ["sfx", "music", "sticker"];
/// The pack type whose files are pictures; the others are sounds.
pub const STICKER_ASSET_TYPE: &str = "sticker";
/// The extensions of the files of a sound pack a clip may play.
pub const ASSET_AUDIO_EXTENSIONS: [&str; 2] = ["flac", "mp3"];
/// The extensions of the files of a sticker pack a clip may show.
pub const ASSET_IMAGE_EXTENSIONS: [&str; 4] = engine::IMAGE_EXTENSIONS;
/// The pack type whose files are fonts a text clip may draw with (see [`ProjectPaths::font_pack_path`]).
/// It is not one of [`ASSET_TYPES`]: no clip source names a font.
pub const FONT_ASSET_TYPE: &str = "font";
/// The extensions of the files of a font pack a text may draw with.
pub const ASSET_FONT_EXTENSIONS: [&str; 3] = ["ttf", "otf", "ttc"];
/// The longest pack id, which is also a folder name.
pub const MAX_ASSET_ID_LENGTH: usize = 128;
/// What an asset source that cannot be resolved turns into: no file can have this name on Windows.
const UNRESOLVED_ASSET: &str = "<unresolved asset>";

/// What is wrong with one part of a path, as Windows would treat it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPartProblem {
    /// A `:` opens a stream of an NTFS file, or names a drive.
    Colon,
    InvalidCharacter,
    /// `CON`, `NUL`, `COM1`..., whatever follows them.
    DeviceName,
    /// Windows drops the dot or space, so the name would mean another file.
    TrailingDotOrSpace,
}

fn is_device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ').to_uppercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|rest| matches!(rest, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}"));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

/// Checks one part of a path. An empty part, `.` and `..` are the caller's.
pub fn check_path_part(part: &str) -> Result<(), PathPartProblem> {
    if part.contains(':') {
        return Err(PathPartProblem::Colon);
    }
    if part.chars().any(|c| c < ' ' || matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
        return Err(PathPartProblem::InvalidCharacter);
    }
    if part.ends_with('.') || part.ends_with(' ') {
        return Err(PathPartProblem::TrailingDotOrSpace);
    }
    if is_device_name(part) {
        return Err(PathPartProblem::DeviceName);
    }
    Ok(())
}

/// Whether `id` can be a file or folder name under a package folder.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ASSET_ID_LENGTH && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) && id != "." && id != ".." && check_path_part(id).is_ok()
}

/// Whether a clip source names a file of an asset pack rather than a file on disk.
pub fn is_asset_source(source: &str) -> bool {
    source.starts_with(ASSET_PREFIX)
}

/// The clip source of `file`, a path with forward slashes inside the pack `id` of type `kind`.
pub fn asset_source(kind: &str, id: &str, file: &str) -> String {
    format!("{ASSET_PREFIX}{kind}/{}/{file}", id.to_ascii_lowercase())
}

/// Why an `asset:` source cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetProblem {
    /// Not of the form `asset:<type>/<id>/<file>`, or with a part that could leave the pack.
    Invalid { source: String },
    UnknownType { source: String, kind: String },
    /// The file is not one a clip may play.
    NotAudio { source: String },
    /// The file is not a picture a clip may show.
    NotImage { source: String },
    /// The file is not a font a text may draw with.
    NotFont { source: String },
    /// No package root holds the pack.
    PackMissing { kind: String, id: String },
    FileMissing { kind: String, id: String, file: String },
    /// The file exists, but through a link that leads out of the pack.
    Escapes { source: String },
    /// The pack is installed, but the user turned it off (see [`read_disabled_packs`]).
    PackDisabled { kind: String, id: String },
}

/// The file under `<root>/packages` listing the packs the user turned off.
pub const DISABLED_PACKS_FILE: &str = "disabled.json";

/// The key of a pack in [`DISABLED_PACKS_FILE`]: its path under `packages/`, lower-cased, with
/// forward slashes, e.g. `sticker/my-pack` or `gpufx/user/io.example.iris.zip`.
pub fn pack_key(relative: &str) -> String {
    relative.replace('\\', "/").trim_matches('/').to_lowercase()
}

/// The key of the pack at `path`, when it is under `<root>/packages`.
pub fn pack_key_of(root: &Path, path: &Path) -> Option<String> {
    let inside = path.strip_prefix(root.join("packages")).ok()?;
    let parts: Vec<String> = inside.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| pack_key(&parts.join("/")))
}

/// The packs turned off, from the [`DISABLED_PACKS_FILE`] of every root. The editor writes it only
/// in the user's root, but it applies to the packs of every root. A file that cannot be read
/// turns nothing off.
pub fn read_disabled_packs(roots: &[PathBuf]) -> std::collections::HashSet<String> {
    let mut disabled = std::collections::HashSet::new();
    for root in roots {
        let Ok(bytes) = std::fs::read(root.join("packages").join(DISABLED_PACKS_FILE)) else { continue };
        let Ok(keys) = serde_json::from_slice::<Vec<String>>(&bytes) else { continue };
        disabled.extend(keys.iter().map(|key| pack_key(key)));
    }
    disabled
}

impl fmt::Display for AssetProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { source } => write!(f, "'{source}' is not a valid asset source"),
            Self::UnknownType { source, kind } => write!(f, "'{source}' names the unknown pack type '{kind}'"),
            Self::NotAudio { source } => write!(f, "'{source}' is not an audio file of a pack"),
            Self::NotImage { source } => write!(f, "'{source}' is not an image of a pack"),
            Self::NotFont { source } => write!(f, "'{source}' is not a font of a pack"),
            Self::PackMissing { kind, id } => write!(f, "the {kind} pack '{id}' is not installed"),
            Self::FileMissing { kind, id, file } => write!(f, "the {kind} pack '{id}' has no file '{file}'"),
            Self::Escapes { source } => write!(f, "'{source}' leads out of its pack"),
            Self::PackDisabled { kind, id } => write!(f, "the {kind} pack '{id}' is turned off"),
        }
    }
}

impl std::error::Error for AssetProblem {}

/// The parts of a valid `asset:` source. The id is lower-cased, as the folder of the pack is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRef {
    pub kind: String,
    pub id: String,
    pub file: String,
}

/// Parses an `asset:` source, refusing anything that could name a place outside the pack.
pub fn parse_asset_source(source: &str) -> Result<AssetRef, AssetProblem> {
    let invalid = || AssetProblem::Invalid { source: source.to_owned() };
    let rest = source.strip_prefix(ASSET_PREFIX).ok_or_else(invalid)?;
    if rest.contains('\\') {
        return Err(invalid());
    }
    let mut parts = rest.splitn(3, '/');
    let (Some(kind), Some(id), Some(file)) = (parts.next(), parts.next(), parts.next()) else { return Err(invalid()) };
    if !ASSET_TYPES.contains(&kind) {
        return Err(AssetProblem::UnknownType { source: source.to_owned(), kind: kind.to_owned() });
    }
    if !is_safe_pack_file(id, file) {
        return Err(invalid());
    }
    let extension = file_extension(file);
    if kind == STICKER_ASSET_TYPE {
        if !ASSET_IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            return Err(AssetProblem::NotImage { source: source.to_owned() });
        }
    } else if !ASSET_AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return Err(AssetProblem::NotAudio { source: source.to_owned() });
    }
    Ok(AssetRef { kind: kind.to_owned(), id: id.to_ascii_lowercase(), file: file.to_owned() })
}

/// Whether `id` and `file` (forward slashes) name a place inside a pack and nowhere else.
fn is_safe_pack_file(id: &str, file: &str) -> bool {
    let unsafe_part = |part: &str| part.is_empty() || part == "." || part == ".." || check_path_part(part).is_err();
    is_safe_id(id) && !file.contains('\\') &&!file.split('/').any(unsafe_part)
}

fn file_extension(file: &str) -> String {
    Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase()
}

/// The parts of `path`, lower-cased as Windows compares them.
fn lowered_parts(path: &Path) -> Vec<String> {
    path.components().map(|part| part.as_os_str().to_string_lossy().to_lowercase()).collect()
}

/// Whether `file`, once links are followed, is still inside `pack`, the folder `<type folder>/<id>`.
fn stays_in_pack(type_folder: &Path, id: &str, file: &Path) -> bool {
    let (Ok(type_folder), Ok(file)) = (type_folder.canonicalize(), file.canonicalize()) else { return false };
    let pack = lowered_parts(&type_folder.join(id));
    let file = lowered_parts(&file);
    file.len() > pack.len() && file[..pack.len()] == pack[..]
}

/// Resolves the relative paths of a project.
///
/// A source is relative to the project file, or names a file of an installed pack as
/// `asset:<type>/<id>/<file>`, looked for under `<root>/packages/<type>/<id>` of each root in
/// order. A package name is tried, in this order, as a path
/// relative to the project file, as a path relative to each package root, and, for a bare name
/// such as `mix`, as `<root>/packages/gpufx/<group>/<name>` with groups in alphabetical order. The
/// name as given is tried by all of those before, for a package that became a zip, the name with
/// `.zip` added is tried by all of them again. A package is a directory holding `manifest.json`, or
/// a `.zip` file.
pub struct ProjectPaths {
    base: PathBuf,
    package_roots: Vec<PathBuf>,
    /// Whether a package may be found by the project's folder or an absolute path outside the roots.
    project_packages: bool,
    /// Smaller copies of video sources the preview decodes instead, by the absolute path of the
    /// original (see [`Self::video_decode_path`]). Empty for an export, which reads the originals.
    proxies: Arc<HashMap<PathBuf, ProxyFile>>,
    /// The packs the user turned off, read once from the roots: none of them resolves.
    disabled: Arc<std::collections::HashSet<String>>,
}

fn is_package(path: &Path) -> bool {
    path.join("manifest.json").is_file() || (path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
}

/// The name a package has once zipped, unless it is already a zip name.
fn zip_name(package: &str) -> Option<String> {
    (!package.to_ascii_lowercase().ends_with(".zip")).then(|| format!("{package}.zip"))
}

impl ProjectPaths {
    /// `project_file` is the path of the project JSON; `package_roots` are searched in order.
    pub fn new(project_file: &Path, package_roots: Vec<PathBuf>) -> Self {
        let base = project_file.parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let disabled = Arc::new(read_disabled_packs(&package_roots));
        Self { base, package_roots, project_packages: true, proxies: Arc::default(), disabled }
    }

    /// The same paths with `proxies`: the files the preview decodes in place of the originals, by
    /// the absolute path of the original.
    pub fn with_proxies(self, proxies: Arc<HashMap<PathBuf, ProxyFile>>) -> Self {
        Self { proxies, ..self }
    }

    /// The same paths without proxies, so every source is read from its original file.
    pub fn without_proxies(&self) -> Self {
        Self { base: self.base.clone(), package_roots: self.package_roots.clone(), project_packages: self.project_packages, proxies: Arc::default(), disabled: self.disabled.clone() }
    }

    /// The package roots, in the order they are searched.
    pub fn package_roots(&self) -> &[PathBuf] {
        &self.package_roots
    }

    pub fn has_proxies(&self) -> bool {
        !self.proxies.is_empty()
    }

    /// The file a video decoder opens for `source`: its proxy when it has one, else the original.
    pub fn video_decode_path(&self, source: &str) -> PathBuf {
        self.video_decode_source(source).map_or_else(|| self.source_path(source), |proxy| proxy.path)
    }

    /// The proxy of `source`, when the preview has one.
    pub fn video_decode_source(&self, source: &str) -> Option<ProxyFile> {
        if self.proxies.is_empty() {
            return None;
        }
        let original = self.source_path(source);
        let key = std::path::absolute(&original).unwrap_or(original);
        self.proxies.get(&key).cloned()
    }

    /// Like [`Self::new`], but packages come only from the package roots. A package is shader code
    /// loaded from disk, so what an agent drives must not load one from beside a project.
    pub fn roots_only(project_file: &Path, package_roots: Vec<PathBuf>) -> Self {
        Self { project_packages: false, ..Self::new(project_file, package_roots) }
    }

    /// Whether `package` stays inside the package roots when joined to one: an absolute name or one
    /// climbing out with `..` does not, unless it already names a place under a root.
    fn inside_roots(&self, package: &str) -> bool {
        let path = Path::new(package);
        if path.is_absolute() {
            if path.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
                return false;
            }
            return self.package_roots.iter().any(|root| std::path::absolute(root).is_ok_and(|root| path.starts_with(root)));
        }
        !path.has_root() && path.components().all(|part| matches!(part, std::path::Component::Normal(_) | std::path::Component::CurDir))
    }

    /// The path of `source`. An `asset:` source that cannot be resolved gets a path where no file
    /// can be; [`Self::asset_problem`] says why.
    pub fn source_path(&self, source: &str) -> PathBuf {
        if is_asset_source(source) {
            return self.asset_path(source).unwrap_or_else(|_| self.base.join(UNRESOLVED_ASSET));
        }
        self.base.join(source)
    }

    /// The file an `asset:` source names, in the first root holding its pack.
    pub fn asset_path(&self, source: &str) -> Result<PathBuf, AssetProblem> {
        let asset = parse_asset_source(source)?;
        self.pack_file(asset, source)
    }

    /// The font file `file` of the font pack `pack`, in the first root holding the pack, checked as
    /// an `asset:` source is.
    pub fn font_pack_path(&self, pack: &str, file: &str) -> Result<PathBuf, AssetProblem> {
        let source = asset_source(FONT_ASSET_TYPE, pack, file);
        if !is_safe_pack_file(pack, file) {
            return Err(AssetProblem::Invalid { source });
        }
        if !ASSET_FONT_EXTENSIONS.contains(&file_extension(file).as_str()) {
            return Err(AssetProblem::NotFont { source });
        }
        self.pack_file(AssetRef { kind: FONT_ASSET_TYPE.to_owned(), id: pack.to_ascii_lowercase(), file: file.to_owned() }, &source)
    }

    /// `asset` looked for in the roots, in order.
    fn pack_file(&self, asset: AssetRef, source: &str) -> Result<PathBuf, AssetProblem> {
        for root in &self.package_roots {
            let type_folder = root.join("packages").join(&asset.kind);
            let pack = type_folder.join(&asset.id);
            if !pack.join("manifest.json").is_file() {
                continue;
            }
            if self.disabled.contains(&pack_key(&format!("{}/{}", asset.kind, asset.id))) {
                return Err(AssetProblem::PackDisabled { kind: asset.kind, id: asset.id });
            }
            let file = asset.file.split('/').fold(pack, |path, part| path.join(part));
            if !file.is_file() {
                return Err(AssetProblem::FileMissing { kind: asset.kind, id: asset.id, file: asset.file });
            }
            if !stays_in_pack(&type_folder, &asset.id, &file) {
                return Err(AssetProblem::Escapes { source: source.to_owned() });
            }
            return Ok(file);
        }
        Err(AssetProblem::PackMissing { kind: asset.kind, id: asset.id })
    }

    /// Why `source` cannot be used, when it is an `asset:` source. `None` for any other source.
    pub fn asset_problem(&self, source: &str) -> Option<AssetProblem> {
        is_asset_source(source).then(|| self.asset_path(source).err()).flatten()
    }

    /// The path of `package`, or `None` when it is not found or the user turned it off.
    pub fn package_path(&self, package: &str) -> Option<PathBuf> {
        let found = self.package_path_even_disabled(package)?;
        (!self.is_disabled(&found)).then_some(found)
    }

    /// The path of `package` as [`Self::package_path`] finds it, turned off or not: for telling a
    /// package that is missing from one the user turned off.
    pub fn package_path_even_disabled(&self, package: &str) -> Option<PathBuf> {
        if !self.project_packages && !self.inside_roots(package) {
            return None;
        }
        let zipped = zip_name(package);
        let direct = |name: &str| {
            let beside_project = self.project_packages.then(|| self.base.join(name));
            beside_project.into_iter().chain(self.package_roots.iter().map(|root| root.join(name))).find(|p| is_package(p))
        };
        // Only a bare name is searched for in the groups.
        let grouped = |name: &str| {
            if package.contains(['/', '\\']) {
                return None;
            }
            for root in &self.package_roots {
                let Ok(entries) = std::fs::read_dir(root.join("packages").join("gpufx")) else { continue };
                let mut groups: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
                groups.sort();
                if let Some(found) = groups.iter().map(|g| g.join(name)).find(|p| is_package(p)) {
                    return Some(found);
                }
            }
            None
        };
        // The name as given is tried everywhere before the zipped one, so turning a folder into a zip
        // changes nothing for a project whose package still exists under its old name, and a bare
        // `mix` is not taken by an unrelated `mix.zip` beside the project.
        direct(package).or_else(|| grouped(package)).or_else(|| zipped.as_deref().and_then(direct)).or_else(|| zipped.as_deref().and_then(grouped))
    }

    /// Whether the package at `path`, under one of the roots, was turned off by the user.
    pub fn is_disabled(&self, path: &Path) -> bool {
        !self.disabled.is_empty() && self.package_roots.iter().filter_map(|root| pack_key_of(root, path)).any(|key| self.disabled.contains(&key))
    }
}

impl Resolver for ProjectPaths {
    fn source_exists(&self, source: &str) -> bool {
        self.source_path(source).is_file()
    }

    fn source_problem(&self, source: &str) -> Option<String> {
        self.asset_problem(source).map(|problem| problem.to_string())
    }

    fn package_exists(&self, package: &str) -> bool {
        self.package_path(package).is_some()
    }

    /// A file FFmpeg cannot open has no audio it could read, so it counts as having none.
    fn source_has_audio(&self, source: &str) -> bool {
        crate::audio::has_audio_stream(&self.source_path(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_source_is_kept_as_it_is() {
        let paths = ProjectPaths::new(Path::new("unsaved.json"), Vec::new());
        assert_eq!(paths.source_path(r"C:\media\clip.mp4"), PathBuf::from(r"C:\media\clip.mp4"));
    }

    fn package_in(root: &Path, relative: &str) -> PathBuf {
        let folder = root.join(relative);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("manifest.json"), b"{}").unwrap();
        folder
    }

    #[test]
    fn roots_only_ignores_a_package_beside_the_project_and_any_path_out_of_the_roots() {
        let scratch = std::env::temp_dir().join(format!("gpufx-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let (project, root) = (scratch.join("project"), scratch.join("root"));
        package_in(&project, "fx");
        package_in(&root, "packages/gpufx/group/mix");
        let file = project.join("p.json");
        let open = ProjectPaths::new(&file, vec![root.clone()]);
        let agent = ProjectPaths::roots_only(&file, vec![root.clone()]);
        assert!(open.package_path("fx").is_some(), "the editor still finds it");
        assert_eq!(agent.package_path("fx"), None);
        assert_eq!(agent.package_path(project.join("fx").to_str().unwrap()), None, "an absolute path out of the roots");
        assert_eq!(agent.package_path("../project/fx"), None, "a climb out of the root");
        assert_eq!(agent.package_path(root.join("../project/fx").to_str().unwrap()), None, "an absolute name climbing out with ..");
        assert_eq!(agent.package_path("mix"), Some(root.join("packages/gpufx/group/mix")));
        assert_eq!(agent.package_path("packages/gpufx/group/mix"), Some(root.join("packages/gpufx/group/mix")));
        let absolute = root.join("packages/gpufx/group/mix");
        assert_eq!(agent.package_path(absolute.to_str().unwrap()), Some(absolute.clone()), "the renderer gets absolute names under a root");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_package_that_became_a_zip_is_found_by_its_old_name_and_a_folder_still_wins() {
        let scratch = std::env::temp_dir().join(format!("gpufx-paths-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let root = scratch.join("root");
        std::fs::create_dir_all(root.join("packages/gpufx/group")).unwrap();
        let zip = root.join("packages/gpufx/group/wipe.zip");
        std::fs::write(&zip, b"zip").unwrap();
        let paths = ProjectPaths::new(&scratch.join("p.json"), vec![root.clone()]);
        assert_eq!(paths.package_path("packages/gpufx/group/wipe"), Some(zip.clone()), "a project stores the old relative name");
        assert_eq!(paths.package_path("packages/gpufx/group/wipe.zip"), Some(zip.clone()));
        assert_eq!(paths.package_path("wipe"), Some(zip.clone()), "a bare name");
        assert_eq!(paths.package_path("wipe.zip"), Some(zip.clone()), "a bare name with its extension");
        assert_eq!(paths.package_path("other"), None);
        let agent = ProjectPaths::roots_only(&scratch.join("p.json"), vec![root.clone()]);
        assert_eq!(agent.package_path("wipe"), Some(zip.clone()), "an agent finds it through the roots");
        assert_eq!(agent.package_path("../root/packages/gpufx/group/wipe"), None, "the fallback does not open a way out");
        let folder = package_in(&root, "packages/gpufx/group/wipe");
        assert_eq!(paths.package_path("packages/gpufx/group/wipe"), Some(folder.clone()), "the name as given wins");
        assert_eq!(paths.package_path("wipe"), Some(folder));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_bare_name_finds_its_group_package_before_an_unrelated_zip_beside_the_project() {
        let scratch = std::env::temp_dir().join(format!("gpufx-paths-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let (project, root) = (scratch.join("project"), scratch.join("root"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("mix.zip"), b"zip").unwrap();
        let folder = package_in(&root, "packages/gpufx/ffmpeg/mix");
        let paths = ProjectPaths::new(&project.join("p.json"), vec![root.clone()]);
        assert_eq!(paths.package_path("mix"), Some(folder));
        assert_eq!(paths.package_path("mix.zip"), Some(project.join("mix.zip")), "the zip name still means the zip");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    fn pack_in(root: &Path, kind: &str, id: &str, files: &[&str]) -> PathBuf {
        let pack = root.join("packages").join(kind).join(id);
        std::fs::create_dir_all(pack.join("audio")).unwrap();
        std::fs::write(pack.join("manifest.json"), b"{}").unwrap();
        for file in files {
            std::fs::write(pack.join(file), b"x").unwrap();
        }
        pack
    }

    fn asset_scratch(name: &str) -> PathBuf {
        let scratch = std::env::temp_dir().join(format!("gpufx-paths-asset-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        scratch
    }

    #[test]
    fn a_font_of_a_font_pack_resolves_inside_its_pack_and_nothing_else_does() {
        let scratch = asset_scratch("font");
        let pack = pack_in(&scratch, "font", "open-fonts", &["a.ttf", "LICENSE.txt"]);
        let paths = ProjectPaths::new(&scratch.join("p.json"), vec![scratch.clone()]);
        assert_eq!(paths.font_pack_path("Open-Fonts", "a.ttf"), Ok(pack.join("a.ttf")), "the id is case-insensitive");
        assert!(matches!(paths.font_pack_path("open-fonts", "LICENSE.txt"), Err(AssetProblem::NotFont { .. })));
        assert!(matches!(paths.font_pack_path("open-fonts", "b.ttf"), Err(AssetProblem::FileMissing { .. })));
        assert!(matches!(paths.font_pack_path("other", "a.ttf"), Err(AssetProblem::PackMissing { .. })));
        for (pack, file) in [("open-fonts", "../open-fonts/a.ttf"), ("open-fonts", r"fonts\a.ttf"), ("..", "a.ttf"), ("open-fonts", "fonts/a.ttf:x.ttf")] {
            assert!(matches!(paths.font_pack_path(pack, file), Err(AssetProblem::Invalid { .. })), "{pack}/{file}");
        }
        assert!(matches!(parse_asset_source("asset:font/open-fonts/fonts/a.ttf"), Err(AssetProblem::UnknownType { .. })), "no clip source names a font");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn an_asset_source_resolves_inside_its_pack_in_any_root_and_case_of_the_id() {
        let scratch = asset_scratch("resolve");
        let (first, second) = (scratch.join("shipped"), scratch.join("user"));
        std::fs::create_dir_all(&first).unwrap();
        let pack = pack_in(&second, "sfx", "avs-sfx-impact", &["audio/boom.flac"]);
        let paths = ProjectPaths::new(&scratch.join("p.json"), vec![first, second]);
        let file = pack.join("audio").join("boom.flac");
        assert_eq!(paths.source_path("asset:sfx/avs-sfx-impact/audio/boom.flac"), file);
        assert_eq!(paths.source_path("asset:sfx/AVS-SFX-Impact/audio/boom.flac"), file, "the id is case-insensitive");
        assert!(paths.source_exists("asset:sfx/avs-sfx-impact/audio/boom.flac"));
        assert_eq!(paths.source_problem("asset:sfx/avs-sfx-impact/audio/boom.flac"), None);
        let agent = ProjectPaths::roots_only(&scratch.join("p.json"), paths.package_roots.clone());
        assert_eq!(agent.asset_path("asset:sfx/avs-sfx-impact/audio/boom.flac"), Ok(file));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn anything_that_could_leave_the_pack_is_refused_without_touching_the_disk() {
        let invalid = [
            "asset:sfx/pack/../other/a.flac",
            "asset:sfx/pack/audio/../../a.flac",
            "asset:sfx/pack/audio/./a.flac",
            "asset:sfx/../pack/a.flac",
            "asset:sfx/pack/audio//a.flac",
            "asset:sfx/pack//a.flac",
            "asset:sfx/pack/C:/Windows/a.flac",
            "asset:sfx/pack/a.flac:stream.flac",
            "asset:sfx/pack/audio\\..\\..\\a.flac",
            "asset:sfx/pack\\audio/a.flac",
            "asset:sfx/pack/CON.flac",
            "asset:sfx/pack/a .flac ",
            "asset:sfx/.../a.flac",
            "asset:sfx/pack",
            "asset:sfx",
            "asset:",
        ];
        for source in invalid {
            assert!(matches!(parse_asset_source(source), Err(AssetProblem::Invalid { .. })), "{source}: {:?}", parse_asset_source(source));
        }
        assert!(matches!(parse_asset_source("asset:gpufx/pack/a.flac"), Err(AssetProblem::UnknownType { .. })), "a shader folder is no pack");
        assert!(matches!(parse_asset_source("asset:../pack/a.flac"), Err(AssetProblem::UnknownType { .. })));
        assert!(matches!(parse_asset_source("asset:sfx/pack/manifest.json"), Err(AssetProblem::NotAudio { .. })));
        assert!(matches!(parse_asset_source("asset:sfx/pack/LICENSE.txt"), Err(AssetProblem::NotAudio { .. })));
    }

    #[test]
    fn a_sticker_source_takes_images_only_and_is_refused_the_same_way_as_a_sound() {
        assert_eq!(parse_asset_source("asset:sticker/pack/images/a.png").unwrap().file, "images/a.png");
        assert_eq!(parse_asset_source("asset:sticker/PACK/a.WebP").unwrap().id, "pack");
        for source in ["asset:sticker/pack/a.flac", "asset:sticker/pack/manifest.json"] {
            assert!(matches!(parse_asset_source(source), Err(AssetProblem::NotImage { .. })), "{source}");
        }
        assert!(matches!(parse_asset_source("asset:sfx/pack/a.png"), Err(AssetProblem::NotAudio { .. })), "a sound pack holds no images");
        for source in ["asset:sticker/pack/../a.png", "asset:sticker/../pack/a.png", "asset:sticker/pack/a.png:stream.png", "asset:sticker/pack/CON.png"] {
            assert!(matches!(parse_asset_source(source), Err(AssetProblem::Invalid { .. })), "{source}");
        }
    }

    #[test]
    fn an_unresolved_asset_never_becomes_a_file_of_the_project_folder() {
        let scratch = asset_scratch("unresolved");
        std::fs::write(scratch.join("evil.flac"), b"x").unwrap();
        let paths = ProjectPaths::new(&scratch.join("p.json"), vec![scratch.clone()]);
        assert!(!paths.source_exists("asset:sfx/../../evil.flac"));
        assert!(!paths.source_path("asset:sfx/nope/a.flac").is_file());
        assert!(!is_asset_source("audio/asset:x.flac"), "only the start of a source counts");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_missing_pack_or_file_is_named() {
        let scratch = asset_scratch("missing");
        let root = scratch.join("root");
        pack_in(&root, "music", "avs-music", &[]);
        let paths = ProjectPaths::new(&scratch.join("p.json"), vec![root]);
        let problem = paths.source_problem("asset:sfx/avs-sfx-ui/audio/a.flac").unwrap();
        assert!(problem.contains("avs-sfx-ui") && problem.contains("not installed"), "{problem}");
        assert_eq!(paths.asset_problem("asset:music/avs-music/audio/a.mp3"), Some(AssetProblem::FileMissing { kind: "music".to_owned(), id: "avs-music".to_owned(), file: "audio/a.mp3".to_owned() }));
        assert_eq!(paths.source_problem(r"C:\media\a.flac"), None, "an ordinary source is the plain resolver's");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_junction_out_of_the_pack_is_refused() {
        let scratch = asset_scratch("junction");
        let root = scratch.join("root");
        let pack = pack_in(&root, "sfx", "p", &[]);
        let outside = scratch.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.flac"), b"x").unwrap();
        let linked = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(pack.join("audio").join("link")).arg(&outside).output();
        if !linked.is_ok_and(|output| output.status.success()) {
            eprintln!("skipped: a junction could not be made");
            return;
        }
        let paths = ProjectPaths::new(&scratch.join("p.json"), vec![root]);
        assert_eq!(paths.asset_problem("asset:sfx/p/audio/link/secret.flac"), Some(AssetProblem::Escapes { source: "asset:sfx/p/audio/link/secret.flac".to_owned() }));
        assert!(!paths.source_exists("asset:sfx/p/audio/link/secret.flac"));
        std::fs::remove_dir(pack.join("audio").join("link")).unwrap();
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_relative_source_joins_the_project_folder() {
        let paths = ProjectPaths::new(Path::new(r"D:\work\project.json"), Vec::new());
        assert_eq!(paths.source_path("a.mp4"), PathBuf::from(r"D:\work\a.mp4"));
    }

    #[test]
    fn a_video_decodes_from_its_proxy_and_an_export_copy_reads_the_original() {
        let original = PathBuf::from(r"D:\work\a.mp4");
        let proxy = PathBuf::from(r"C:\cache\a-proxy.mp4");
        let map = HashMap::from([(original.clone(), ProxyFile::plain(proxy.clone()))]);
        let paths = ProjectPaths::new(Path::new(r"D:\work\p.json"), Vec::new()).with_proxies(Arc::new(map));
        assert!(paths.has_proxies());
        assert_eq!(paths.video_decode_path("a.mp4"), proxy, "relative to the project");
        assert_eq!(paths.video_decode_path(r"D:\work\a.mp4"), proxy, "absolute");
        assert_eq!(paths.video_decode_path("b.mp4"), PathBuf::from(r"D:\work\b.mp4"), "no proxy");
        assert_eq!(paths.source_path("a.mp4"), original, "the source path stays the original");
        let export = paths.without_proxies();
        assert!(!export.has_proxies());
        assert_eq!(export.video_decode_path("a.mp4"), original);
    }

    #[test]
    fn a_proxy_time_lands_on_the_proxy_frame_that_holds_the_original_frame() {
        // 23.976 fps: one frame is 3753.75 ticks; the proxy steps 3753. Time bases of 1/24000 (the
        // usual one), 1/90000 and 1/1000 (coarse, so the rounding moves frames).
        for time_base in [(1, 24000), (1, 90000), (1, 1000)] {
            let timing = ProxyTiming { rate: (24000, 1001), time_base, frame_duration: Time::from_ticks(3753) };
            for frame in [0_i64, 1, 2, 3, 100, 14_385, 100_000] {
                let start = (frame as f64 * 3753.75) as i64;
                for offset in [-2, -1, 0, 1, 2, 1800, 3750, 3752] {
                    let time = Time::from_ticks((start + offset).max(0));
                    let asked = timing.proxy_time(time).ticks();
                    assert_eq!(asked % 3753, 0, "a proxy frame start");
                    // Proxy frame `asked / 3753` holds what the original's decoder showed at `asked`.
                    assert_eq!(timing.original_frame(asked), timing.original_frame(time.ticks()), "{time_base:?}: time {} ticks asked {asked}", time.ticks());
                    assert!(asked <= time.ticks() + 3753, "{time_base:?}: never more than a proxy frame on");
                }
            }
        }
        // The case the GPU test found: at 7506 ticks the original shows its frame 2 (pts 2001.6
        // rounds to 2002), so the proxy frame there holds frame 2, not 1.
        let film = ProxyTiming { rate: (24000, 1001), time_base: (1, 24000), frame_duration: Time::from_ticks(3753) };
        assert_eq!(film.proxy_time(Time::from_ticks(6000)), Time::from_ticks(3753));
        assert_eq!(film.proxy_time(Time::from_ticks(7506)), Time::from_ticks(7506));
        // Same steps: the time is kept to the frame start.
        let even = ProxyTiming { rate: (30, 1), time_base: (1, 15360), frame_duration: Time::from_ticks(3000) };
        assert_eq!(even.proxy_time(Time::from_ticks(4500)), Time::from_ticks(3000));
        assert_eq!(even.proxy_time(Time::from_ticks(3000)), Time::from_ticks(3000));
    }

    #[test]
    fn a_disabled_pack_and_package_do_not_resolve() {
        let root = std::env::temp_dir().join(format!("gpufx-paths-disabled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("packages/sticker/pack")).unwrap();
        std::fs::write(root.join("packages/sticker/pack/manifest.json"), "{}").unwrap();
        std::fs::write(root.join("packages/sticker/pack/a.png"), "x").unwrap();
        std::fs::create_dir_all(root.join("packages/gpufx/g/blur")).unwrap();
        std::fs::write(root.join("packages/gpufx/g/blur/manifest.json"), "{}").unwrap();
        let paths = ProjectPaths::new(Path::new("p.json"), vec![root.clone()]);
        assert!(paths.asset_path("asset:sticker/pack/a.png").is_ok());
        assert!(paths.package_path("packages/gpufx/g/blur").is_some());
        std::fs::write(root.join("packages").join(DISABLED_PACKS_FILE), r#"["Sticker/Pack", "gpufx/g/blur"]"#).unwrap();
        assert_eq!(read_disabled_packs(std::slice::from_ref(&root)).len(), 2);
        let paths = ProjectPaths::new(Path::new("p.json"), vec![root.clone()]);
        assert!(matches!(paths.asset_path("asset:sticker/pack/a.png"), Err(AssetProblem::PackDisabled { .. })));
        assert_eq!(paths.package_path("packages/gpufx/g/blur"), None);
        assert_eq!(paths.package_path("blur"), None, "nor by its bare name");
        std::fs::write(root.join("packages").join(DISABLED_PACKS_FILE), "not json").unwrap();
        assert!(read_disabled_packs(std::slice::from_ref(&root)).is_empty(), "a broken file turns nothing off");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
