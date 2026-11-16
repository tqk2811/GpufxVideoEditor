//! Preview proxies: smaller copies of large video sources that the preview decodes in place of
//! the originals. An export always reads the originals (see `ProjectPaths::video_decode_path`).
//!
//! A proxy is an H.264 file, 8-bit SDR, [`PROXY_HEIGHT`] pixels high with the source's shape,
//! already turned upright, with a keyframe every [`KEYFRAME_INTERVAL`] frames so a seek decodes few
//! frames, and no sound: the sound is still read from the original. It is made by `gpufx-render`
//! in a child process, from a one-clip project, so the colours (an HDR source included) come out
//! as the export would draw them, and a crash of the encoder does not take the editor down.
//!
//! Proxies live in `%LOCALAPPDATA%\gpufx-editor\proxies`, named after a hash of the original's
//! absolute path and one of its size and modification time, so a replaced file gets a new proxy.
//! The export writes a temporary file and renames it, so a half-made proxy never has the final
//! name. Older versions of the same original are removed once a new one is made, and files not
//! used for [`MAX_AGE`] are swept at start.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use d3d12_backend::{probe_media, probe_video_format, EncoderChoice, ExportJob, ProjectPaths, ProxyFile, ProxyTiming, VideoCodec, VideoFormat};
use engine::{ClipPlacement, Output, Project, Time, Track, TrackKind, MIN_FRAME_DURATION, SUPPORTED_VERSION};

use crate::visuals::worker::FileStamp;

/// The height of a proxy, unless the source is lower.
pub const PROXY_HEIGHT: u32 = 540;
/// Frames between two keyframes of a proxy.
pub const KEYFRAME_INTERVAL: u32 = 10;
/// In [`ProxyMode::Auto`], sources taller than this get a proxy.
pub const AUTO_MIN_HEIGHT: u32 = 1080;
/// A proxy not used for this long is removed when the editor starts.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// A temporary file left by a proxy that did not finish is removed after this long.
const PARTIAL_AGE: Duration = Duration::from_secs(24 * 3600);
const FOLDER: &str = "gpufx-editor";
const PROXIES: &str = "proxies";
const EXTENSION: &str = "mp4";
/// How often the worker reads the progress of the child.
const PROGRESS_POLL: Duration = Duration::from_millis(100);

/// Which sources get a proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    /// None: the preview decodes the originals.
    #[default]
    Off,
    /// Videos taller than [`AUTO_MIN_HEIGHT`].
    Auto,
    /// Every video.
    On,
}

impl ProxyMode {
    pub const ALL: [ProxyMode; 3] = [ProxyMode::Off, ProxyMode::Auto, ProxyMode::On];

    /// The name in the settings file.
    pub fn code(self) -> &'static str {
        match self {
            ProxyMode::Off => "off",
            ProxyMode::Auto => "auto",
            ProxyMode::On => "on",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.code() == code)
    }

    /// Whether a video of `format` gets a proxy in this mode.
    pub fn wants(self, format: &VideoFormat) -> bool {
        match self {
            ProxyMode::Off => false,
            ProxyMode::Auto => format.height > AUTO_MIN_HEIGHT,
            ProxyMode::On => true,
        }
    }
}

/// `%LOCALAPPDATA%\gpufx-editor\proxies`, when the variable is set.
pub fn proxy_folder() -> Option<PathBuf> {
    let folder = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(folder).join(FOLDER).join(PROXIES))
}

/// FNV-1a: a hash that stays the same across Rust versions, unlike the standard hasher.
fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The part of a proxy name that stands for `original`: its absolute path, lower-cased as Windows
/// compares paths.
pub fn path_key(original: &Path) -> String {
    let absolute = std::path::absolute(original).unwrap_or_else(|_| original.to_path_buf());
    let text = absolute.to_string_lossy().replace('/', "\\").to_lowercase();
    format!("{:016x}", fnv1a(text.bytes()))
}

/// The part of a proxy name that stands for one version of the original.
pub fn stamp_key(stamp: FileStamp) -> String {
    let nanos = stamp.modified.and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |since| since.as_nanos());
    format!("{:016x}", fnv1a(stamp.size.to_le_bytes().into_iter().chain(nanos.to_le_bytes())))
}

/// The proxy of this version of `original` in `folder`.
pub fn proxy_file(folder: &Path, original: &Path, stamp: FileStamp) -> PathBuf {
    folder.join(format!("{}-{}.{EXTENSION}", path_key(original), stamp_key(stamp)))
}

