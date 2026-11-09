//! Where the editor finds its files: the package root and the paths of the open project.

use std::path::{Path, PathBuf};

use d3d12_backend::ProjectPaths;

/// Names a directory holding `packages/gpufx`, for an install that keeps them elsewhere.
pub const PACKAGE_ROOT_VARIABLE: &str = "GPUFX_PACKAGE_ROOT";
/// The packages of a checkout, the last place looked in.
const DEV_PACKAGE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../external/FFmpegGpuFilter");
/// A project with no file yet: its clips hold absolute paths, which [`ProjectPaths`] keeps as they are.
const UNSAVED_PROJECT_FILE: &str = "unsaved.json";

/// Whether `root` holds effect packages.
pub fn has_packages(root: &Path) -> bool {
    root.join("packages").join("gpufx").is_dir()
}

/// The first of the candidates that holds packages: next to the executable, the variable, the checkout.
pub fn choose_package_root(exe_dir: Option<PathBuf>, variable: Option<PathBuf>, dev: PathBuf, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    [exe_dir, variable, Some(dev)].into_iter().flatten().find(|root| exists(root))
}

/// The package root of this run, or none when no candidate has packages.
pub fn package_roots() -> Vec<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let variable = std::env::var_os(PACKAGE_ROOT_VARIABLE).map(PathBuf::from);
    choose_package_root(exe_dir, variable, PathBuf::from(DEV_PACKAGE_ROOT), has_packages).into_iter().collect()
}

/// The paths of the project stored in `file`, or of an unsaved one.
pub fn project_paths(file: Option<&Path>, package_roots: &[PathBuf]) -> ProjectPaths {
    ProjectPaths::new(file.unwrap_or_else(|| Path::new(UNSAVED_PROJECT_FILE)), package_roots.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn next_to_the_executable_wins() {
        let chosen = choose_package_root(Some(root("exe")), Some(root("var")), root("dev"), |_| true);
        assert_eq!(chosen, Some(root("exe")));
    }

    #[test]
    fn the_variable_comes_before_the_checkout() {
        let chosen = choose_package_root(Some(root("exe")), Some(root("var")), root("dev"), |p| p != Path::new("exe"));
        assert_eq!(chosen, Some(root("var")));
    }

    #[test]
    fn the_checkout_is_the_last_resort_and_may_be_missing() {
        assert_eq!(choose_package_root(None, None, root("dev"), |_| true), Some(root("dev")));
        assert_eq!(choose_package_root(Some(root("exe")), None, root("dev"), |_| false), None);
    }

    #[test]
    fn paths_follow_the_project_file() {
        let paths = project_paths(Some(Path::new(r"D:\work\a.json")), &[]);
        assert_eq!(paths.source_path("b.mp4"), PathBuf::from(r"D:\work\b.mp4"));
    }
}
