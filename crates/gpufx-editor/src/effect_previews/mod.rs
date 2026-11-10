//! Rendered previews of effect packages, kept as files so an effect is never rendered again in a
//! later run: how a preview is named ([`key`]), how it is stored and found again ([`disk`]), the
//! helper process that renders ([`child`]), and [`PreviewManager`], which the UI talks to.
//!
//! Each frame the UI says which tiles it wants, the hovered one first. What is on disk is read by a
//! background thread and costs no render; the rest goes to the render thread, one job at a time.
//! The UI thread never reads a file or decodes a picture; it only uploads the finished pixels.

pub mod child;
pub mod disk;
pub mod key;
pub mod play;
pub mod worker;

pub use disk::{PreviewImage, PreviewStore, SweepReport, DISK_LIMIT};
pub use key::{package_digest, package_prefix, preview_key, PreviewSpec, PREVIEW_RENDER_VERSION};
pub use worker::{PreviewItem, Renderer, Resolved, LOOP_COLUMNS};

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use d3d12_backend::{PreviewJobKind, ProjectPaths};
use egui::{ColorImage, TextureHandle, TextureOptions};

use child::{PreviewSession, ProcessLauncher, RenderOutcome, SessionConfig};
use worker::{run_loader, run_renderer, Line, LoaderContext, Loaded, PackageResolver, RenderJob, Rendered, RendererFactory};

use crate::export::sibling_render_exe;
use crate::visuals::worker::Waker;

/// How many still textures are kept.
const STILL_TEXTURES: usize = 300;
/// How many loop strips are kept.
const LOOP_TEXTURES: usize = 24;
/// Textures uploaded per UI frame, so a burst of finished pictures costs little.
const UPLOADS_PER_FRAME: usize = 6;
/// The most previews asked for at once, well under [`STILL_TEXTURES`] so that what is on screen is
/// never evicted by what is on screen.
const MAX_WANTED: usize = 256;
/// How long a preview that failed, in a way that may pass, is left alone.
const RETRY_DELAY: Duration = Duration::from_secs(30);
/// How long the UI thread waits for the threads when the app closes. What is still running then is
/// left behind; the job object ends its helper with the process.
const DROP_WAIT: Duration = Duration::from_millis(200);
/// The package the control job renders.
const CONTROL_PACKAGE: &str = "packages/gpufx/ffmpeg/invert";

/// What a tile of the picker shows.
#[derive(Clone)]
pub enum TileState {
    /// It has to be rendered first.
    Pending,
    /// It is being looked for on disk, or waits for its texture.
    Loading,
    Still(TextureHandle),
    /// It could not be rendered, with the reason for a tooltip.
    Failed(String),
    /// Previews cannot be rendered at all in this run, with the reason.
    Unavailable(String),
}

/// What a hovered tile can animate.
#[derive(Clone)]
pub enum AnimationState {
    /// Not there yet; asked for while the tile is hovered.
    Rendering,
    /// The effect does not change over time, so there are no frames to play.
    Static,
    Frames(LoopFrames),
    /// There will be no animation.
    Unavailable,
}

/// The frames of a loop in one texture, in rows of [`LOOP_COLUMNS`].
#[derive(Clone)]
pub struct LoopFrames {
    pub texture: TextureHandle,
    pub frames: u32,
}

impl LoopFrames {
    /// Where frame `index`, wrapped around, is in the texture.
    ///
    /// The area is inset by half a texel on every side, so that linear filtering at its edge does
    /// not take in a texel of the neighbouring frame.
    pub fn uv(&self, index: u32) -> egui::Rect {
        frame_uv(index, self.frames)
    }
}

fn frame_uv(index: u32, frames: u32) -> egui::Rect {
    let frames = frames.max(1);
    let index = index % frames;
    let (columns, rows) = (frames.min(LOOP_COLUMNS) as f32, frames.div_ceil(LOOP_COLUMNS) as f32);
    let (column, row) = ((index % LOOP_COLUMNS) as f32, (index / LOOP_COLUMNS) as f32);
    let spec = PreviewSpec::STANDARD;
    // Half a texel of the whole texture, in uv.
    let (inset_u, inset_v) = (0.5 / (spec.width as f32 * columns), 0.5 / (spec.height as f32 * rows));
    egui::Rect::from_min_max(
        egui::pos2(column / columns + inset_u, row / rows + inset_v),
        egui::pos2((column + 1.0) / columns - inset_u, (row + 1.0) / rows - inset_v),
    )
}