/// Whether `name` is a finished proxy: `<16 hex>-<16 hex>.mp4`.
fn is_proxy_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".mp4") else { return false };
    let hex = |part: &str| part.len() == 16 && part.bytes().all(|b| b.is_ascii_hexdigit());
    stem.split_once('-').is_some_and(|(path, stamp)| hex(path) && hex(stamp))
}

/// Removes the other finished proxies of the original `keep` was made from: older versions of it.
pub fn remove_other_versions(folder: &Path, keep: &Path) {
    let Some(keep_name) = keep.file_name().map(|name| name.to_string_lossy().into_owned()) else { return };
    let Some((prefix, _)) = keep_name.split_once('-') else { return };
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != keep_name && is_proxy_name(&name) && name.starts_with(&format!("{prefix}-")) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Removes proxies not used for `max_age` and temporary files of proxies that did not finish.
/// Returns how many files went.
pub fn sweep(folder: &Path, now: SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let age = entry.metadata().and_then(|meta| meta.modified()).ok().and_then(|modified| now.duration_since(modified).ok()).unwrap_or_default();
        let limit = if is_proxy_name(&name) {
            max_age
        } else if name.ends_with(".tmp.mp4") {
            PARTIAL_AGE
        } else {
            continue;
        };
        if age > limit && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Marks a proxy as used now, so the sweep keeps it.
fn touch(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

fn even(value: f64) -> u32 {
    ((value / 2.0).round() as u32).max(1) * 2
}

/// The frame size of the proxy of a video of `format`: [`PROXY_HEIGHT`] high (or the source's own
/// height, made even, when it is lower) with the source's shape, both sides even.
pub fn proxy_size(format: &VideoFormat) -> (u32, u32) {
    let height = even(f64::from(format.height.min(PROXY_HEIGHT)));
    let aspect = f64::from(format.width.max(1)) / f64::from(format.height.max(1));
    (even(aspect * f64::from(height)), height)
}

/// The placement that covers the whole proxy frame with the source, cropping the sliver that even
/// sizes cost rather than leaving a thin bar at an edge.
fn covering_placement(format: &VideoFormat, width: u32, height: u32) -> ClipPlacement {
    let aspect = f64::from(format.width.max(1)) / f64::from(format.height.max(1));
    let frame_aspect = f64::from(width) / f64::from(height);
    // The picture's height in parts of the frame is width_part * frame_aspect / aspect.
    let width_part = (aspect / frame_aspect).max(1.0);
    let height_part = width_part * frame_aspect / aspect;
    ClipPlacement { x: (1.0 - width_part) / 2.0, y: (1.0 - height_part) / 2.0, width: width_part }
}

/// The frame step of the proxy of a video of `format`: one frame of the source rounded down to
/// whole ticks (see [`VideoFormat::frame_duration`]), so the proxy samples every frame of a
/// constant-rate source. A source above 60 fps gets 60, the fastest a project may run, and one
/// that states no rate at all 30.
pub fn proxy_frame_duration(format: &VideoFormat) -> Time {
    format.frame_duration.map_or(Time::from_ticks(3000), |ticks| ticks.max(MIN_FRAME_DURATION))
}

/// What the preview needs to read the proxy at `path` of a video of `format` in its place: the
/// original's shape, since the proxy's even sides round it, and, for a constant-rate source the
/// proxy samples at every frame, how the original's times map to proxy frames. Without that map
/// (a variable-rate source, or one above 60 fps) the proxy is read at the original's time and
/// shows the source as sampled at most one proxy frame earlier.
pub fn preview_file(path: PathBuf, format: &VideoFormat) -> ProxyFile {
    let frame_duration = proxy_frame_duration(format);
    let timing = format.rate.filter(|_| format.constant_rate && format.frame_duration == Some(frame_duration)).map(|rate| ProxyTiming { rate, time_base: format.time_base, frame_duration });
    ProxyFile { path, display_aspect: Some(format.aspect), timing }
}

/// The one-clip project whose export is the proxy of `original`, which runs `duration`.
pub fn proxy_project(original: &Path, format: &VideoFormat, duration: Time) -> Project {
    let (width, height) = proxy_size(format);
    let frame_duration = proxy_frame_duration(format);
    let mut clip = crate::editor::new_clip(&original.to_string_lossy(), Time::ZERO, duration);
    clip.placement = Some(covering_placement(format, width, height));
    clip.muted = true;
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width, height, frame_duration, audio: None },
        tracks: vec![Track { kind: TrackKind::Video, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false, subtitle: None }],
        markers: Vec::new(),
        beats: Default::default(),
    }
}

/// Where the proxy of one source stands.
#[derive(Debug, Clone, PartialEq)]
pub enum ProxyStatus {
    /// Waiting for the worker.
    Queued,
    /// Being made, this far from 0 to 1.
    Running(f32),
    Ready(PathBuf),
    /// The mode does not want one for this source.
    NotNeeded,
    /// Stopped by the user; asked for again only after the mode changes.
    Cancelled,
    Failed(String),
}

/// How making one proxy ended.
#[derive(Debug, Clone, PartialEq)]
pub enum MakeOutcome {
    Done,
    Cancelled,
    Failed(String),
}

/// The file work behind the proxies. A trait so the queue can be tested without FFmpeg.
pub trait ProxyMaker: Send + Sync + 'static {
    fn stamp(&self, path: &Path) -> Option<FileStamp>;
    /// The picture of the video and how long it runs.
    fn inspect(&self, path: &Path) -> Result<(VideoFormat, Time), String>;
    /// Writes the proxy of `original` to `out`, calling `progress` with 0 to 1 now and then.
    fn make(&self, original: &Path, format: &VideoFormat, duration: Time, out: &Path, cancel: &Arc<AtomicBool>, progress: &dyn Fn(f32)) -> MakeOutcome;
}

