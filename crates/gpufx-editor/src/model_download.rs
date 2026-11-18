//! Fetching model zips in the background, shared by the AI models of the editor (background
//! removal, object tracking): `curl.exe` with progress and cancel, the SHA-256 check, the install
//! through a temporary file, the cleanup of what a killed editor left, and the table of downloads
//! the UI shows.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

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
        let mut text = String::from_utf8_lossy(line).trim().to_owned();
        line.clear();
        // curl prints its error right after the last progress line, without a line break.
        if let Some(at) = text.find("curl: (").filter(|&at| at > 0) {
            messages.push(text[at..].trim().to_owned());
            text.truncate(at);
            text = text.trim().to_owned();
        }
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
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for curl: {e}"));
            }
        }
    }
}

/// Removes `path`, trying again for a moment while something (an antivirus, a curl still
/// closing) holds it; [`clean_stale_files`] takes what is left at the next start.
fn remove_with_retry(path: &Path) {
    for _ in 0..10 {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => std::thread::sleep(Duration::from_millis(50)),
            _ => return,
        }
    }
}

/// Removes the `*.download` and `*.part` files of `folder` older than `stale_after`: what a
/// killed editor left behind. Returns how many went.
pub fn clean_stale_files(folder: &Path, stale_after: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    let now = std::time::SystemTime::now();
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            let leftover = name.ends_with(".download") || name.ends_with(".part");
            let old = entry.metadata().and_then(|meta| meta.modified()).is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() >= stale_after);
            leftover && old && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
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

/// The background downloads of models, one at most per key.
pub struct Downloads<K> {
    /// The status and the finished downloads under one lock, so a frame never sees a download
    /// that is neither running nor finished.
    inner: Mutex<DownloadsInner<K>>,
    /// Wakes the window when a download ends.
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct DownloadsInner<K> {
    status: HashMap<K, DownloadStatus>,
    finished: Vec<(K, DownloadOutcome)>,
}

impl<K> Default for DownloadsInner<K> {
    fn default() -> Self {
        Self { status: HashMap::new(), finished: Vec::new() }
    }
}

impl<K> Default for Downloads<K> {
    fn default() -> Self {
        Self { inner: Mutex::new(DownloadsInner::default()), waker: Mutex::new(None) }
    }
}

impl<K> std::fmt::Debug for Downloads<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let running = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).status.values().filter(|s| matches!(s, DownloadStatus::Running(_))).count();
        f.debug_struct("Downloads").field("running", &running).finish_non_exhaustive()
    }
}

impl<K: Clone + Eq + std::hash::Hash + Send + Sync + 'static> Downloads<K> {
    fn lock(&self) -> std::sync::MutexGuard<'_, DownloadsInner<K>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sets what wakes the window when a download ends.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(waker));
    }

    /// Where the download of `model` stands; `None` when none ran, or the last one succeeded or
    /// was cancelled.
    pub fn status(&self, key: K) -> Option<DownloadStatus> {
        self.lock().status.get(&key).cloned()
    }

    /// Whether a download is running.
    pub fn any_running(&self) -> bool {
        self.lock().status.values().any(|status| matches!(status, DownloadStatus::Running(_)))
    }

    /// Marks `model` running and returns its progress, or `None` when it already runs.
    pub fn begin(&self, key: K) -> Option<Arc<Progress>> {
        let mut inner = self.lock();
        if matches!(inner.status.get(&key), Some(DownloadStatus::Running(_))) {
            return None;
        }
        let progress = Arc::new(Progress::default());
        inner.status.insert(key.clone(), DownloadStatus::Running(progress.clone()));
        Some(progress)
    }

    /// Records the end of the download of `model` and wakes the window.
    pub fn end(&self, key: K, result: Result<PathBuf, String>) {
        let outcome = match result {
            Ok(path) => DownloadOutcome::Installed(path),
            Err(problem) if problem == CANCELLED => DownloadOutcome::Cancelled,
            Err(problem) => DownloadOutcome::Failed(problem),
        };
        {
            let mut inner = self.lock();
            match &outcome {
                DownloadOutcome::Failed(problem) => {
                    inner.status.insert(key.clone(), DownloadStatus::Failed(problem.clone()));
                }
                _ => {
                    inner.status.remove(&key);
                }
            }
            inner.finished.push((key.clone(), outcome));
        }
        let waker = self.waker.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Asks the download of `model` to stop.
    pub fn cancel(&self, key: K) {
        if let Some(DownloadStatus::Running(progress)) = self.status(key) {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The downloads that ended since the last call.
    pub fn take_finished(&self) -> Vec<(K, DownloadOutcome)> {
        std::mem::take(&mut self.lock().finished)
    }

    /// Runs `work` for `model` on a background thread; `false` when `model` already downloads.
    pub fn start_with(self: &Arc<Self>, key: K, work: impl FnOnce(&Progress) -> Result<PathBuf, String> + Send + 'static) -> bool {
        let Some(progress) = self.begin(key.clone()) else {
            return false;
        };
        let downloads = self.clone();
        let thread_key = key.clone();
        let spawned = std::thread::Builder::new().name("model-download".to_owned()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&progress))).unwrap_or_else(|_| Err("the download panicked".to_owned()));
            downloads.end(thread_key, result);
        });
        if let Err(e) = spawned {
            self.end(key, Err(format!("starting the download: {e}")));
        }
        true
    }
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

/// The problem of a download that was cancelled.
pub const CANCELLED: &str = "cancelled";