/// What is known about a preview that has no texture. A preview with no stage and no texture has
/// to be looked for on disk.
enum Stage {
    /// Decoded, waits for its share of the uploads.
    Uploading,
    /// Not on disk.
    Missing(Arc<Resolved>),
    /// Failed in a way that may pass: not asked for again before `until`, and then only by a later
    /// frame that still wants it.
    Backoff { until: Instant, reason: String },
    Failed(String),
}

/// Textures by package, least recently used out first. A value is dropped on eviction, which frees
/// the texture.
struct TileCache<T> {
    entries: HashMap<String, (u64, T)>,
    capacity: usize,
    clock: u64,
}

impl<T> TileCache<T> {
    fn new(capacity: usize) -> Self {
        Self { entries: HashMap::new(), capacity: capacity.max(1), clock: 0 }
    }

    fn contains(&self, package: &str) -> bool {
        self.entries.contains_key(package)
    }

    /// Marks `package` as just used and returns its value.
    fn touch(&mut self, package: &str) -> Option<&T> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(package).map(|(used, value)| {
            *used = clock;
            &*value
        })
    }

    fn insert(&mut self, package: String, value: T) {
        self.clock += 1;
        if !self.entries.contains_key(&package) && self.entries.len() >= self.capacity {
            if let Some(oldest) = self.entries.iter().min_by_key(|(_, (used, _))| *used).map(|(package, _)| package.clone()) {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(package, (self.clock, value));
    }
}

/// A loop in memory.
enum LoopTexture {
    Static,
    Strip(LoopFrames),
}

/// How the manager reaches the disk and the renderer.
pub struct PreviewBackend {
    store_root: PathBuf,
    resolve: PackageResolver,
    renderer: RendererFactory,
    version: String,
}

impl PreviewBackend {
    /// The real one: the folder in the user's local data and `gpufx-render.exe` beside the editor.
    /// It only looks at those once its threads run, and a helper starts at the first job.
    pub fn for_editor(package_roots: Vec<PathBuf>) -> Result<Self, String> {
        let store_root = PreviewStore::default_root().ok_or("LOCALAPPDATA is not set, so previews have nowhere to be kept")?;
        let paths = Arc::new(ProjectPaths::roots_only(Path::new("effect-previews.json"), package_roots));
        let renderer_paths = paths.clone();
        let renderer_root = store_root.clone();
        let renderer: RendererFactory = Box::new(move || {
            let exe = sibling_render_exe();
            if !exe.is_file() {
                return Err(format!("the preview helper {} does not exist", exe.display()));
            }
            let mut config = SessionConfig::new();
            config.control_package = renderer_paths.package_path(CONTROL_PACKAGE);
            Ok(Box::new(PreviewSession::new(Box::new(ProcessLauncher::new(exe)), PreviewStore::new(renderer_root), config)) as Box<dyn Renderer>)
        });
        Ok(Self { store_root, resolve: Arc::new(move |package| paths.package_path(package)), renderer, version: SessionConfig::new().version })
    }
}

struct Threads {
    loader: Arc<Line<PreviewItem, Loaded>>,
    renderer: Arc<Line<RenderJob, Rendered>>,
    handles: Vec<JoinHandle<()>>,
}

/// The previews of the session, on the UI thread. It starts nothing until [`Self::start`], so tests
/// and paths without a window run no thread and no process.
pub struct PreviewManager {
    threads: Option<Threads>,
    /// Why nothing new can be rendered.
    unavailable: Option<String>,
    paused: bool,
    retry_delay: Duration,
    stages: HashMap<PreviewItem, Stage>,
    stills: TileCache<TextureHandle>,
    loops: TileCache<LoopTexture>,
    /// Decoded pictures, with the number of frames of a loop.
    uploads: VecDeque<(PreviewItem, ColorImage, u32)>,
    /// What this frame wants, best first.
    wanted: Vec<PreviewItem>,
    /// What the last finished frame wanted.
    live: HashSet<PreviewItem>,
    published_loads: Vec<PreviewItem>,
    published_jobs: Vec<RenderJob>,
    /// The built-in picture, for showing the effect against it.
    original: Option<TextureHandle>,
    pending_original: Option<ColorImage>,
}

impl Default for PreviewManager {
    fn default() -> Self {
        Self {
            threads: None,
            unavailable: None,
            paused: false,
            retry_delay: RETRY_DELAY,
            stages: HashMap::new(),
            stills: TileCache::new(STILL_TEXTURES),
            loops: TileCache::new(LOOP_TEXTURES),
            uploads: VecDeque::new(),
            wanted: Vec::new(),
            live: HashSet::new(),
            published_loads: Vec::new(),
            published_jobs: Vec::new(),
            original: None,
            pending_original: None,
        }
    }
}

impl PreviewManager {
    /// Starts the threads. `wake` is called from them when results wait to be collected.
    pub fn start(&mut self, backend: PreviewBackend, wake: Waker) {
        let PreviewBackend { store_root, resolve, renderer, version } = backend;
        let adapter: Arc<Mutex<Option<String>>> = Arc::default();
        let loader = Arc::new(Line::new());
        let render_line = Arc::new(Line::new());
        let render_store = PreviewStore::new(store_root.clone());
        let context = LoaderContext { store: PreviewStore::new(store_root), resolve, adapter: adapter.clone(), version };
        let (loader_line, loader_wake) = (loader.clone(), wake.clone());
        let loader_thread = thread::Builder::new().name("effect-preview-loader".to_owned()).spawn(move || run_loader(loader_line, context, loader_wake));
        let (renderer_line, renderer_wake) = (render_line.clone(), wake);
        let render_thread = thread::Builder::new().name("effect-preview-render".to_owned()).spawn(move || run_renderer(renderer_line, renderer, render_store, adapter, renderer_wake));
        match (loader_thread, render_thread) {
            (Ok(loader_thread), Ok(render_thread)) => self.threads = Some(Threads { loader, renderer: render_line, handles: vec![loader_thread, render_thread] }),
            (first, second) => {
                loader.shut_down();
                render_line.shut_down();
                drop((first, second));
                self.unavailable = Some("the threads that make previews could not start".to_owned());
            }
        }
    }

    /// Starts the real backend, or records why there is none.
    pub fn start_for_editor(&mut self, package_roots: Vec<PathBuf>, wake: Waker) {
        match PreviewBackend::for_editor(package_roots) {
            Ok(backend) => self.start(backend, wake),
            Err(reason) => self.unavailable = Some(reason),
        }
    }

    /// Sends no new render job while playback or an export needs the machine; the current one
    /// finishes. Reading what is on disk goes on.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Takes finished work and uploads a few textures. Call once before anything is drawn.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.wanted.clear();
        if self.threads.is_some() && self.pump(ctx) {
            ctx.request_repaint();
        }
    }

    /// True when decoded pictures still wait for their upload.
    fn pump(&mut self, ctx: &egui::Context) -> bool {
        let Some(threads) = &self.threads else { return false };
        let loaded = threads.loader.take_done();
        let rendered = threads.renderer.take_done();
        for (item, result) in loaded {
            match item {
                Some(item) => self.apply_loaded(item, result),
                None => {
                    if let Loaded::Original(image) = result {
                        self.pending_original = Some(image);
                    }
                }
            }
        }
        for (job, result) in rendered {
            match result {
                Rendered::Unavailable(reason) => self.unavailable = Some(reason),
                Rendered::Finished(outcome) => {
                    if let Some(job) = job {
                        self.apply_rendered(job.item, outcome);
                    }
                }
            }
        }
        if let Some(image) = self.pending_original.take() {
            self.original = Some(ctx.load_texture("effect-preview-original", image, TextureOptions::LINEAR));
        }
        let mut uploaded = 0;
        while uploaded < UPLOADS_PER_FRAME {
            let Some((item, image, frames)) = self.uploads.pop_front() else { break };
            self.stages.remove(&item);
            // Scrolled away since it was asked for: reading it again is cheap, a texture is not.
            if self.live.contains(&item) {
                self.upload(ctx, item, image, frames);
                uploaded += 1;
            }
        }
        !self.uploads.is_empty()
    }

    fn apply_loaded(&mut self, item: PreviewItem, loaded: Loaded) {
        match loaded {
            Loaded::Image(image) => {
                self.stages.insert(item.clone(), Stage::Uploading);
                self.uploads.push_back((item, image, 1));
            }
            Loaded::Loop { atlas, frames } => {
                self.stages.insert(item.clone(), Stage::Uploading);
                self.uploads.push_back((item, atlas, frames));
            }
            Loaded::Missing(resolved) => {
                self.stages.insert(item, Stage::Missing(resolved));
            }
            Loaded::Failed(reason) => {
                self.stages.insert(item, Stage::Failed(reason));
            }
            Loaded::Retry(reason) => self.back_off(item, reason),
            Loaded::Original(_) => {}
        }
    }

    fn apply_rendered(&mut self, item: PreviewItem, outcome: RenderOutcome) {
        match outcome {
            // The file is what counts: the loader reads it like any other.
            RenderOutcome::Published { .. } => {
                self.stages.remove(&item);
            }
            RenderOutcome::NotPreviewable(reason) | RenderOutcome::AlreadyFailed(reason) | RenderOutcome::GivenUp(reason) | RenderOutcome::Failed { reason, recorded: true } => {
                self.stages.insert(item, Stage::Failed(reason));
            }
            RenderOutcome::Failed { reason, recorded: false } => self.back_off(item, reason),
            RenderOutcome::Cancelled | RenderOutcome::Stopped(_) | RenderOutcome::CoolingDown(_) => {}
        }
    }

    fn back_off(&mut self, item: PreviewItem, reason: String) {
        self.stages.insert(item, Stage::Backoff { until: Instant::now() + self.retry_delay, reason });
    }

    fn upload(&mut self, ctx: &egui::Context, item: PreviewItem, image: ColorImage, frames: u32) {
        let name = format!("effect-preview-{}", item.package);
        match item.kind {
            PreviewJobKind::Still => {
                let texture = ctx.load_texture(name, image, TextureOptions::LINEAR);
                self.stills.insert(item.package, texture);
            }
            PreviewJobKind::Loop => {
                let value = if frames <= 1 { LoopTexture::Static } else { LoopTexture::Strip(LoopFrames { texture: ctx.load_texture(name, image, TextureOptions::LINEAR), frames }) };
                self.loops.insert(item.package, value);
            }
        }
    }

    /// Says what the picker shows this frame, which replaces what the last frame wanted. A frame
    /// that does not call this wants nothing. The hovered tile comes first, with its loop.
    ///
    /// Pass only the tiles that are on screen. At most [`MAX_WANTED`] previews are asked for: tiles
    /// past that, in the order given, are not asked for at all, so they show [`TileState::Loading`]
    /// until a frame that fits them in, such as after scrolling, lists them.
    pub fn set_wanted<'a>(&mut self, hovered: Option<&str>, visible: impl IntoIterator<Item = &'a str>) {
        self.wanted = wanted_items(hovered, visible);
    }

    /// Hands what this frame wants to the threads. Call once after everything is drawn.
    pub fn end_frame(&mut self) {
        let wanted = std::mem::take(&mut self.wanted);
        self.live = wanted.iter().cloned().collect();
        let Some(threads) = &self.threads else { return };
        let now = Instant::now();
        let mut loads = Vec::new();
        let mut jobs = Vec::new();
        for item in wanted {
            let cached = match item.kind {
                PreviewJobKind::Still => self.stills.contains(&item.package),
                PreviewJobKind::Loop => self.loops.contains(&item.package),
            };
            if cached {
                continue;
            }
            // A package whose still failed has no loop worth rendering either.
            if item.kind == PreviewJobKind::Loop && matches!(self.stages.get(&still_of(&item)), Some(Stage::Failed(_) | Stage::Backoff { .. })) {
                continue;
            }
            match self.stages.get(&item) {
                None => loads.push(item),
                Some(Stage::Backoff { until, .. }) if now >= *until => {
                    self.stages.remove(&item);
                    loads.push(item);
                }
                Some(Stage::Missing(resolved)) if !self.paused && self.unavailable.is_none() && self.may_render(&item) => {
                    let resolved = resolved.clone();
                    jobs.push(RenderJob { item, resolved });
                }
                Some(_) => {}
            }
        }
        if loads != self.published_loads {
            threads.loader.replace(loads.clone());
            self.published_loads = loads;
        }
        if jobs != self.published_jobs {
            threads.renderer.replace(jobs.clone());
            self.published_jobs = jobs;
        }
    }

    /// A loop is rendered only once the still of its package is known to be good, which is when its
    /// texture is there or its file was read. So the still always finishes first, and a package whose
    /// still fails never has a loop rendered after it.
    fn may_render(&self, item: &PreviewItem) -> bool {
        item.kind == PreviewJobKind::Still || self.stills.contains(&item.package) || matches!(self.stages.get(&still_of(item)), Some(Stage::Uploading))
    }

    /// What the tile of `package` shows. Asking does not put it in the queue: [`Self::set_wanted`] does.
    pub fn tile(&mut self, package: &str) -> TileState {
        if let Some(texture) = self.stills.touch(package) {
            return TileState::Still(texture.clone());
        }
        if self.threads.is_none() {
            return TileState::Unavailable(self.unavailable_reason());
        }
        match self.stages.get(&PreviewItem { package: package.to_owned(), kind: PreviewJobKind::Still }) {
            None | Some(Stage::Uploading) => TileState::Loading,
            Some(Stage::Missing(_)) => match &self.unavailable {
                Some(reason) => TileState::Unavailable(reason.clone()),
                None => TileState::Pending,
            },
            Some(Stage::Backoff { reason, .. } | Stage::Failed(reason)) => TileState::Failed(reason.clone()),
        }
    }

    /// What the hovered tile of `package` can play. Only a hovered tile asks for a loop.
    pub fn animation(&mut self, package: &str) -> AnimationState {
        match self.loops.touch(package) {
            Some(LoopTexture::Static) => return AnimationState::Static,
            Some(LoopTexture::Strip(frames)) => return AnimationState::Frames(frames.clone()),
            None => {}
        }
        if self.threads.is_none() {
            return AnimationState::Unavailable;
        }
        match self.stages.get(&PreviewItem { package: package.to_owned(), kind: PreviewJobKind::Loop }) {
            None | Some(Stage::Uploading) => AnimationState::Rendering,
            Some(Stage::Missing(_)) if self.unavailable.is_none() => AnimationState::Rendering,
            Some(_) => AnimationState::Unavailable,
        }
    }

    /// The built-in picture the previews are rendered on, once it has been decoded.
    pub fn original(&self) -> Option<TextureHandle> {
        self.original.clone()
    }

    fn unavailable_reason(&self) -> String {
        self.unavailable.clone().unwrap_or_else(|| "effect previews are not running".to_owned())
    }
}