/// Makes proxies with `gpufx-render` in a child process.
pub struct ChildProxyMaker {
    pub tool: PathBuf,
    pub package_roots: Vec<PathBuf>,
}

impl ProxyMaker for ChildProxyMaker {
    fn stamp(&self, path: &Path) -> Option<FileStamp> {
        file_stamp(path)
    }

    fn inspect(&self, path: &Path) -> Result<(VideoFormat, Time), String> {
        let probe = probe_media(path)?;
        if !probe.has_video {
            return Err(probe.video_problem.unwrap_or_else(|| "no video stream".to_owned()));
        }
        let duration = probe.video_duration.ok_or("the video states no length")?;
        Ok((probe_video_format(path)?, duration))
    }

    fn make(&self, original: &Path, format: &VideoFormat, duration: Time, out: &Path, cancel: &Arc<AtomicBool>, progress: &dyn Fn(f32)) -> MakeOutcome {
        let project = proxy_project(original, format, duration);
        let total = engine::frame_count(&project).max(1);
        let job = ExportJob {
            project,
            project_file: None,
            paths: Arc::new(ProjectPaths::new(Path::new("proxy.json"), self.package_roots.clone())),
            out: out.to_path_buf(),
            encoder: EncoderChoice::Auto(VideoCodec::H264),
            hashes: None,
            keyframe_interval: Some(KEYFRAME_INTERVAL),
        };
        let done = AtomicI64::new(0);
        let finished = AtomicBool::new(false);
        let outcome = std::thread::scope(|scope| {
            let runner = scope.spawn(|| {
                let outcome = crate::export::child::run_child(&job, &self.package_roots, &self.tool, cancel, &done, crate::export::child::CANCEL_GRACE);
                finished.store(true, Ordering::SeqCst);
                outcome
            });
            while !finished.load(Ordering::SeqCst) {
                progress(done.load(Ordering::SeqCst) as f32 / total as f32);
                std::thread::sleep(PROGRESS_POLL);
            }
            runner.join()
        });
        match outcome {
            Ok(crate::export::ExportOutcome::Finished(_)) => MakeOutcome::Done,
            Ok(crate::export::ExportOutcome::Cancelled) => MakeOutcome::Cancelled,
            Ok(crate::export::ExportOutcome::Failed(message)) => MakeOutcome::Failed(crate::i18n::Language::English.t(message)),
            Err(_) => MakeOutcome::Failed("the proxy thread panicked".to_owned()),
        }
    }
}

/// The size and modification time of `path`.
pub fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp { size: meta.len(), modified: meta.modified().ok() })
}

/// What the manager shares with its worker.
#[derive(Default)]
struct State {
    mode: ProxyMode,
    entries: HashMap<PathBuf, ProxyStatus>,
    /// How the preview reads each ready proxy.
    files: HashMap<PathBuf, ProxyFile>,
    queue: VecDeque<PathBuf>,
    /// The source being made and how to stop it.
    running: Option<(PathBuf, Arc<AtomicBool>)>,
    /// Goes up whenever the set of ready proxies in use changes.
    revision: u64,
    /// Bumped by a mode change, so a result made for an old mode is dropped.
    generation: u64,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn notify_ui(&self) {
        if let Some(waker) = &self.waker {
            waker();
        }
    }
}

