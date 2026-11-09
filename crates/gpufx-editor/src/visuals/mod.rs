//! Pictures and level bars of clips, made in the background and kept in memory.
//!
//! The UI asks with [`ClipVisuals::tile`] and [`ClipVisuals::peaks`] while it draws: nothing here
//! touches a file on the UI thread. What is not ready shows as a placeholder and is asked for,
//! and each frame replaces the waiting jobs, so only what is on screen is ever generated.
//!
//! Tiles are keyed by source identity and bucket time, and an identity covers the path, size and
//! modification time, so a replaced file gets new pictures. The cache is in memory only.

pub mod backend;
pub mod cache;
pub mod levels;
pub mod plan;
pub mod worker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use d3d12_backend::{ProjectPaths, ThumbnailError, ThumbnailImage};
use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use engine::{Project, Time, TICKS_PER_SECOND};

use backend::FfmpegGenerator;
use cache::{TileCache, TileKey};
use levels::PeakData;
use plan::bucket_time;
use worker::{Done, FileStamp, Generator, Job, JobKey, SourceFacts, Waker, WorkerPool};

/// About 38 MB of 128x72 pictures, far more than a screen shows.
const TILE_CACHE_ENTRIES: usize = 1024;
/// Level data of a source that is not on screen is dropped beyond this.
const PEAK_BYTES_LIMIT: usize = 64 * 1024 * 1024;
/// Textures uploaded per UI frame, so a burst of finished pictures costs little.
const UPLOADS_PER_FRAME: usize = 6;
/// How often a visible source is checked for having been replaced.
const STAMP_INTERVAL: Duration = Duration::from_secs(5);
const FRAME_WORKERS: usize = 2;
const AUDIO_WORKERS: usize = 1;
/// The most picture jobs asked for at once; the screen cannot show more.
const MAX_WANTED_FRAMES: usize = 128;
/// A tile not ready borrows a neighbour within this many steps.
const BORROW_STEPS: i64 = 4;
/// How often a tile that ran out of time is tried again before it counts as failed.
const MAX_TIMEOUT_RETRIES: u32 = 3;
/// The wait before the first of those tries, doubling for each one after it.
const TIMEOUT_RETRY_DELAY: Duration = Duration::from_secs(5);

/// What is known of a source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceView {
    Pending,
    Failed,
    Known(SourceFacts),
}

enum Tile {
    /// No picture can be made; never asked for again until the file changes.
    Failed,
    /// The decoder ran out of time, which may only mean a busy machine: asked for again after
    /// `not_before`, a few times.
    Retry { attempts: u32, not_before: Instant },
    Uploading(ColorImage),
    Ready(TextureHandle),
}

struct Slot {
    path: Arc<Path>,
    stamp: Option<FileStamp>,
    identity: Option<u32>,
    view: SourceView,
    checked: Option<Instant>,
    /// The last check could not read the file: a failure made since may have been the drive.
    stamp_lost: bool,
}

struct PeakEntry {
    data: PeakData,
    finished: bool,
    used: u64,
}

type PoolFactory = Box<dyn Fn(Option<Waker>) -> WorkerPool>;

/// All cached visuals of the session. Lives on the UI thread.
pub struct ClipVisuals {
    factory: PoolFactory,
    pool: Option<WorkerPool>,
    paused: bool,
    waker: Option<Waker>,
    paths: Option<Arc<ProjectPaths>>,
    /// Source text as the clip stores it, to its slot, without normalizing on every frame.
    interned: HashMap<String, usize>,
    by_path: HashMap<String, usize>,
    slots: Vec<Slot>,
    next_identity: u32,
    live_identities: HashSet<u32>,
    tiles: TileCache<Tile>,
    peaks: HashMap<u32, PeakEntry>,
    /// The current run of the levels of an identity; results of any other run are ignored.
    peak_runs: HashMap<u32, u32>,
    peak_bytes_limit: usize,
    uploads: VecDeque<TileKey>,
    wanted_frames: Vec<Job>,
    wanted_audio: Vec<Job>,
    wanted_keys: HashSet<JobKey>,
    published: Vec<JobKey>,
    clock: u64,
    project_revision: Option<u64>,
    /// Asks for a repaint when a tile waiting to be tried again falls due.
    ctx: Option<egui::Context>,
}

impl Default for ClipVisuals {
    fn default() -> Self {
        Self::with_generator(Arc::new(FfmpegGenerator))
    }
}

