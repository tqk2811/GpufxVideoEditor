//! The AI background removal models: where they are installed, importing one from a zip, and
//! fetching one on first use.
//!
//! A model is a `gpufx_ai_matting` package zip, installed as
//! `%LOCALAPPDATA%\gpufx-editor\models\gpufx_ai_matting\<id>.zip`. The compositor also finds the
//! models of the package roots (a debug build's `target\debug\packages`), see
//! [`d3d12_backend::find_matting_model`].
//!
//! Fetching: the converted zips are release assets of `tqk2811/FFmpegGpuFilter`
//! ([`DEFAULT_MODEL_BASE_URL`], overridable with [`MODEL_BASE_URL_ENV`]); each download is checked
//! against [`expected_zip_sha256`]. The editor also offers "import from a zip file".

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use engine::MatteModel;
use sha2::{Digest, Sha256};

/// Names another base URL the models are fetched from: `<base>/<id>.zip`.
pub const MODEL_BASE_URL_ENV: &str = "GPUFX_MODEL_BASE_URL";

/// The built-in base URL of the models: the release of the matting models of FFmpegGpuFilter.
pub const DEFAULT_MODEL_BASE_URL: Option<&str> = Some("https://github.com/tqk2811/FFmpegGpuFilter/releases/download/models-matting-v1");

/// The SHA-256 of the released zip of each model (the zips `scripts/convert-matting-models.py`
/// built for that release). The `source.json` of a package records only the hashes of the
/// intermediate ncnn files, not of the zip.
pub fn expected_zip_sha256(model: MatteModel) -> Option<&'static str> {
    match model {
        MatteModel::Off => None,
        MatteModel::Quality => Some("b591b624c4564782719a4f6c23a344c1848d805c648d6e90fc0bcc1cb04952f9"),
        MatteModel::Fast => Some("ed81a1278a4e9cd2031dd6571ba264fe9e22a5c2a7a25e91b18bbc622b3f3835"),
    }
}

/// Where and how to fetch a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSource {
    /// `<base>/<id>.zip` is fetched.
    pub base_url: Option<String>,
    /// The SHA-256 the fetched zip must have, lowercase hex.
    pub sha256: Option<String>,
}

impl ModelSource {
    /// The source of `model` from the build and the environment.
    pub fn of(model: MatteModel) -> Self {
        let base_url = std::env::var(MODEL_BASE_URL_ENV).ok().filter(|url| !url.trim().is_empty()).or_else(|| DEFAULT_MODEL_BASE_URL.map(str::to_owned));
        Self { base_url, sha256: expected_zip_sha256(model).map(str::to_owned) }
    }

    /// Whether fetching can run: a URL and a hash to check against.
    pub fn enabled(&self) -> bool {
        self.base_url.is_some() && self.sha256.is_some()
    }
}

/// The folder the editor installs models into.
pub fn install_dir() -> Option<PathBuf> {
    d3d12_backend::user_models_dir().map(|dir| dir.join(d3d12_backend::MATTING_FOLDER))
}

/// Where each model was found when last looked, so the inspector does not touch the disk every
/// frame. [`refresh`] looks again.
static FOUND: Mutex<Option<HashMap<MatteModel, Option<PathBuf>>>> = Mutex::new(None);

/// Where `model` is installed, looking in the folders of [`d3d12_backend::model_folders`] the
/// first time after a [`refresh`].
pub fn installed(model: MatteModel, package_roots: &[PathBuf]) -> Option<PathBuf> {
    let mut found = FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    found.get_or_insert_with(HashMap::new).entry(model).or_insert_with(|| d3d12_backend::find_matting_model(model, package_roots)).clone()
}

/// Forgets where the models were found: after an import or a fetch.
pub fn refresh() {
    *FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// The SHA-256 of a file, lowercase hex.
pub fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Checks that `zip` is a matting model package for `model`: a `gpufx_ai_matting` package whose
/// `io.json` sizes the frame the way that model does.
pub fn check_model_zip(model: MatteModel, zip: &Path) -> Result<(), String> {
    d3d12_backend::check_matting_package(zip)?;
    let file = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut io = String::new();
    archive.by_name("io.json").map_err(|_| "the package has no io.json".to_owned())?.read_to_string(&mut io).map_err(|e| e.to_string())?;
    let io: serde_json::Value = serde_json::from_str(&io).map_err(|e| format!("io.json: {e}"))?;
    let mode = io.pointer("/size/mode").and_then(serde_json::Value::as_str).unwrap_or_default();
    let wanted = match model {
        MatteModel::Quality => "short_side",
        MatteModel::Fast => "fixed",
        MatteModel::Off => return Err("no model".to_owned()),
    };
    if mode != wanted {
        return Err(format!("this package is not the {} model (its io.json size mode is \"{mode}\")", model.package_id().unwrap_or_default()));
    }
    Ok(())
}

/// Copies `source` into `folder` as the zip of `model`, through a temporary file so a failure
/// leaves the old one. Returns where it went.
fn install_file(model: MatteModel, source: &Path, folder: &Path) -> Result<PathBuf, String> {
    let id = model.package_id().ok_or("no model")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let target = folder.join(format!("{id}.zip"));
    let temporary = folder.join(format!("{id}.zip.part"));
    std::fs::copy(source, &temporary).map_err(|e| format!("copying {}: {e}", source.display()))?;
    std::fs::rename(&temporary, &target).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("{}: {e}", target.display())
    })?;
    refresh();
    Ok(target)
}

/// Imports the model zip `source` the user picked as `model` into `folder` (normally
/// [`install_dir`]), after checking it.
pub fn import_zip(model: MatteModel, source: &Path, folder: &Path) -> Result<PathBuf, String> {
    check_model_zip(model, source)?;
    install_file(model, source, folder)
}

