//! The model of the robust tracker (NanoTrack V2): where it is installed, importing it from a
//! zip, and fetching it on first use.
//!
//! The model is a `gpufx_ai_tracking` package zip (`tracker.json` with a backbone and a head
//! model), installed as `%LOCALAPPDATA%\gpufx-editor\models\gpufx_ai_tracking\nanotrack-v2.zip`,
//! and also looked for in [`TRACKING_MODELS_ENV`] and the package roots.
//!
//! Fetching: the converted zip is a release asset of `tqk2811/FFmpegGpuFilter`
//! ([`DEFAULT_BASE_URL`], tag `models-tracking-v1`, overridable with [`BASE_URL_ENV`]), checked
//! against `sha256_zip` of the package's `source.json`. Until that release exists the hash is
//! empty and fetching stays off: the model can still be imported from a zip.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use crate::matting_models::ModelSource;
use crate::model_download::{Downloads, Progress};

/// The id of the model: its zip is `<id>.zip`.
pub const MODEL_ID: &str = "nanotrack-v2";

/// The folder of tracking models under a models folder or a package root's `packages`.
pub const TRACKING_FOLDER: &str = "gpufx_ai_tracking";

/// Names an extra folder holding `nanotrack-v2.zip`, looked at first. For tests and development.
pub const TRACKING_MODELS_ENV: &str = "GPUFX_TRACKING_MODELS";

/// Names another base URL the model is fetched from: `<base>/nanotrack-v2.zip`.
pub const BASE_URL_ENV: &str = "GPUFX_TRACKING_MODEL_BASE_URL";

/// The built-in base URL: the release of the tracking models of FFmpegGpuFilter.
pub const DEFAULT_BASE_URL: &str = "https://github.com/tqk2811/FFmpegGpuFilter/releases/download/models-tracking-v1";

/// The SHA-256 of the released zip: `sha256_zip` of the package's `source.json`, built in;
/// `None` while it is empty (no release yet).
pub fn expected_zip_sha256() -> Option<String> {
    let source = include_str!("../../../external/FFmpegGpuFilter/packages/gpufx_ai_tracking/nanotrack-v2/source.json");
    let source: serde_json::Value = serde_json::from_str(source).ok()?;
    source.get("sha256_zip")?.as_str().filter(|hash| hash.len() == 64).map(str::to_ascii_lowercase)
}