/// The proxies of this session: which sources have one, a queue of those still to make, made one
/// at a time on a thread of its own.
pub struct ProxyManager {
    shared: Arc<Shared>,
    folder: Option<PathBuf>,
}

impl ProxyManager {
    /// Starts the worker. Without a folder nothing is made and every source stays on its original.
    pub fn start<M: ProxyMaker>(maker: M, folder: Option<PathBuf>, mode: ProxyMode, waker: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let shared = Arc::new(Shared { state: Mutex::new(State { mode, ..State::default() }), wake: Condvar::new(), waker });
        if let Some(folder) = folder.clone() {
            let worker = shared.clone();
            std::thread::Builder::new().name("proxy".to_owned()).spawn(move || work(&worker, &maker, &folder)).expect("spawn the proxy thread");
        }
        Self { shared, folder }
    }

    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    pub fn mode(&self) -> ProxyMode {
        self.shared.lock().mode
    }

    /// Changes which sources get a proxy. What is queued or running stops; ready proxies are kept
    /// and are used again once the mode is not Off.
    pub fn set_mode(&self, mode: ProxyMode) {
        let mut state = self.shared.lock();
        if state.mode == mode {
            return;
        }
        state.mode = mode;
        state.generation += 1;
        state.queue.clear();
        if let Some((_, cancel)) = state.running.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        state.entries.retain(|_, status| matches!(status, ProxyStatus::Ready(_)));
        state.revision += 1;
    }

    /// Queues the sources that have no status yet. Called with the video sources of the project,
    /// as absolute paths, whenever they may have changed; the cost is a lookup per source.
    pub fn request<'a>(&self, sources: impl IntoIterator<Item = &'a Path>) {
        if self.folder.is_none() {
            return;
        }
        let mut state = self.shared.lock();
        if state.mode == ProxyMode::Off {
            return;
        }
        let mut added = false;
        for source in sources {
            if !state.entries.contains_key(source) {
                state.entries.insert(source.to_path_buf(), ProxyStatus::Queued);
                state.queue.push_back(source.to_path_buf());
                added = true;
            }
        }
        drop(state);
        if added {
            self.shared.wake.notify_all();
        }
    }

    /// Stops what is queued or running. Those sources are not asked for again until the mode changes.
    pub fn cancel_all(&self) {
        let mut state = self.shared.lock();
        let queued: Vec<PathBuf> = state.queue.drain(..).collect();
        for source in queued {
            state.entries.insert(source, ProxyStatus::Cancelled);
        }
        if let Some((source, cancel)) = state.running.take() {
            cancel.store(true, Ordering::SeqCst);
            state.entries.insert(source, ProxyStatus::Cancelled);
        }
    }

    pub fn status(&self, source: &Path) -> Option<ProxyStatus> {
        self.shared.lock().entries.get(source).cloned()
    }

    /// How many proxies are queued or being made, and the progress of the running one.
    pub fn pending(&self) -> (usize, Option<f32>) {
        let state = self.shared.lock();
        let running = state.running.as_ref().and_then(|(source, _)| match state.entries.get(source) {
            Some(ProxyStatus::Running(progress)) => Some(*progress),
            _ => None,
        });
        (state.queue.len() + usize::from(state.running.is_some()), running)
    }

    /// Changes whenever [`Self::proxies`] would give another map.
    pub fn revision(&self) -> u64 {
        self.shared.lock().revision
    }

    /// The ready proxies to decode in place of their originals; empty while the mode is Off.
    pub fn proxies(&self) -> HashMap<PathBuf, ProxyFile> {
        let state = self.shared.lock();
        if state.mode == ProxyMode::Off {
            return HashMap::new();
        }
        state.entries.iter().filter_map(|(source, status)| match status {
            ProxyStatus::Ready(_) => state.files.get(source).map(|file| (source.clone(), file.clone())),
            _ => None,
        }).collect()
    }
}