/// How old a leftover download or temporary file must be before [`clean_stale_files`] removes
/// it, so the running download of another editor is left alone.
pub const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// Copies `source` into `folder` as `<id>.zip`, through a temporary file so a failure
/// leaves the old one. Returns where it went.
pub fn install_file(id: &str, source: &Path, folder: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let target = folder.join(format!("{id}.zip"));
    let temporary = folder.join(format!("{id}.zip.{}.part", std::process::id()));
    std::fs::copy(source, &temporary).map_err(|e| format!("copying {}: {e}", source.display()))?;
    std::fs::rename(&temporary, &target).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("{}: {e}", target.display())
    })?;
    Ok(target)
}

/// Fetches `url` into `folder` with `curl.exe` (part of Windows) as a temporary download named after
/// `id`, reporting to `progress` and stopping when it is cancelled, checks its SHA-256 against
/// `sha256`, then hands it to `install`. The download is removed whatever happens.
pub fn fetch_zip(url: &str, sha256: &str, id: &str, folder: &Path, progress: &Progress, install: impl FnOnce(&Path) -> Result<PathBuf, String>) -> Result<PathBuf, String> {
    if progress.cancelled() {
        return Err(CANCELLED.to_owned());
    }
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let download = folder.join(format!("{id}.zip.{}.download", std::process::id()));
    let result = (|| {
        let mut child = Command::new("curl.exe")
            .args(["--fail", "--location", "--show-error", "--proto", "=https,file", "--proto-redir", "=https", "--connect-timeout", "30", "--max-time", "1800", "--output"])
            .arg(&download)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("running curl: {e}"))?;
        // The job takes curl down with the editor, even when it exits or crashes mid-download.
        let job = crate::child_process::KillOnClose::new().and_then(|job| job.assign(&child, "curl").map(|()| job));
        if let Err(problem) = job {
            let _ = child.kill();
            let _ = child.wait();
            return Err(problem);
        }
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
        // From here a cancel is not looked at: the install is a copy and a rename, quick and atomic.
        install(&download)
    })();
    remove_with_retry(&download);
    result
}


#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-model-download-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        let downloads = Arc::new(Downloads::<&str>::default());
        let (release, wait) = std::sync::mpsc::channel::<()>();
        assert!(downloads.start_with("fast", move |progress| {
            progress.received.store(7, Ordering::Relaxed);
            wait.recv().ok();
            Err("offline".to_owned())
        }));
        assert!(downloads.any_running());
        assert!(!downloads.start_with("fast", |_| Ok(PathBuf::new())), "no second download of the same model");
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
        assert_eq!(finished, Some(vec![("fast", DownloadOutcome::Failed("offline".to_owned()))]));
        assert!(matches!(downloads.status("fast"), Some(DownloadStatus::Failed(problem)) if problem == "offline"));
        // A retry replaces the failure; a cancel ends without a failure.
        let progress = downloads.begin("fast").expect("a retry starts");
        downloads.cancel("fast");
        assert!(progress.cancelled());
        downloads.end("fast", Err(CANCELLED.to_owned()));
        assert!(downloads.status("fast").is_none());
        assert_eq!(downloads.take_finished(), vec![("fast", DownloadOutcome::Cancelled)]);
        downloads.end("quality", Ok(PathBuf::from("q.zip")));
        assert!(downloads.status("quality").is_none() && !downloads.any_running());
    }

    #[test]
    fn wait_or_cancel_kills_a_hanging_child() {
        // `ping -n 600` runs for ten minutes: a child that truly hangs.
        let Ok(mut child) = Command::new("ping.exe").args(["-n", "600", "127.0.0.1"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            eprintln!("skipping: ping.exe is not available");
            return;
        };
        assert!(child.try_wait().unwrap().is_none(), "the child runs");
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert_eq!(wait_or_cancel(&mut child, &progress), Err(CANCELLED.to_owned()));
        assert!(child.try_wait().unwrap().is_some(), "the child was stopped");
    }

    #[test]
    fn a_curl_error_after_a_progress_line_is_kept() {
        let progress = Progress::default();
        let messages = read_curl_stderr(&b" 50  1024   50   512    0     0curl: (28) Operation timed out\n"[..], &progress);
        assert_eq!(messages, vec!["curl: (28) Operation timed out".to_owned()]);
        assert_eq!(progress.snapshot(), (512, Some(1024)));
    }

    #[test]
    fn stale_leftovers_are_cleaned_and_fresh_ones_kept() {
        let folder = scratch("stale");
        for name in ["a.zip.1.download", "a.zip.1.part", "a.zip"] {
            std::fs::write(folder.join(name), b"x").unwrap();
        }
        assert_eq!(clean_stale_files(&folder, Duration::from_secs(3600)), 0, "fresh files may belong to a running download");
        assert_eq!(clean_stale_files(&folder, Duration::ZERO), 2);
        assert!(folder.join("a.zip").exists());
        assert!(!folder.join("a.zip.1.download").exists() && !folder.join("a.zip.1.part").exists());
    }

    #[test]
    fn the_end_of_a_download_wakes_the_window_and_is_seen_at_once() {
        let downloads = Arc::new(Downloads::<&str>::default());
        let woken = Arc::new(AtomicU64::new(0));
        let counter = woken.clone();
        downloads.set_waker(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        downloads.begin("fast").unwrap();
        downloads.end("fast", Ok(PathBuf::from("f.zip")));
        assert_eq!(woken.load(Ordering::Relaxed), 1);
        assert!(!downloads.any_running());
        assert_eq!(downloads.take_finished().len(), 1);
    }
}