/// Where and how to fetch the model, from the build and the environment.
pub fn source() -> ModelSource {
    let base_url = std::env::var(BASE_URL_ENV).ok().filter(|url| !url.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    ModelSource { base_url: Some(base_url), sha256: expected_zip_sha256() }
}

/// The folder the editor installs the model into.
pub fn install_dir() -> Option<PathBuf> {
    d3d12_backend::user_models_dir().map(|dir| dir.join(TRACKING_FOLDER))
}

/// The folders the model is looked for in, in order: [`TRACKING_MODELS_ENV`], the user's models
/// folder, then `<root>/packages/gpufx_ai_tracking` of each package root.
pub fn model_folders(package_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(dir) = std::env::var_os(TRACKING_MODELS_ENV).filter(|dir| !dir.is_empty()) {
        folders.push(PathBuf::from(dir));
    }
    folders.extend(install_dir());
    folders.extend(package_roots.iter().map(|root| root.join("packages").join(TRACKING_FOLDER)));
    folders
}

/// The model in `folder`: `nanotrack-v2.zip`, or a folder `nanotrack-v2` holding `tracker.json`.
fn model_in(folder: &Path) -> Option<PathBuf> {
    let zip = folder.join(format!("{MODEL_ID}.zip"));
    if zip.is_file() {
        return Some(zip);
    }
    let dir = folder.join(MODEL_ID);
    dir.join("tracker.json").is_file().then_some(dir)
}

/// Where the model was found when last looked, with the package roots looked in, so the inspector
/// does not scan every frame. [`refresh`] forgets it.
static FOUND: Mutex<Option<(Vec<PathBuf>, PathBuf)>> = Mutex::new(None);

/// Where the model is installed. The place found last is kept while it still exists and the
/// package roots are the same; otherwise [`model_folders`] are looked in again. Not finding it is
/// not kept, so a model put in place by hand is seen.
pub fn installed(package_roots: &[PathBuf]) -> Option<PathBuf> {
    let mut found = FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((roots, path)) = found.as_ref() {
        if roots.as_slice() == package_roots && path.exists() {
            return Some(path.clone());
        }
    }
    let path = model_folders(package_roots).iter().find_map(|folder| model_in(folder));
    *found = path.clone().map(|path| (package_roots.to_vec(), path));
    path
}

/// Forgets where the model was found: after an import or a fetch.
pub fn refresh() {
    *FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Checks without a GPU that `zip` is a tracker package: `tracker.json` of a Siamese tracker and
/// both its models, each listing `gpufx_ai_tracking`.
pub fn check_model_zip(zip: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", zip.display()))?;
    let names: Vec<String> = archive.file_names().filter_map(|name| name.ok().map(|name| name.into_owned())).collect();
    let mut read = |name: &str| -> Result<serde_json::Value, String> {
        let mut text = String::new();
        archive.by_name(name).map_err(|_| format!("the package has no {name}"))?.read_to_string(&mut text).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("{name}: {e}"))
    };
    let tracker = read("tracker.json")?;
    if tracker["format"] != "gpufx-nn-tracker" || tracker["version"] != 1 || tracker["kind"] != "siamese" {
        return Err("tracker.json is not a gpufx-nn-tracker version 1 of kind siamese".to_owned());
    }
    for part in ["backbone", "head"] {
        let dir = tracker[part].as_str().filter(|dir| !dir.is_empty() && !dir.contains(['/', '\\', ':', '.'])).ok_or_else(|| format!("tracker.json names no {part} folder"))?;
        let model = read(&format!("{dir}/model.json"))?;
        let used_for = model["used_for"].as_array().cloned().unwrap_or_default();
        if !used_for.iter().any(|filter| filter == TRACKING_FOLDER) {
            return Err(format!("{dir}/model.json is not for {TRACKING_FOLDER}"));
        }
        if !names.iter().any(|name| *name == format!("{dir}/weights.bin")) {
            return Err(format!("the package has no {dir}/weights.bin"));
        }
    }
    Ok(())
}

/// Imports the model zip `source` into `folder` (normally [`install_dir`]), after checking it.
pub fn import_zip(source: &Path, folder: &Path) -> Result<PathBuf, String> {
    check_model_zip(source)?;
    let installed = crate::model_download::install_file(MODEL_ID, source, folder)?;
    refresh();
    Ok(installed)
}

/// Fetches the model from `source` into `folder`, reporting to `progress`; refused while the
/// source has no hash to check against.
pub fn fetch_with_progress(source: &ModelSource, folder: &Path, progress: &Progress) -> Result<PathBuf, String> {
    let (Some(base), Some(sha256)) = (&source.base_url, &source.sha256) else {
        return Err("the tracking model has no download source yet: import the zip instead".to_owned());
    };
    let url = format!("{}/{MODEL_ID}.zip", base.trim_end_matches('/'));
    crate::model_download::fetch_zip(&url, sha256, MODEL_ID, folder, progress, |download| import_zip(download, folder))
}

/// The download of the model (one key: there is one model).
pub fn downloads() -> &'static Arc<Downloads<()>> {
    static DOWNLOADS: std::sync::OnceLock<Arc<Downloads<()>>> = std::sync::OnceLock::new();
    DOWNLOADS.get_or_init(Arc::default)
}