impl Drop for ProxyManager {
    /// Stops the child of a running proxy. The worker itself is never joined: it may be in a probe
    /// on a dead network drive, and the process exit ends it.
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.queue.clear();
        state.generation += 1;
        if let Some((_, cancel)) = state.running.take() {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// Sets the status of `source`, if the mode it was made for still holds.
fn settle(shared: &Shared, source: &Path, generation: u64, (status, file): (ProxyStatus, Option<ProxyFile>)) {
    let mut state = shared.lock();
    if state.generation != generation {
        return;
    }
    if matches!(state.entries.get(source), Some(ProxyStatus::Cancelled)) && !matches!(status, ProxyStatus::Ready(_)) {
        return;
    }
    let ready = matches!(status, ProxyStatus::Ready(_));
    state.entries.insert(source.to_path_buf(), status);
    match file {
        Some(file) => state.files.insert(source.to_path_buf(), file),
        None => state.files.remove(source),
    };
    if state.running.as_ref().is_some_and(|(running, _)| running == source) {
        state.running = None;
    }
    if ready {
        state.revision += 1;
    }
    drop(state);
    shared.notify_ui();
}

fn work<M: ProxyMaker>(shared: &Shared, maker: &M, folder: &Path) {
    let _ = std::fs::create_dir_all(folder);
    sweep(folder, SystemTime::now(), MAX_AGE);
    loop {
        let (source, generation, mode, cancel) = {
            let mut state = shared.lock();
            loop {
                if let Some(source) = state.queue.pop_front() {
                    let cancel = Arc::new(AtomicBool::new(false));
                    state.running = Some((source.clone(), cancel.clone()));
                    break (source, state.generation, state.mode, cancel);
                }
                state = shared.wake.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let status = make_one(shared, maker, folder, &source, generation, mode, &cancel);
        settle(shared, &source, generation, status);
    }
}

fn make_one<M: ProxyMaker>(shared: &Shared, maker: &M, folder: &Path, source: &Path, generation: u64, mode: ProxyMode, cancel: &Arc<AtomicBool>) -> (ProxyStatus, Option<ProxyFile>) {
    let failed = |message: String| (ProxyStatus::Failed(message), None);
    let Some(stamp) = maker.stamp(source) else { return failed(format!("{} cannot be read", source.display())) };
    // The source's shape and frame rate are needed to read even a proxy made earlier.
    let (format, duration) = match maker.inspect(source) {
        Ok(found) => found,
        Err(message) => return failed(message),
    };
    if !mode.wants(&format) {
        return (ProxyStatus::NotNeeded, None);
    }
    let target = proxy_file(folder, source, stamp);
    if target.is_file() {
        touch(&target);
        return (ProxyStatus::Ready(target.clone()), Some(preview_file(target, &format)));
    }
    let progress = |fraction: f32| {
        let mut state = shared.lock();
        if state.generation == generation && matches!(state.entries.get(source), Some(ProxyStatus::Queued | ProxyStatus::Running(_))) {
            state.entries.insert(source.to_path_buf(), ProxyStatus::Running(fraction.clamp(0.0, 1.0)));
            drop(state);
            shared.notify_ui();
        }
    };
    progress(0.0);
    match maker.make(source, &format, duration, &target, cancel, &progress) {
        MakeOutcome::Done if target.is_file() => {
            remove_other_versions(folder, &target);
            (ProxyStatus::Ready(target.clone()), Some(preview_file(target, &format)))
        }
        MakeOutcome::Done => failed(format!("{} was not written", target.display())),
        MakeOutcome::Cancelled => (ProxyStatus::Cancelled, None),
        MakeOutcome::Failed(message) => failed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::time::Instant;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-proxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch folder");
        dir
    }

    fn format(width: u32, height: u32) -> VideoFormat {
        VideoFormat { width, height, aspect: f64::from(width) / f64::from(height), rate: Some((30, 1)), constant_rate: true, time_base: (1, 15360), frame_duration: Some(Time::from_ticks(3000)) }
    }

    fn stamp(size: u64, seconds: u64) -> FileStamp {
        FileStamp { size, modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)) }
    }

    #[test]
    fn the_mode_round_trips_through_its_code_and_picks_the_sources() {
        for mode in ProxyMode::ALL {
            assert_eq!(ProxyMode::from_code(mode.code()), Some(mode));
        }
        assert_eq!(ProxyMode::from_code("sometimes"), None);
        assert!(!ProxyMode::Off.wants(&format(3840, 2160)));
        assert!(ProxyMode::Auto.wants(&format(3840, 2160)));
        assert!(ProxyMode::Auto.wants(&format(1080, 1920)), "a tall phone video");
        assert!(!ProxyMode::Auto.wants(&format(1920, 1080)), "1080 itself is not taller than 1080");
        assert!(ProxyMode::On.wants(&format(640, 360)));
    }

    #[test]
    fn a_proxy_name_follows_the_path_and_the_version_of_the_file() {
        let folder = Path::new(r"C:\proxies");
        let a = proxy_file(folder, Path::new(r"D:\media\a.mp4"), stamp(10, 1));
        assert_eq!(a, proxy_file(folder, Path::new(r"d:\MEDIA\A.mp4"), stamp(10, 1)), "Windows paths compare without case");
        assert_eq!(a, proxy_file(folder, Path::new("D:/media/a.mp4"), stamp(10, 1)), "either slash");
        assert_ne!(a, proxy_file(folder, Path::new(r"D:\media\b.mp4"), stamp(10, 1)), "another file");
        assert_ne!(a, proxy_file(folder, Path::new(r"D:\media\a.mp4"), stamp(11, 1)), "another size");
        assert_ne!(a, proxy_file(folder, Path::new(r"D:\media\a.mp4"), stamp(10, 2)), "another time");
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(is_proxy_name(&name), "{name}");
        assert!(name.starts_with(&path_key(Path::new(r"D:\media\a.mp4"))));
        assert!(!is_proxy_name("0123456789abcdef-0123456789abcdef.4242-0.tmp.mp4"), "a temporary file");
        assert!(!is_proxy_name("notes.mp4"));
    }

    #[test]
    fn a_new_version_removes_the_older_ones_of_the_same_file_only() {
        let folder = scratch("versions");
        let old = proxy_file(&folder, Path::new(r"D:\a.mp4"), stamp(1, 1));
        let new = proxy_file(&folder, Path::new(r"D:\a.mp4"), stamp(2, 2));
        let other = proxy_file(&folder, Path::new(r"D:\b.mp4"), stamp(1, 1));
        let partial = folder.join(format!("{}.77-0.tmp.mp4", new.file_stem().unwrap().to_string_lossy()));
        for file in [&old, &new, &other, &partial] {
            std::fs::write(file, b"x").unwrap();
        }
        remove_other_versions(&folder, &new);
        assert!(!old.exists(), "the old version goes");
        assert!(new.exists() && other.exists(), "the new one and another file's stay");
        assert!(partial.exists(), "a temporary file another run may be writing stays");
    }

    #[test]
    fn the_sweep_removes_old_proxies_and_old_partials_only() {
        let folder = scratch("sweep");
        let proxy = proxy_file(&folder, Path::new(r"D:\a.mp4"), stamp(1, 1));
        let partial = folder.join("0123456789abcdef-0123456789abcdef.1-0.tmp.mp4");
        let foreign = folder.join("keep.txt");
        for file in [&proxy, &partial, &foreign] {
            std::fs::write(file, b"x").unwrap();
        }
        let now = SystemTime::now();
        assert_eq!(sweep(&folder, now, MAX_AGE), 0, "everything is fresh");
        assert_eq!(sweep(&folder, now + PARTIAL_AGE + Duration::from_secs(60), MAX_AGE), 1, "a day later the partial goes");
        assert!(proxy.exists() && !partial.exists());
        assert_eq!(sweep(&folder, now + MAX_AGE + Duration::from_secs(60), MAX_AGE), 1, "a month later the proxy goes");
        assert!(!proxy.exists() && foreign.exists(), "files that are not proxies are left alone");
    }

    #[test]
    fn the_proxy_is_540_high_with_even_sides_and_the_shape_of_the_source() {
        assert_eq!(proxy_size(&format(3840, 2160)), (960, 540));
        assert_eq!(proxy_size(&format(2160, 3840)), (304, 540), "upright phone video");
        assert_eq!(proxy_size(&format(4096, 2160)), (1024, 540));
        assert_eq!(proxy_size(&format(640, 361)), (642, 362), "a low source keeps its height, made even");
        let odd = format(2048, 1080);
        let (width, height) = proxy_size(&odd);
        assert_eq!((width, height), (1024, 540));
    }

    #[test]
    fn the_proxy_clip_covers_its_frame() {
        let source = format(2000, 1125);
        let (width, height) = proxy_size(&source);
        let placement = covering_placement(&source, width, height);
        let aspect = 2000.0 / 1125.0;
        let picture_height = placement.width * f64::from(width) / f64::from(height) / aspect;
        assert!(placement.width >= 1.0 && picture_height >= 1.0 - 1e-9, "{placement:?} {picture_height}");
        assert!((placement.x + placement.width / 2.0 - 0.5).abs() < 1e-9 && (placement.y + picture_height / 2.0 - 0.5).abs() < 1e-9, "centred");
        assert!(placement.width < 1.01 && picture_height < 1.01, "crops a sliver at most");
        let project = proxy_project(Path::new(r"D:\a.mp4"), &format(3840, 2160), Time::from_ticks(90_000));
        assert_eq!((project.output.width, project.output.height), (960, 540));
        assert!(project.output.audio.is_none(), "a proxy has no sound");
        assert_eq!(project.tracks[0].clips[0].duration, Time::from_ticks(90_000));
        let fast_format = VideoFormat { rate: Some((120, 1)), frame_duration: Some(Time::from_ticks(750)), ..format(3840, 2160) };
        let fast = proxy_project(Path::new(r"D:\a.mp4"), &fast_format, Time::from_ticks(90_000));
        assert_eq!(fast.output.frame_duration, MIN_FRAME_DURATION, "120 fps becomes the 60 fps a project may have");
        assert_eq!(preview_file(PathBuf::from("p.mp4"), &fast_format).timing, None, "a capped proxy is read at the original's time");
    }

    #[test]
    fn the_preview_reads_a_proxy_with_the_shape_and_frame_times_of_its_original() {
        // 2160x3840 has a proxy of 304x540, a little wider than the source.
        let upright = format(2160, 3840);
        let file = preview_file(PathBuf::from("p.mp4"), &upright);
        assert_eq!(file.display_aspect, Some(2160.0 / 3840.0), "the original's shape, not 304/540");
        let film = VideoFormat { rate: Some((24000, 1001)), frame_duration: d3d12_backend::frame_ticks_floor(24000, 1001), ..format(3840, 2160) };
        let project = proxy_project(Path::new(r"D:\a.mp4"), &film, Time::from_ticks(90_000));
        assert_eq!(project.output.frame_duration, Time::from_ticks(3753), "one 23.976 frame, rounded down");
        assert_eq!(preview_file(PathBuf::from("p.mp4"), &film).timing, Some(ProxyTiming { rate: (24000, 1001), time_base: (1, 15360), frame_duration: Time::from_ticks(3753) }));
        let variable = VideoFormat { constant_rate: false, ..film };
        assert_eq!(preview_file(PathBuf::from("p.mp4"), &variable).timing, None, "a variable rate is read at the original's time");
        let unstated = VideoFormat { rate: None, frame_duration: None, constant_rate: false, ..film };
        assert_eq!(proxy_frame_duration(&unstated), Time::from_ticks(3000));
    }

    /// A maker driven by the test: each `make` waits for an outcome sent on the channel.
    struct FakeMaker {
        formats: HashMap<PathBuf, VideoFormat>,
        made: Sender<PathBuf>,
        outcomes: Mutex<Receiver<MakeOutcome>>,
    }

    impl ProxyMaker for FakeMaker {
        fn stamp(&self, path: &Path) -> Option<FileStamp> {
            self.formats.contains_key(path).then(|| stamp(1, 1))
        }

        fn inspect(&self, path: &Path) -> Result<(VideoFormat, Time), String> {
            Ok((self.formats[path], Time::from_ticks(90_000)))
        }

        fn make(&self, original: &Path, _: &VideoFormat, _: Time, out: &Path, cancel: &Arc<AtomicBool>, progress: &dyn Fn(f32)) -> MakeOutcome {
            progress(0.5);
            let _ = self.made.send(original.to_path_buf());
            let outcomes = self.outcomes.lock().unwrap();
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return MakeOutcome::Cancelled;
                }
                if let Ok(outcome) = outcomes.recv_timeout(Duration::from_millis(5)) {
                    if outcome == MakeOutcome::Done {
                        std::fs::write(out, b"proxy").unwrap();
                    }
                    return outcome;
                }
            }
        }
    }