impl Drop for PreviewManager {
    /// Runs on the UI thread as the app closes, so the wait is bounded and short.
    fn drop(&mut self) {
        let Some(mut threads) = self.threads.take() else { return };
        threads.loader.shut_down();
        threads.renderer.shut_down();
        let started = Instant::now();
        for handle in threads.handles.drain(..) {
            while !handle.is_finished() {
                if started.elapsed() >= DROP_WAIT {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            let _ = handle.join();
        }
    }
}

fn still_of(item: &PreviewItem) -> PreviewItem {
    PreviewItem { package: item.package.clone(), kind: PreviewJobKind::Still }
}

/// The hovered tile's still and loop, then the still of each visible tile, once each.
fn wanted_items<'a>(hovered: Option<&str>, visible: impl IntoIterator<Item = &'a str>) -> Vec<PreviewItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    if let Some(package) = hovered {
        seen.insert(package.to_owned());
        items.push(PreviewItem { package: package.to_owned(), kind: PreviewJobKind::Still });
        items.push(PreviewItem { package: package.to_owned(), kind: PreviewJobKind::Loop });
    }
    for package in visible {
        if items.len() >= MAX_WANTED {
            break;
        }
        if seen.insert(package.to_owned()) {
            items.push(PreviewItem { package: package.to_owned(), kind: PreviewJobKind::Still });
        }
    }
    items
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod manager_tests;
