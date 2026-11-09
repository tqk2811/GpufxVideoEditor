//! Where the project's source files and effect packages live on disk.

use std::path::{Path, PathBuf};

use engine::Resolver;

/// Resolves the relative paths of a project.
///
/// A source is relative to the project file. A package name is tried, in this order, as a path
/// relative to the project file, as a path relative to each package root, and, for a bare name
/// such as `mix`, as `<root>/packages/gpufx/<group>/<name>` with groups in alphabetical order.
/// A package is a directory holding `manifest.json`, or a `.zip` file.
pub struct ProjectPaths {
    base: PathBuf,
    package_roots: Vec<PathBuf>,
}

fn is_package(path: &Path) -> bool {
    path.join("manifest.json").is_file() || (path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
}

impl ProjectPaths {
    /// `project_file` is the path of the project JSON; `package_roots` are searched in order.
    pub fn new(project_file: &Path, package_roots: Vec<PathBuf>) -> Self {
        let base = project_file.parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self { base, package_roots }
    }

    /// The path of `source`.
    pub fn source_path(&self, source: &str) -> PathBuf {
        self.base.join(source)
    }

    /// The path of `package`, or `None` when it is not found.
    pub fn package_path(&self, package: &str) -> Option<PathBuf> {
        let direct = [self.base.join(package)].into_iter().chain(self.package_roots.iter().map(|root| root.join(package)));
        if let Some(found) = direct.into_iter().find(|p| is_package(p)) {
            return Some(found);
        }
        if package.contains(['/', '\\']) {
            return None;
        }
        for root in &self.package_roots {
            let Ok(entries) = std::fs::read_dir(root.join("packages").join("gpufx")) else { continue };
            let mut groups: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            groups.sort();
            if let Some(found) = groups.iter().map(|g| g.join(package)).find(|p| is_package(p)) {
                return Some(found);
            }
        }
        None
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