/// Starts downloading the model into [`install_dir`] in the background; `false` when it already
/// downloads.
pub fn start_download() -> bool {
    downloads().start_with((), move |progress| {
        let folder = install_dir().ok_or("LOCALAPPDATA is not set")?;
        fetch_with_progress(&source(), &folder, progress)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-tracking-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The built model zip of the FFmpegGpuFilter checkout (`scripts/convert-tracking-models.py`).
    pub(crate) fn built_zip() -> Option<PathBuf> {
        let zip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/FFmpegGpuFilter/build/packages/gpufx_ai_tracking/nanotrack-v2.zip");
        zip.is_file().then_some(zip)
    }

    #[test]
    fn fetching_waits_for_the_release_hash() {
        let folder = scratch("off");
        let none = ModelSource { base_url: Some(DEFAULT_BASE_URL.to_owned()), sha256: None };
        assert!(fetch_with_progress(&none, &folder, &Progress::default()).unwrap_err().contains("no download source"));
        assert_eq!(source().sha256, expected_zip_sha256());
        assert!(source().base_url.unwrap().ends_with("models-tracking-v1"));
    }

    #[test]
    fn a_file_that_is_not_a_tracker_is_refused() {
        let folder = scratch("bad");
        let bad = folder.join("bad.zip");
        std::fs::write(&bad, b"not a zip").unwrap();
        assert!(import_zip(&bad, &folder.join("installed")).is_err());
        assert!(!folder.join("installed").join("nanotrack-v2.zip").exists());
    }

    #[test]
    fn the_built_model_imports_and_fetches_with_its_hash() {
        let Some(zip) = built_zip() else {
            eprintln!("skipping: nanotrack-v2.zip is not built (scripts/convert-tracking-models.py)");
            return;
        };
        let folder = scratch("import");
        let installed = import_zip(&zip, &folder).expect("imports");
        assert_eq!(installed, folder.join("nanotrack-v2.zip"));
        // A fetch from a local folder, checked against the hash of the built zip.
        let served = scratch("served");
        std::fs::copy(&zip, served.join("nanotrack-v2.zip")).unwrap();
        let base = format!("file:///{}", served.display().to_string().replace('\\', "/"));
        let fetched = scratch("fetched");
        let wrong = ModelSource { base_url: Some(base.clone()), sha256: Some("0".repeat(64)) };
        assert!(fetch_with_progress(&wrong, &fetched, &Progress::default()).unwrap_err().contains("SHA-256"));
        let right = ModelSource { base_url: Some(base), sha256: Some(crate::model_download::file_sha256(&zip).unwrap()) };
        assert_eq!(fetch_with_progress(&right, &fetched, &Progress::default()).expect("fetched"), fetched.join("nanotrack-v2.zip"));
        if let Some(released) = expected_zip_sha256() {
            assert_eq!(crate::model_download::file_sha256(&zip).unwrap(), released, "the zip was rebuilt: update sha256_zip of its source.json and the release");
        }
    }

    /// The place found is kept only while it exists and the package roots stay the same.
    #[test]
    fn the_found_model_follows_the_package_roots_and_the_disk() {
        if installed(&[]).is_some() {
            eprintln!("skipping: a model is installed outside the package roots");
            return;
        }
        let place = |root: &Path| {
            let folder = root.join("packages").join(TRACKING_FOLDER);
            std::fs::create_dir_all(&folder).unwrap();
            let zip = folder.join(format!("{MODEL_ID}.zip"));
            std::fs::write(&zip, b"zip").unwrap();
            zip
        };
        let (a, b) = (scratch("roots-a"), scratch("roots-b"));
        let (zip_a, zip_b) = (place(&a), place(&b));
        assert_eq!(installed(std::slice::from_ref(&a)), Some(zip_a.clone()));
        assert_eq!(installed(std::slice::from_ref(&b)), Some(zip_b), "other package roots are looked in");
        assert_eq!(installed(std::slice::from_ref(&a)), Some(zip_a.clone()));
        std::fs::remove_file(&zip_a).unwrap();
        assert_eq!(installed(std::slice::from_ref(&a)), None, "a deleted model is not given");
        place(&a);
        assert_eq!(installed(std::slice::from_ref(&a)), Some(zip_a), "not finding it is not kept");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
