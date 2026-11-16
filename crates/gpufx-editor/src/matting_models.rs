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
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine::MatteModel;
use sha2::{Digest, Sha256};

/// Names another base URL the models are fetched from: `<base>/<id>.zip`.
pub const MODEL_BASE_URL_ENV: &str = "GPUFX_MODEL_BASE_URL";

/// The built-in base URL of the models: the release of the matting models of FFmpegGpuFilter.
pub const DEFAULT_MODEL_BASE_URL: Option<&str> = Some("https://github.com/tqk2811/FFmpegGpuFilter/releases/download/models-matting-v1");

/// The SHA-256 of the released zip of each model: `sha256_zip` of the package's `source.json`
/// in the FFmpegGpuFilter checkout, built into the editor.
pub fn expected_zip_sha256(model: MatteModel) -> Option<String> {
    let source = match model {
        MatteModel::Off => return None,
        MatteModel::Quality => include_str!("../../../external/FFmpegGpuFilter/packages/gpufx_ai_matting/modnet-webcam/source.json"),
        MatteModel::Fast => include_str!("../../../external/FFmpegGpuFilter/packages/gpufx_ai_matting/pp-humanseg-v2-lite/source.json"),
    };
    let source: serde_json::Value = serde_json::from_str(source).ok()?;
    source.get("sha256_zip")?.as_str().filter(|hash| hash.len() == 64).map(str::to_ascii_lowercase)
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
        Self { base_url, sha256: expected_zip_sha256(model) }
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
    let temporary = folder.join(format!("{id}.zip.{}.part", std::process::id()));
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

/// What a download reports while it runs, shared between the download thread and the UI.
#[derive(Debug, Default)]
pub struct Progress {
    /// Bytes received so far.
    pub received: AtomicU64,
    /// The size of the zip, 0 while curl does not know it yet.
    pub total: AtomicU64,
    /// Set to stop the download: curl is killed and the partial file removed.
    pub cancel: AtomicBool,
}

impl Progress {
    /// `(received, total)`, `total` being `None` while unknown.
    pub fn snapshot(&self) -> (u64, Option<u64>) {
        let total = self.total.load(Ordering::Relaxed);
        (self.received.load(Ordering::Relaxed), (total > 0).then_some(total))
    }

    /// Whether the download was asked to stop.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// The problem of a download that was cancelled.
pub const CANCELLED: &str = "cancelled";

/// Parses a size of curl's progress meter: `512`, `320k`, `10.2M` (1024-based suffixes).
fn parse_curl_size(text: &str) -> Option<u64> {
    let scale = match text.chars().last()? {
        'k' | 'K' => 1u64 << 10,
        'M' => 1 << 20,
        'G' => 1 << 30,
        'T' => 1 << 40,
        'P' => 1 << 50,
        _ => 1,
    };
    let number = if scale == 1 { text } else { &text[..text.len() - 1] };
    let value: f64 = number.parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| (value * scale as f64) as u64)
}

/// Parses one line of curl's progress meter (`% Total % Received % Xferd …`) into
/// `(received, total)`; `None` for the header and other lines.
pub fn parse_curl_progress(line: &str) -> Option<(u64, u64)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 4 || fields[0].parse::<u32>().is_err() || fields[2].parse::<u32>().is_err() {
        return None;
    }
    Some((parse_curl_size(fields[3])?, parse_curl_size(fields[1])?))
}

/// Reads curl's stderr: progress lines (split on `\r` and `\n`) go to `progress`, the other
/// non-empty lines are returned (curl's error messages).
fn read_curl_stderr(stderr: impl Read, progress: &Progress) -> Vec<String> {
    let mut messages = Vec::new();
    let mut line = Vec::new();
    let mut handle = |line: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(line).trim().to_owned();
        line.clear();
        if text.is_empty() {
            return;
        }
        match parse_curl_progress(&text) {
            Some((received, total)) => {
                progress.received.store(received, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
            }
            None if text.starts_with('%') || text.contains("Dload") => {}
            None => messages.push(text),
        }
    };
    for byte in std::io::BufReader::new(stderr).bytes() {
        let Ok(byte) = byte else { break };
        if byte == b'\r' || byte == b'\n' {
            handle(&mut line);
        } else {
            line.push(byte);
        }
    }
    handle(&mut line);
    messages
}