impl ClipVisuals {
    /// Visuals made by `generator`, on threads started at the first request.
    pub fn with_generator<G: Generator>(generator: Arc<G>) -> Self {
        Self {
            factory: Box::new(move |waker| WorkerPool::start(generator.clone(), FRAME_WORKERS, AUDIO_WORKERS, waker)),
            pool: None,
            paused: false,
            waker: None,
            paths: None,
            interned: HashMap::new(),
            by_path: HashMap::new(),
            slots: Vec::new(),
            next_identity: 0,
            live_identities: HashSet::new(),
            tiles: TileCache::new(TILE_CACHE_ENTRIES),
            peaks: HashMap::new(),
            peak_runs: HashMap::new(),
            peak_bytes_limit: PEAK_BYTES_LIMIT,
            uploads: VecDeque::new(),
            wanted_frames: Vec::new(),
            wanted_audio: Vec::new(),
            wanted_keys: HashSet::new(),
            published: Vec::new(),
            clock: 0,
            project_revision: None,
            ctx: None,
        }
    }

    /// `waker` is called from worker threads when results wait to be drawn.
    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
    }

    /// Starts no new work while playback or an export needs the machine.
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused != paused {
            self.paused = paused;
            if let Some(pool) = &self.pool {
                pool.set_paused(paused);
            }
        }
    }

    /// Whether new work is held back.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// How sources resolve from now on.
    pub fn set_paths(&mut self, paths: Arc<ProjectPaths>) {
        self.paths = Some(paths);
        self.interned.clear();
    }

    /// Takes finished work and uploads a few textures. Call once before anything is drawn.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.clock += 1;
        self.ctx = Some(ctx.clone());
        if let Some(pool) = &self.pool {
            for done in pool.take_done() {
                self.apply(done);
            }
        }
        let mut uploaded = 0;
        while uploaded < UPLOADS_PER_FRAME {
            let Some(key) = self.uploads.pop_front() else { break };
            if self.upload(ctx, key) {
                uploaded += 1;
            }
        }
        if !self.uploads.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Hands what was asked for this frame to the workers.
    pub fn end_frame(&mut self) {
        let frames = std::mem::take(&mut self.wanted_frames);
        let audio = std::mem::take(&mut self.wanted_audio);
        self.wanted_keys.clear();
        let keys: Vec<JobKey> = frames.iter().chain(&audio).map(|job| job.key.clone()).collect();
        if keys == self.published {
            return;
        }
        let pool = self.pool.get_or_insert_with(|| {
            let pool = (self.factory)(self.waker.clone());
            pool.set_paused(self.paused);
            pool
        });
        pool.publish(frames, audio);
        self.published = keys;
    }

    /// Stops reading levels of sources the project no longer uses.
    pub fn sync_project(&mut self, revision: u64, project: &Project) {
        if self.project_revision == Some(revision) {
            return;
        }
        self.project_revision = Some(revision);
        let mut used = HashSet::new();
        for clip in project.tracks.iter().flat_map(|track| &track.clips) {
            let slot = self.slot(&clip.source);
            if let Some(identity) = self.slots[slot].identity {
                used.insert(identity);
            }
        }
        let abandoned: Vec<u32> = self.peaks.iter().filter(|(identity, entry)| !entry.finished && !used.contains(identity)).map(|(identity, _)| *identity).collect();
        for identity in abandoned {
            self.peaks.remove(&identity);
            self.cancel_peak_run(identity);
        }
    }

    /// Ends the current run of the levels of `identity`; asking again reads from the start.
    fn cancel_peak_run(&mut self, identity: u32) {
        let run = self.peak_runs.entry(identity).or_insert(0);
        let cancelled = *run;
        *run += 1;
        if let Some(pool) = &self.pool {
            pool.cancel_peaks(identity, cancelled);
        }
    }

    /// True while pictures or levels are still being made, for the frame report.
    pub fn generating(&self) -> bool {
        !self.uploads.is_empty() || self.pool.as_ref().is_some_and(|pool| pool.outstanding() > 0)
    }

    /// The slot of `source`. Checks once in a while, off this thread, whether the file changed.
    pub fn slot(&mut self, source: &str) -> usize {
        let slot = match self.interned.get(source) {
            Some(slot) => *slot,
            None => self.intern(source),
        };
        let now = Instant::now();
        let due = self.slots[slot].view != SourceView::Pending && self.slots[slot].checked.is_none_or(|at| now.duration_since(at) >= STAMP_INTERVAL);
        if due {
            // Marked as checked when the answer comes: this request can be replaced before a
            // worker is free, and the next frame then asks again.
            self.want(Job { key: JobKey::Stamp(slot as u32), path: self.slots[slot].path.clone() }, true);
        }
        slot
    }

    fn intern(&mut self, source: &str) -> usize {
        let path = self.paths.as_ref().map_or_else(|| Path::new(source).to_path_buf(), |paths| paths.source_path(source));
        let normalized = normalize(&path);
        let slot = match self.by_path.get(&normalized) {
            Some(slot) => *slot,
            None => {
                let slot = self.slots.len();
                self.slots.push(Slot { path: path.into(), stamp: None, identity: None, view: SourceView::Pending, checked: None, stamp_lost: false });
                self.by_path.insert(normalized, slot);
                slot
            }
        };
        self.interned.insert(source.to_owned(), slot);
        slot
    }

    /// What is known of the source of `slot`, and asks for it when nothing is yet.
    pub fn view(&mut self, slot: usize) -> SourceView {
        if self.slots[slot].view == SourceView::Pending {
            self.want(Job { key: JobKey::Inspect(slot as u32), path: self.slots[slot].path.clone() }, true);
        }
        self.slots[slot].view
    }

    /// The picture for bucket time `bucket` (grid `step`), or a neighbour already made, or `None`.
    pub fn tile(&mut self, slot: usize, bucket: i64, step: i64) -> Option<TextureId> {
        let identity = self.slots[slot].identity?;
        let key = (identity, bucket);
        let now = Instant::now();
        let ask = match self.tiles.touch(key) {
            Some(Tile::Ready(handle)) => return Some(handle.id()),
            Some(Tile::Retry { not_before, .. }) if *not_before > now => {
                if let Some(ctx) = &self.ctx {
                    ctx.request_repaint_after(not_before.saturating_duration_since(now));
                }
                false
            }
            Some(Tile::Retry { .. }) | None => true,
            Some(_) => false,
        };
        if ask && self.wanted_frames.len() < MAX_WANTED_FRAMES {
            self.want(Job { key: JobKey::Frame(identity, bucket), path: self.slots[slot].path.clone() }, false);
        }
        let neighbour = self.tiles.nearest(key, step.saturating_mul(BORROW_STEPS), |tile| matches!(tile, Tile::Ready(_)))?;
        match self.tiles.touch(neighbour) {
            Some(Tile::Ready(handle)) => Some(handle.id()),
            _ => None,
        }
    }

    /// The picture a library tile of `source` shows, a little way into it.
    pub fn poster(&mut self, source: &str, length: Option<Time>) -> Option<(TextureId, f32)> {
        let slot = self.slot(source);
        let SourceView::Known(facts) = self.view(slot) else { return None };
        if !facts.has_video {
            return None;
        }
        let wanted = length.map_or(TICKS_PER_SECOND, |length| (length.ticks() / 10).min(TICKS_PER_SECOND));
        let bucket = bucket_time(wanted, TICKS_PER_SECOND, length);
        self.tile(slot, bucket, TICKS_PER_SECOND).map(|texture| (texture, facts.aspect))
    }

    /// The levels of `slot` so far, asked for when none exist.
    pub fn peaks(&mut self, slot: usize) -> Option<&PeakData> {
        let identity = self.slots[slot].identity?;
        let SourceView::Known(facts) = self.slots[slot].view else { return None };
        if !facts.has_audio {
            return None;
        }
        if !self.peaks.contains_key(&identity) {
            let run = self.peak_runs.get(&identity).copied().unwrap_or(0);
            self.want(Job { key: JobKey::Peaks(identity, run), path: self.slots[slot].path.clone() }, false);
            return None;
        }
        let clock = self.clock;
        let entry = self.peaks.get_mut(&identity)?;
        entry.used = clock;
        Some(&entry.data)
    }

    fn want(&mut self, job: Job, front: bool) {
        if !self.wanted_keys.insert(job.key.clone()) {
            return;
        }
        match job.key {
            JobKey::Peaks(..) => self.wanted_audio.push(job),
            _ if front => self.wanted_frames.insert(0, job),
            _ => self.wanted_frames.push(job),
        }
    }

    fn apply(&mut self, done: Done) {
        match done {
            Done::Stamp { slot, stamp } => {
                let index = slot as usize;
                self.slots[index].checked = Some(Instant::now());
                // No stamp means the file could not be read just now, not that it changed. When it
                // reads again unchanged, what failed meanwhile is tried once more.
                if stamp.is_none() {
                    self.slots[index].stamp_lost = true;
                } else if self.slots[index].stamp != stamp {
                    self.reset_slot(index);
                } else if std::mem::take(&mut self.slots[index].stamp_lost) {
                    self.clear_failures(index);
                }
            }
            Done::Inspected { slot, stamp, result } => {
                let entry = &mut self.slots[slot as usize];
                entry.stamp = stamp;
                entry.checked = Some(Instant::now());
                match result {
                    Ok(facts) => {
                        let identity = self.next_identity;
                        self.next_identity += 1;
                        self.live_identities.insert(identity);
                        entry.identity = Some(identity);
                        entry.view = SourceView::Known(facts);
                    }
                    Err(_) => entry.view = SourceView::Failed,
                }
            }
            Done::Frame { identity, bucket, result } => {
                if !self.live_identities.contains(&identity) {
                    return;
                }
                let key = (identity, bucket);
                match result {
                    Ok(image) => {
                        self.tiles.insert(key, Tile::Uploading(color_image(&image)));
                        self.uploads.push_back(key);
                    }
                    Err(ThumbnailError::Failed(_)) => self.tiles.insert(key, Tile::Failed),
                    Err(ThumbnailError::TimedOut) => {
                        let attempts = match self.tiles.peek_mut(key) {
                            Some(Tile::Retry { attempts, .. }) => *attempts + 1,
                            _ => 1,
                        };
                        let tile = if attempts > MAX_TIMEOUT_RETRIES { Tile::Failed } else { Tile::Retry { attempts, not_before: Instant::now() + TIMEOUT_RETRY_DELAY * (1 << (attempts - 1)) } };
                        self.tiles.insert(key, tile);
                    }
                }
            }
            Done::Peaks { identity, run, peaks, finished } => {
                if !self.live_identities.contains(&identity) || self.peak_runs.get(&identity).copied().unwrap_or(0) != run {
                    return;
                }
                let clock = self.clock;
                let entry = self.peaks.entry(identity).or_insert(PeakEntry { data: PeakData::default(), finished: false, used: clock });
                entry.data.append(&peaks);
                entry.finished |= finished;
                self.enforce_peak_limit();
            }
        }
    }

    /// Forgets what could not be read of a source, keeping every picture and level that was.
    fn clear_failures(&mut self, index: usize) {
        let Some(identity) = self.slots[index].identity else { return };
        self.tiles.remove_where(identity, |tile| matches!(tile, Tile::Failed));
        if self.peaks.get(&identity).is_some_and(|entry| entry.finished && entry.data.is_empty()) {
            self.peaks.remove(&identity);
        }
    }

    /// Forgets a source whose file changed: its identity ends, so nothing old can come back.
    fn reset_slot(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        slot.stamp = None;
        slot.view = SourceView::Pending;
        slot.checked = None;
        slot.stamp_lost = false;
        if let Some(identity) = slot.identity.take() {
            self.live_identities.remove(&identity);
            self.tiles.remove_source(identity);
            self.peaks.remove(&identity);
            self.cancel_peak_run(identity);
            self.peak_runs.remove(&identity);
        }
    }

    fn enforce_peak_limit(&mut self) {
        let mut total: usize = self.peaks.values().map(|entry| entry.data.bytes()).sum();
        while total > self.peak_bytes_limit {
            // Runs after the clock moved on, so a source drawn last frame has `used == clock - 1`.
            let oldest = self.peaks.iter().filter(|(_, entry)| entry.finished && entry.used + 1 < self.clock).min_by_key(|(_, entry)| entry.used).map(|(identity, _)| *identity);
            let Some(oldest) = oldest else { break };
            if let Some(entry) = self.peaks.remove(&oldest) {
                total -= entry.data.bytes();
            }
        }
    }

    /// Turns a finished picture into a texture; false when it was evicted meanwhile.
    fn upload(&mut self, ctx: &egui::Context, key: TileKey) -> bool {
        let Some(entry) = self.tiles.peek_mut(key) else { return false };
        let image = match entry {
            Tile::Uploading(image) => image.clone(),
            _ => return false,
        };
        *entry = Tile::Ready(ctx.load_texture(format!("clip-thumb-{}-{}", key.0, key.1), image, TextureOptions::LINEAR));
        true
    }
}

fn color_image(image: &ThumbnailImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.rgba)
}

/// Windows compares paths without case and with either slash.
fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('/', "\\").to_lowercase()
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests;
