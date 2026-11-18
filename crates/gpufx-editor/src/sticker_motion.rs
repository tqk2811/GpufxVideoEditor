//! The moving picture an animated sticker of the library shows while the pointer stays on it.
//!
//! The UI asks with [`StickerMotion::want`] while it draws a hovered tile; a worker thread decodes
//! small frames of the file and the UI uploads them in [`StickerMotion::begin_frame`]. Nothing here
//! reads a file on the UI thread. A few decoded stickers stay in a small LRU cache, and a decode the
//! pointer has left is cancelled between two frames.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use d3d12_backend::{animation_thumbnail, AnimatedPicture, ThumbnailLimits};
use egui::{ColorImage, TextureHandle, TextureOptions};

/// The height the frames are decoded at: a tile is rarely taller, and a larger one scales up.
pub const MOTION_HEIGHT: u32 = 192;
/// The most frames kept of one sticker; a longer loop plays its start.
pub const MOTION_MAX_FRAMES: usize = 150;
/// The most bytes the frames of one sticker may take.
pub const MOTION_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// How many decoded stickers stay in memory.
pub const MOTION_CACHE_ENTRIES: usize = 4;

/// What decodes the frames of a file; asked between frames whether to stop.
pub type Decode = dyn Fn(&Path, &dyn Fn() -> bool) -> Result<Option<AnimatedPicture>, String> + Send + Sync;

/// The frame of a loop shown `elapsed` seconds after it starts, and how many seconds until the next
/// one; `None` when the picture no longer changes. The loop repeats `loops` times, or forever when
/// it is 0, and then the last frame stays, as [`AnimatedPicture::frame_index_at`] plays a clip.
pub fn frame_at(ends_ms: &[u64], loops: u32, elapsed: f64) -> (usize, Option<f64>) {
    let Some(&length) = ends_ms.last().filter(|length| **length > 0) else { return (0, None) };
    let played = (elapsed.max(0.0) * 1000.0) as u64;
    if loops != 0 && played >= length.saturating_mul(u64::from(loops)) {
        return (ends_ms.len() - 1, None);
    }
    let ms = played % length;
    let index = ends_ms.partition_point(|end| *end <= ms).min(ends_ms.len() - 1);
    let next_in = (ends_ms[index] - ms).max(1) as f64 / 1000.0;
    (index, Some(next_in))
}

/// A cache of the `capacity` values used last, by key.
#[derive(Debug)]
pub struct Lru<V> {
    capacity: usize,
    /// Least recently used first.
    entries: VecDeque<(String, V)>,
}

impl<V> Lru<V> {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), entries: VecDeque::new() }
    }

    /// The value of `key`, which becomes the one used last.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        let index = self.entries.iter().position(|(entry, _)| entry == key)?;
        let entry = self.entries.remove(index)?;
        self.entries.push_back(entry);
        self.entries.back().map(|(_, value)| value)
    }

    /// Keeps `value` for `key`, dropping the value used least recently when full.
    pub fn insert(&mut self, key: String, value: V) {
        self.entries.retain(|(entry, _)| *entry != key);
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((key, value));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(entry, _)| entry == key)
    }
}

/// The uploaded frames of a sticker.
pub struct MotionFrames {
    pub textures: Vec<TextureHandle>,
    pub ends_ms: Vec<u64>,
    pub loops: u32,
    /// Width over height.
    pub aspect: f32,
}

impl MotionFrames {
    /// The texture shown `elapsed` seconds after the pointer settled, and the seconds until the
    /// next one.
    pub fn at(&self, elapsed: f64) -> (&TextureHandle, Option<f64>) {
        let (index, next_in) = frame_at(&self.ends_ms, self.loops, elapsed);
        (&self.textures[index.min(self.textures.len() - 1)], next_in)
    }
}

enum Entry {
    Ready(Arc<MotionFrames>),
    /// Not animated, or it could not be read: the tile keeps its poster.
    Unavailable,
}

/// What a hovered tile can show.
pub enum MotionView {
    /// The frames are being decoded; the poster stays meanwhile.
    Loading,
    Ready(Arc<MotionFrames>),
    Unavailable,
}

struct Job {
    run: u64,
    source: String,
    path: PathBuf,
}

struct Done {
    run: u64,
    source: String,
    result: Result<Option<AnimatedPicture>, String>,
}

/// The worker thread; it ends when this is dropped.
struct Worker {
    jobs: Sender<Job>,
    done: Arc<Mutex<Vec<Done>>>,
}

/// The run of the job that may go on; any other run stops at its next frame.
type Current = Arc<AtomicU64>;

impl Worker {
    fn start(decode: Arc<Decode>, current: Current) -> Self {
        let (jobs, receiver) = mpsc::channel::<Job>();
        let done = Arc::new(Mutex::new(Vec::new()));
        let results = done.clone();
        std::thread::Builder::new().name("sticker-motion".into()).spawn(move || work(receiver, &*decode, &current, &results)).expect("start the sticker motion thread");
        Self { jobs, done }
    }
}