/// Waits for `child`, killing it when `progress` is cancelled. Returns whether it succeeded,
/// `Err(CANCELLED)` after a cancel.
fn wait_or_cancel(child: &mut Child, progress: &Progress) -> Result<bool, String> {
    loop {
        if progress.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("waiting for curl: {e}")),
        }
    }
}

/// Fetches `model` from `source` into `folder` with `curl.exe` (part of Windows), checks the
/// SHA-256 and the package, then installs it. Refused while the source is not [`ModelSource::enabled`].
pub fn fetch(model: MatteModel, source: &ModelSource, folder: &Path) -> Result<PathBuf, String> {
    fetch_with_progress(model, source, folder, &Progress::default())
}

/// [`fetch`], reporting to `progress` and stopping when it is cancelled. The partial download is
/// removed whatever happens, and the installed zip is only replaced (temporary file + rename)
/// once the new one checks out.
pub fn fetch_with_progress(model: MatteModel, source: &ModelSource, folder: &Path, progress: &Progress) -> Result<PathBuf, String> {
    let (Some(base), Some(sha256)) = (&source.base_url, &source.sha256) else {
        return Err("the model has no download source: import the zip instead".to_owned());
    };
    let id = model.package_id().ok_or("no model")?;
    if progress.cancelled() {
        return Err(CANCELLED.to_owned());
    }
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let download = folder.join(format!("{id}.zip.{}.download", std::process::id()));
    let url = format!("{}/{id}.zip", base.trim_end_matches('/'));
    let result = (|| {
        let mut child = Command::new("curl.exe")
            .args(["--fail", "--location", "--show-error", "--proto", "=https,file", "--proto-redir", "=https", "--connect-timeout", "30", "--max-time", "1800", "--output"])
            .arg(&download)
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("running curl: {e}"))?;
        let stderr = child.stderr.take();
        let (waited, messages) = std::thread::scope(|scope| {
            let reader = scope.spawn(|| stderr.map(|stderr| read_curl_stderr(stderr, progress)).unwrap_or_default());
            let waited = wait_or_cancel(&mut child, progress);
            (waited, reader.join().unwrap_or_default())
        });
        if !waited? {
            return Err(format!("fetching {url} failed: {}", messages.join("; ")));
        }
        if progress.cancelled() {
            return Err(CANCELLED.to_owned());
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

/// Where a background download of a model stands.
#[derive(Debug, Clone)]
pub enum DownloadStatus {
    /// Running, reporting to the shared progress.
    Running(Arc<Progress>),
    /// Stopped with this problem; starting again retries.
    Failed(String),
}

/// A finished download the window has not handled yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    Installed(PathBuf),
    Failed(String),
    Cancelled,
}

/// The background downloads of the models, one at most per model.
#[derive(Debug, Default)]
pub struct Downloads {
    status: Mutex<HashMap<MatteModel, DownloadStatus>>,
    finished: Mutex<Vec<(MatteModel, DownloadOutcome)>>,
}

impl Downloads {
    /// Where the download of `model` stands; `None` when none ran, or the last one succeeded or
    /// was cancelled.
    pub fn status(&self, model: MatteModel) -> Option<DownloadStatus> {
        self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&model).cloned()
    }

    /// Whether a download is running.
    pub fn any_running(&self) -> bool {
        self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).values().any(|status| matches!(status, DownloadStatus::Running(_)))
    }

    /// Marks `model` running and returns its progress, or `None` when it already runs.
    pub fn begin(&self, model: MatteModel) -> Option<Arc<Progress>> {
        let mut status = self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(status.get(&model), Some(DownloadStatus::Running(_))) {
            return None;
        }
        let progress = Arc::new(Progress::default());
        status.insert(model, DownloadStatus::Running(progress.clone()));
        Some(progress)
    }

    /// Records the end of the download of `model`.
    pub fn end(&self, model: MatteModel, result: Result<PathBuf, String>) {
        let outcome = match result {
            Ok(path) => DownloadOutcome::Installed(path),
            Err(problem) if problem == CANCELLED => DownloadOutcome::Cancelled,
            Err(problem) => DownloadOutcome::Failed(problem),
        };
        {
            let mut status = self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match &outcome {
                DownloadOutcome::Failed(problem) => {
                    status.insert(model, DownloadStatus::Failed(problem.clone()));
                }
                _ => {
                    status.remove(&model);
                }
            }
        }
        self.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((model, outcome));
    }

    /// Asks the download of `model` to stop.
    pub fn cancel(&self, model: MatteModel) {
        if let Some(DownloadStatus::Running(progress)) = self.status(model) {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The downloads that ended since the last call.
    pub fn take_finished(&self) -> Vec<(MatteModel, DownloadOutcome)> {
        std::mem::take(&mut *self.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Runs `work` for `model` on a background thread; `false` when `model` already downloads.
    pub fn start_with(self: &Arc<Self>, model: MatteModel, work: impl FnOnce(&Progress) -> Result<PathBuf, String> + Send + 'static) -> bool {
        let Some(progress) = self.begin(model) else {
            return false;
        };
        let downloads = self.clone();
        let spawned = std::thread::Builder::new().name("matte-model-download".to_owned()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&progress))).unwrap_or_else(|_| Err("the download panicked".to_owned()));
            downloads.end(model, result);
        });
        if let Err(e) = spawned {
            self.end(model, Err(format!("starting the download: {e}")));
        }
        true
    }
}