    fn wait_for(manager: &ProxyManager, source: &Path, wanted: impl Fn(&ProxyStatus) -> bool) -> ProxyStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = manager.status(source).filter(|status| wanted(status)) {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "{} stayed {:?}", source.display(), manager.status(source));
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn manager(name: &str, mode: ProxyMode) -> (ProxyManager, Receiver<PathBuf>, Sender<MakeOutcome>, PathBuf) {
        let folder = scratch(name);
        let (made, made_rx) = channel();
        let (outcome_tx, outcomes) = channel();
        let formats = HashMap::from([(PathBuf::from(r"D:\big.mp4"), format(3840, 2160)), (PathBuf::from(r"D:\small.mp4"), format(1280, 720)), (PathBuf::from(r"D:\other.mp4"), format(3840, 2160))]);
        let maker = FakeMaker { formats, made, outcomes: Mutex::new(outcomes) };
        (ProxyManager::start(maker, Some(folder.clone()), mode, None), made_rx, outcome_tx, folder)
    }

    #[test]
    fn auto_makes_proxies_of_big_videos_one_at_a_time_and_maps_them_when_ready() {
        let (manager, made, outcomes, folder) = manager("queue", ProxyMode::Auto);
        let (big, small, other) = (Path::new(r"D:\big.mp4"), Path::new(r"D:\small.mp4"), Path::new(r"D:\other.mp4"));
        let revision = manager.revision();
        manager.request([big, small, other]);
        assert_eq!(made.recv_timeout(Duration::from_secs(10)).unwrap(), big, "the first in the queue");
        assert_eq!(wait_for(&manager, big, |s| matches!(s, ProxyStatus::Running(_))), ProxyStatus::Running(0.5));
        assert!(made.try_recv().is_err(), "one at a time");
        assert!(manager.proxies().is_empty(), "nothing is used before it is ready");
        outcomes.send(MakeOutcome::Done).unwrap();
        let ready = wait_for(&manager, big, |s| matches!(s, ProxyStatus::Ready(_)));
        assert_eq!(ready, ProxyStatus::Ready(proxy_file(&folder, big, stamp(1, 1))));
        assert_eq!(wait_for(&manager, small, |s| *s != ProxyStatus::Queued), ProxyStatus::NotNeeded, "720p needs none in Auto");
        assert_eq!(made.recv_timeout(Duration::from_secs(10)).unwrap(), other);
        outcomes.send(MakeOutcome::Failed("encoder broke".into())).unwrap();
        assert_eq!(wait_for(&manager, other, |s| matches!(s, ProxyStatus::Failed(_))), ProxyStatus::Failed("encoder broke".into()));
        assert_ne!(manager.revision(), revision);
        assert_eq!(manager.proxies(), HashMap::from([(big.to_path_buf(), preview_file(proxy_file(&folder, big, stamp(1, 1)), &format(3840, 2160)))]));
        manager.request([big, small, other]);
        assert!(made.recv_timeout(Duration::from_millis(50)).is_err(), "known sources are not queued again");

        manager.set_mode(ProxyMode::Off);
        assert!(manager.proxies().is_empty(), "Off decodes the originals");
        manager.request([small]);
        assert_eq!(manager.status(small), None, "Off queues nothing");
        manager.set_mode(ProxyMode::On);
        assert_eq!(manager.proxies().len(), 1, "the ready proxy is used again");
        manager.request([small]);
        assert_eq!(made.recv_timeout(Duration::from_secs(10)).unwrap(), small, "On makes one for every video");
        outcomes.send(MakeOutcome::Done).unwrap();
        wait_for(&manager, small, |s| matches!(s, ProxyStatus::Ready(_)));
    }