/// Fetches `model` from `source` into `folder` with `curl.exe` (part of Windows), checks the
/// SHA-256 and the package, then installs it. Refused while the source is not [`ModelSource::enabled`].
pub fn fetch(model: MatteModel, source: &ModelSource, folder: &Path) -> Result<PathBuf, String> {
    let (Some(base), Some(sha256)) = (&source.base_url, &source.sha256) else {
        return Err("the model has no download source: import the zip instead".to_owned());
    };
    let id = model.package_id().ok_or("no model")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let download = folder.join(format!("{id}.zip.download"));
    let url = format!("{}/{id}.zip", base.trim_end_matches('/'));
    let status = Command::new("curl.exe").args(["--fail", "--location", "--silent", "--show-error", "--output"]).arg(&download).arg(&url).status().map_err(|e| format!("running curl: {e}"))?;
    let result = (|| {
        if !status.success() {
            return Err(format!("fetching {url} failed ({status})"));
        }
        let got = file_sha256(&download)?;
        if !got.eq_ignore_ascii_case(sha256) {
            return Err(format!("{url}: SHA-256 {got} is not the expected {sha256}"));
        }
        import_zip(model, &download, folder)
    })();
    let _ = std::fs::remove_file(&download);
    result
}

/// Installs `model` into [`install_dir`]: from the zip `path`, or downloaded from its
/// [`ModelSource`] when `path` is `None`.
pub fn install(model: MatteModel, path: Option<&Path>) -> Result<PathBuf, String> {
    let folder = install_dir().ok_or("LOCALAPPDATA is not set")?;
    match path {
        Some(path) => import_zip(model, path, &folder),
        None => fetch(model, &ModelSource::of(model), &folder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-matting-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A built model zip, from [`d3d12_backend::MATTING_MODELS_ENV`] or the FFmpegGpuFilter build.
    fn built_zip(model: MatteModel) -> Option<PathBuf> {
        let id = model.package_id().unwrap();
        let mut folders = Vec::new();
        if let Some(dir) = std::env::var_os(d3d12_backend::MATTING_MODELS_ENV) {
            folders.push(PathBuf::from(dir));
        }
        folders.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/FFmpegGpuFilter/build/packages/gpufx_ai_matting"));
        folders.into_iter().map(|folder| folder.join(format!("{id}.zip"))).find(|zip| zip.is_file())
    }

    #[test]
    fn fetching_is_off_until_a_url_and_a_hash_are_set() {
        assert!(!ModelSource { base_url: None, sha256: Some("00".into()) }.enabled());
        assert!(!ModelSource { base_url: Some("https://example.invalid".into()), sha256: None }.enabled());
        let folder = scratch("off");
        let refused = fetch(MatteModel::Fast, &ModelSource { base_url: None, sha256: None }, &folder);
        assert!(refused.unwrap_err().contains("no download source"));
        assert!(ModelSource::of(MatteModel::Quality).enabled() && ModelSource::of(MatteModel::Fast).enabled(), "fetching is on by default");
        assert!(!ModelSource::of(MatteModel::Off).enabled());
    }

    #[test]
    fn a_file_that_is_not_a_model_is_refused() {
        let folder = scratch("bad");
        let bad = folder.join("bad.zip");
        std::fs::write(&bad, b"not a zip").unwrap();
        assert!(import_zip(MatteModel::Fast, &bad, &folder.join("installed")).is_err());
        assert!(!folder.join("installed").join("pp-humanseg-v2-lite.zip").exists());
    }

    #[test]
    fn the_built_zips_are_the_released_ones() {
        for model in [MatteModel::Quality, MatteModel::Fast] {
            let Some(zip) = built_zip(model) else {
                eprintln!("skipping: the {} zip is not built", model.package_id().unwrap());
                continue;
            };
            assert_eq!(Some(file_sha256(&zip).unwrap().as_str()), expected_zip_sha256(model), "{} was rebuilt: update expected_zip_sha256 and the release", zip.display());
        }
    }

    #[test]
    fn a_built_model_imports_and_a_wrong_one_is_refused() {
        let Some(fast) = built_zip(MatteModel::Fast) else {
            eprintln!("skipping: the pp-humanseg-v2-lite zip is not built (scripts/convert-matting-models.py)");
            return;
        };
        let folder = scratch("import");
        assert!(import_zip(MatteModel::Quality, &fast, &folder).is_err(), "PP-HumanSeg is not the quality model");
        let installed = import_zip(MatteModel::Fast, &fast, &folder).expect("imports");
        assert_eq!(installed, folder.join("pp-humanseg-v2-lite.zip"));
        assert_eq!(file_sha256(&installed).unwrap(), file_sha256(&fast).unwrap());
    }

    #[test]
    fn a_fetch_checks_the_hash() {
        let Some(fast) = built_zip(MatteModel::Fast) else {
            eprintln!("skipping: the pp-humanseg-v2-lite zip is not built");
            return;
        };
        let served = scratch("served");
        std::fs::copy(&fast, served.join("pp-humanseg-v2-lite.zip")).unwrap();
        let base = format!("file:///{}", served.display().to_string().replace('\\', "/"));
        let folder = scratch("fetched");
        let wrong = ModelSource { base_url: Some(base.clone()), sha256: Some("0".repeat(64)) };
        assert!(fetch(MatteModel::Fast, &wrong, &folder).unwrap_err().contains("SHA-256"));
        assert!(!folder.join("pp-humanseg-v2-lite.zip").exists());
        let right = ModelSource { base_url: Some(base), sha256: Some(file_sha256(&fast).unwrap()) };
        assert_eq!(fetch(MatteModel::Fast, &right, &folder).expect("fetched"), folder.join("pp-humanseg-v2-lite.zip"));
    }
}