/// The downloads of the editor.
pub fn downloads() -> &'static Arc<Downloads> {
    static DOWNLOADS: std::sync::OnceLock<Arc<Downloads>> = std::sync::OnceLock::new();
    DOWNLOADS.get_or_init(Arc::default)
}

/// Starts downloading `model` from its [`ModelSource`] into [`install_dir`] in the background;
/// `false` when it already downloads.
pub fn start_download(model: MatteModel) -> bool {
    downloads().start_with(model, move |progress| {
        let folder = install_dir().ok_or("LOCALAPPDATA is not set")?;
        fetch_with_progress(model, &ModelSource::of(model), &folder, progress)
    })
}

/// `received / total (percent)` for the panel, or only `received` while the size is unknown.
pub fn format_progress(received: u64, total: Option<u64>) -> String {
    fn megabytes(bytes: u64) -> String {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
    match total {
        Some(total) if total > 0 => format!("{} / {} ({}%)", megabytes(received), megabytes(total), received.min(total) * 100 / total),
        _ => megabytes(received),
    }
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
    fn curl_progress_lines_parse() {
        assert_eq!(parse_curl_progress("  3 10.2M    3  320k    0     0   512k      0  0:00:20 --:--:--  0:00:20  512k"), Some((320 * 1024, (10.2 * 1048576.0) as u64)));
        assert_eq!(parse_curl_progress("100  2048  100  2048    0     0   9000      0 --:--:-- --:--:-- --:--:--  9000"), Some((2048, 2048)));
        assert_eq!(parse_curl_progress("  % Total    % Received % Xferd  Average Speed   Time"), None);
        assert_eq!(parse_curl_progress("curl: (22) The requested URL returned error: 404"), None);
        let progress = Progress::default();
        let stderr = b"  % Total    % Received % Xferd  Average Speed\r\n                                 Dload  Upload\r\n  0     0    0     0    0     0\r 50  1024   50   512    0     0\rcurl: (6) Could not resolve host\n";
        let messages = read_curl_stderr(&stderr[..], &progress);
        assert_eq!(progress.snapshot(), (512, Some(1024)));
        assert_eq!(messages, vec!["curl: (6) Could not resolve host".to_owned()]);
        assert_eq!(format_progress(512 * 1024, Some(1024 * 1024)), "0.5 MB / 1.0 MB (50%)");
        assert_eq!(format_progress(1024 * 1024, None), "1.0 MB");
    }

    #[test]
    fn downloads_run_once_and_record_the_outcome() {
        let downloads = Arc::new(Downloads::default());
        let (release, wait) = std::sync::mpsc::channel::<()>();
        assert!(downloads.start_with(MatteModel::Fast, move |progress| {
            progress.received.store(7, Ordering::Relaxed);
            wait.recv().ok();
            Err("offline".to_owned())
        }));
        assert!(downloads.any_running());
        assert!(!downloads.start_with(MatteModel::Fast, |_| Ok(PathBuf::new())), "no second download of the same model");
        release.send(()).unwrap();
        let finished = (0..500).find_map(|_| {
            let finished = downloads.take_finished();
            if finished.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
                None
            } else {
                Some(finished)
            }
        });
        assert_eq!(finished, Some(vec![(MatteModel::Fast, DownloadOutcome::Failed("offline".to_owned()))]));
        assert!(matches!(downloads.status(MatteModel::Fast), Some(DownloadStatus::Failed(problem)) if problem == "offline"));
        // A retry replaces the failure; a cancel ends without a failure.
        let progress = downloads.begin(MatteModel::Fast).expect("a retry starts");
        downloads.cancel(MatteModel::Fast);
        assert!(progress.cancelled());
        downloads.end(MatteModel::Fast, Err(CANCELLED.to_owned()));
        assert!(downloads.status(MatteModel::Fast).is_none());
        assert_eq!(downloads.take_finished(), vec![(MatteModel::Fast, DownloadOutcome::Cancelled)]);
        downloads.end(MatteModel::Quality, Ok(PathBuf::from("q.zip")));
        assert!(downloads.status(MatteModel::Quality).is_none() && !downloads.any_running());
    }

    #[test]
    fn a_fetch_cancelled_before_it_starts_leaves_no_file() {
        let folder = scratch("cancel");
        let source = ModelSource { base_url: Some("https://example.invalid".into()), sha256: Some("0".repeat(64)) };
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert_eq!(fetch_with_progress(MatteModel::Fast, &source, &folder, &progress).unwrap_err(), CANCELLED);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    }

    #[test]
    fn cancelling_a_running_fetch_kills_curl_and_removes_the_partial_file() {
        // A local address nothing answers on stands for a slow server, without network.
        let folder = scratch("kill");
        let source = ModelSource { base_url: Some("https://127.0.0.1:9".into()), sha256: Some("0".repeat(64)) };
        let progress = Arc::new(Progress::default());
        let cancel = progress.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel.store(true, Ordering::Relaxed);
        });
        let result = fetch_with_progress(MatteModel::Fast, &source, &folder, &progress);
        canceller.join().unwrap();
        // curl either was still connecting (cancelled) or already failed to connect: both leave no file.
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0, "no partial file is left");
    }

    #[test]
    fn wait_or_cancel_kills_the_child() {
        let Ok(mut child) = Command::new("curl.exe").args(["--silent", "--max-time", "60", "file:///CON"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            eprintln!("skipping: curl.exe is not available");
            return;
        };
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert_eq!(wait_or_cancel(&mut child, &progress), Err(CANCELLED.to_owned()));
        assert!(child.try_wait().unwrap().is_some(), "curl was stopped");
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
            assert_eq!(Some(file_sha256(&zip).unwrap()), expected_zip_sha256(model), "{} was rebuilt: update sha256_zip of its source.json and the release", zip.display());
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
