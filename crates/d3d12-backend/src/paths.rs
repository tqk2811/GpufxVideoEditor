//! Where the project's source files and effect packages live on disk.

use std::fmt;
use std::path::{Path, PathBuf};

use engine::Resolver;

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
    /// No package root holds the pack.
    PackMissing { kind: String, id: String },
    FileMissing { kind: String, id: String, file: String },
    /// The file exists, but through a link that leads out of the pack.
    Escapes { source: String },
}

impl fmt::Display for AssetProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { source } => write!(f, "'{source}' is not a valid asset source"),
            Self::UnknownType { source, kind } => write!(f, "'{source}' names the unknown pack type '{kind}'"),
            Self::NotAudio { source } => write!(f, "'{source}' is not an audio file of a pack"),
            Self::NotImage { source } => write!(f, "'{source}' is not an image of a pack"),
            Self::PackMissing { kind, id } => write!(f, "the {kind} pack '{id}' is not installed"),
            Self::FileMissing { kind, id, file } => write!(f, "the {kind} pack '{id}' has no file '{file}'"),
            Self::Escapes { source } => write!(f, "'{source}' leads out of its pack"),
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
    if !is_safe_id(id) {
        return Err(invalid());
    }
    let unsafe_part = |part: &str| part.is_empty() || part == "." || part == ".." || check_path_part(part).is_err();
    if file.split('/').any(unsafe_part) {
        return Err(invalid());
    }
    let extension = Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if kind == STICKER_ASSET_TYPE {
        if !ASSET_IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            return Err(AssetProblem::NotImage { source: source.to_owned() });
        }
    } else if !ASSET_AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return Err(AssetProblem::NotAudio { source: source.to_owned() });
    }
    Ok(AssetRef { kind: kind.to_owned(), id: id.to_ascii_lowercase(), file: file.to_owned() })
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
        Self { base, package_roots, project_packages: true }
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
        for root in &self.package_roots {
            let type_folder = root.join("packages").join(&asset.kind);
            let pack = type_folder.join(&asset.id);
            if !pack.join("manifest.json").is_file() {
                continue;
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

    /// The path of `package`, or `None` when it is not found.
    pub fn package_path(&self, package: &str) -> Option<PathBuf> {
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
}
