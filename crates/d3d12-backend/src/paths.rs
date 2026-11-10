//! Where the project's source files and effect packages live on disk.

use std::path::{Path, PathBuf};

use engine::Resolver;

/// Resolves the relative paths of a project.
///
/// A source is relative to the project file. A package name is tried, in this order, as a path
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

    /// The path of `source`.
    pub fn source_path(&self, source: &str) -> PathBuf {
        self.base.join(source)
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

    #[test]
    fn a_relative_source_joins_the_project_folder() {
        let paths = ProjectPaths::new(Path::new(r"D:\work\project.json"), Vec::new());
        assert_eq!(paths.source_path("a.mp4"), PathBuf::from(r"D:\work\a.mp4"));
    }
}