fn work(receiver: Receiver<Job>, decode: &Decode, current: &Current, done: &Mutex<Vec<Done>>) {
    while let Ok(job) = receiver.recv() {
        if current.load(Ordering::Acquire) != job.run {
            continue;
        }
        let cancelled = || current.load(Ordering::Acquire) != job.run;
        let result = decode(&job.path, &cancelled);
        if cancelled() {
            continue;
        }
        done.lock().unwrap_or_else(|e| e.into_inner()).push(Done { run: job.run, source: job.source, result });
    }
}

fn default_decode(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Option<AnimatedPicture>, String> {
    animation_thumbnail(path, ThumbnailLimits { height: MOTION_HEIGHT, max_frames: MOTION_MAX_FRAMES, max_bytes: MOTION_MAX_BYTES }, cancelled)
}

/// The moving pictures of the library's stickers. Lives on the UI thread.
pub struct StickerMotion {
    decode: Arc<Decode>,
    worker: Option<Worker>,
    current: Current,
    next_run: u64,
    cache: Lru<Entry>,
    /// The sticker being decoded and its run.
    in_flight: Option<(String, u64)>,
    /// The sticker a tile asked for this frame.
    wanted: Option<String>,
    ctx: Option<egui::Context>,
}

impl Default for StickerMotion {
    fn default() -> Self {
        Self::with_decoder(Arc::new(default_decode))
    }
}

impl StickerMotion {
    /// Frames made by `decode`, on a thread started at the first request.
    pub fn with_decoder(decode: Arc<Decode>) -> Self {
        Self { decode, worker: None, current: Arc::new(AtomicU64::new(0)), next_run: 1, cache: Lru::new(MOTION_CACHE_ENTRIES), in_flight: None, wanted: None, ctx: None }
    }

    /// Takes finished frames and uploads them. Call once before anything is drawn.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        self.wanted = None;
        let Some(worker) = &self.worker else { return };
        let finished = std::mem::take(&mut *worker.done.lock().unwrap_or_else(|e| e.into_inner()));
        for done in finished {
            if self.in_flight.as_ref().is_none_or(|(_, run)| *run != done.run) {
                continue;
            }
            self.in_flight = None;
            let entry = match done.result {
                Ok(Some(picture)) if !picture.frames.is_empty() => Entry::Ready(Arc::new(upload(ctx, &done.source, picture))),
                _ => Entry::Unavailable,
            };
            self.cache.insert(done.source, entry);
        }
    }

    /// What the tile of `source` shows while the pointer stays on it; asks for its frames from
    /// `path` when they are not decoded yet.
    pub fn want(&mut self, source: &str, path: impl FnOnce() -> PathBuf) -> MotionView {
        self.wanted = Some(source.to_owned());
        match self.cache.get(source) {
            Some(Entry::Ready(frames)) => return MotionView::Ready(frames.clone()),
            Some(Entry::Unavailable) => return MotionView::Unavailable,
            None => {}
        }
        if self.in_flight.as_ref().is_some_and(|(current, _)| current == source) {
            return MotionView::Loading;
        }
        let run = self.next_run;
        self.next_run += 1;
        self.current.store(run, Ordering::Release);
        let worker = self.worker.get_or_insert_with(|| Worker::start(self.decode.clone(), self.current.clone()));
        if worker.jobs.send(Job { run, source: source.to_owned(), path: path() }).is_err() {
            self.cache.insert(source.to_owned(), Entry::Unavailable);
            return MotionView::Unavailable;
        }
        self.in_flight = Some((source.to_owned(), run));
        MotionView::Loading
    }

    /// Cancels the decode no tile asked for this frame: the pointer left its sticker. Call after
    /// the tiles are drawn, every frame.
    pub fn end_frame(&mut self) {
        if self.in_flight.as_ref().is_some_and(|(source, _)| self.wanted.as_deref() != Some(source)) {
            self.in_flight = None;
            // No job has run 0, so whatever runs stops.
            self.current.store(0, Ordering::Release);
        }
    }

    /// True while a sticker is being decoded.
    pub fn loading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Whether the frames of `source` are kept.
    pub fn is_cached(&self, source: &str) -> bool {
        self.cache.contains(source)
    }
}

fn upload(ctx: &egui::Context, source: &str, picture: AnimatedPicture) -> MotionFrames {
    let aspect = picture.frames.first().map_or(1.0, |frame| frame.width as f32 / frame.height.max(1) as f32);
    let textures = picture
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let image = ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
            ctx.load_texture(format!("sticker-motion:{source}#{index}"), image, TextureOptions::LINEAR)
        })
        .collect();
    MotionFrames { textures, ends_ms: picture.ends_ms, loops: picture.loops, aspect }
}

#[cfg(test)]
mod tests {
    use super::*;
    use d3d12_backend::StillPicture;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    fn picture(frames: usize) -> AnimatedPicture {
        let still = StillPicture { width: 2, height: 1, rgba: vec![255; 8], has_alpha: false };
        AnimatedPicture { frames: vec![still; frames], ends_ms: (1..=frames as u64).map(|index| index * 100).collect(), loops: 0 }
    }