    #[test]
    fn an_existing_proxy_is_used_without_making_it_again() {
        let (manager, made, _outcomes, folder) = manager("existing", ProxyMode::On);
        let big = Path::new(r"D:\big.mp4");
        std::fs::write(proxy_file(&folder, big, stamp(1, 1)), b"made earlier").unwrap();
        manager.request([big]);
        assert!(matches!(wait_for(&manager, big, |s| *s != ProxyStatus::Queued), ProxyStatus::Ready(_)));
        assert!(made.try_recv().is_err());
    }

    #[test]
    fn cancelling_stops_the_running_proxy_and_the_queue() {
        let (manager, made, _outcomes, _folder) = manager("cancel", ProxyMode::On);
        let (big, other) = (Path::new(r"D:\big.mp4"), Path::new(r"D:\other.mp4"));
        manager.request([big, other]);
        assert_eq!(made.recv_timeout(Duration::from_secs(10)).unwrap(), big);
        manager.cancel_all();
        assert_eq!(wait_for(&manager, big, |s| *s == ProxyStatus::Cancelled), ProxyStatus::Cancelled);
        assert_eq!(manager.status(other), Some(ProxyStatus::Cancelled));
        assert_eq!(manager.pending().0, 0);
        manager.request([big, other]);
        assert!(made.recv_timeout(Duration::from_millis(50)).is_err(), "cancelled sources wait for a mode change");
        manager.set_mode(ProxyMode::Auto);
        manager.request([big]);
        assert_eq!(made.recv_timeout(Duration::from_secs(10)).unwrap(), big, "asked for again after a mode change");
    }
}