    /// Runs frames until `done` holds, or fails after a few seconds.
    fn pump_until(motion: &mut StickerMotion, ctx: &egui::Context, mut done: impl FnMut(&mut StickerMotion) -> bool) {
        let start = Instant::now();
        while !done(motion) {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(5));
            motion.begin_frame(ctx);
        }
    }

    #[test]
    fn a_frame_is_chosen_by_the_time_since_the_hover_and_loops() {
        let ends = [100, 250, 400];
        assert_eq!(frame_at(&ends, 0, 0.0), (0, Some(0.1)));
        assert_eq!(frame_at(&ends, 0, 0.1), (1, Some(0.15)));
        assert_eq!(frame_at(&ends, 0, 0.399), (2, Some(0.001)));
        assert_eq!(frame_at(&ends, 0, 0.45).0, 0, "forever loops again");
        assert_eq!(frame_at(&ends, 2, 0.45).0, 0);
        assert_eq!(frame_at(&ends, 2, 0.8), (2, None), "a fixed count holds the last frame");
        assert_eq!(frame_at(&[], 0, 3.0), (0, None));
    }

    #[test]
    fn the_frame_matches_what_a_clip_plays() {
        let picture = AnimatedPicture { ends_ms: vec![100, 250, 400], loops: 3, ..picture(3) };
        for ms in (0..1500).step_by(7) {
            let ticks = ms * (engine::TICKS_PER_SECOND / 1000);
            assert_eq!(frame_at(&picture.ends_ms, picture.loops, ms as f64 / 1000.0).0, picture.frame_index_at(ticks), "at {ms} ms");
        }
    }

    #[test]
    fn the_cache_drops_the_sticker_used_least_recently() {
        let mut cache = Lru::new(2);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        assert_eq!(cache.get("a"), Some(&1));
        cache.insert("c".into(), 3);
        assert!(cache.contains("a") && cache.contains("c") && !cache.contains("b"));
        cache.insert("a".into(), 4);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("a"), Some(&4));
    }

    #[test]
    fn a_hovered_sticker_is_decoded_off_the_ui_thread_and_kept() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ui_thread = std::thread::current().id();
        let counted = calls.clone();
        let mut motion = StickerMotion::with_decoder(Arc::new(move |_: &Path, _: &dyn Fn() -> bool| {
            assert_ne!(std::thread::current().id(), ui_thread);
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Some(picture(3)))
        }));
        let ctx = egui::Context::default();
        motion.begin_frame(&ctx);
        assert!(matches!(motion.want("a", || "a.webp".into()), MotionView::Loading));
        motion.end_frame();
        pump_until(&mut motion, &ctx, |motion| {
            let ready = matches!(motion.want("a", || "a.webp".into()), MotionView::Ready(_));
            motion.end_frame();
            ready
        });
        let MotionView::Ready(frames) = motion.want("a", || "a.webp".into()) else { panic!("ready") };
        assert_eq!(frames.textures.len(), 3);
        assert_eq!(frames.aspect, 2.0);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "decoded once, then from the cache");
    }

    #[test]
    fn leaving_a_sticker_cancels_its_decode_and_keeps_nothing() {
        let (started_tx, started) = mpsc::channel();
        let (stopped_tx, stopped) = mpsc::channel();
        let mut motion = StickerMotion::with_decoder(Arc::new(move |_: &Path, cancelled: &dyn Fn() -> bool| {
            started_tx.send(()).unwrap();
            let start = Instant::now();
            while !cancelled() {
                assert!(start.elapsed() < Duration::from_secs(5), "never cancelled");
                std::thread::sleep(Duration::from_millis(1));
            }
            stopped_tx.send(()).unwrap();
            Ok(Some(picture(2)))
        }));
        let ctx = egui::Context::default();
        motion.begin_frame(&ctx);
        motion.want("a", || "a.webp".into());
        motion.end_frame();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        // A frame where no tile asks: the pointer left.
        motion.begin_frame(&ctx);
        motion.end_frame();
        stopped.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!motion.loading());
        std::thread::sleep(Duration::from_millis(20));
        motion.begin_frame(&ctx);
        assert!(!motion.is_cached("a"), "a cancelled decode is not kept");
    }

    #[test]
    fn a_file_that_does_not_move_keeps_its_poster() {
        let mut motion = StickerMotion::with_decoder(Arc::new(|_: &Path, _: &dyn Fn() -> bool| Ok(None)));
        let ctx = egui::Context::default();
        motion.begin_frame(&ctx);
        motion.want("a", || "a.png".into());
        pump_until(&mut motion, &ctx, |motion| motion.is_cached("a"));
        assert!(matches!(motion.want("a", || "a.png".into()), MotionView::Unavailable));
    }

    #[test]
    fn the_real_decoder_reads_the_fixture_small() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../d3d12-backend/tests/fixtures/animated.webp");
        let picture = default_decode(&fixture, &|| false).unwrap().expect("animated");
        assert!(picture.frames.len() > 1 && picture.frames.len() <= MOTION_MAX_FRAMES);
        assert!(picture.frames.iter().all(|frame| frame.height <= MOTION_HEIGHT));
    }
}
