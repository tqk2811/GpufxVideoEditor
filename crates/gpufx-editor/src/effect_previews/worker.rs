//! The two threads behind [`PreviewManager`](super::PreviewManager): one reads and decodes what is
//! on disk, the other owns the [`PreviewSession`] and renders what is not. Each has a [`Line`], a
//! list that the UI thread replaces, and finished work that the UI thread collects.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use d3d12_backend::{preview_image_files, PreviewJobKind};
use egui::ColorImage;

use super::child::{PreviewRequest, PreviewSession, RenderOutcome};
use super::{package_digest, package_prefix, preview_key, PreviewImage, PreviewSpec, PreviewStore, DISK_LIMIT};
use crate::visuals::worker::Waker;

/// The shortest sleep of a cooldown, so that a cooldown that is about to end cannot make a spin.
const MIN_COOLDOWN_SLEEP: Duration = Duration::from_millis(10);
/// How many frames of a loop lie side by side in its texture.
pub const LOOP_COLUMNS: u32 = 6;

/// One preview of one package.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewItem {
    pub package: String,
    pub kind: PreviewJobKind,
}

/// Where a package is and what its previews are called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub dir: PathBuf,
    pub prefix: String,
    pub key: String,
}

/// What renders previews; [`PreviewSession`] outside the tests. It lives and ends on the render
/// thread.
pub trait Renderer: Send {
    /// Blocks until the job is over.
    fn render(&mut self, request: &PreviewRequest) -> RenderOutcome;
    /// The adapter of the helper, once one was started.
    fn device(&self) -> Option<String>;
    /// How long the cooldown still lasts, during one.
    fn cooldown_left(&self) -> Option<Duration>;
    /// Setting it ends a [`render`](Self::render) that is waiting.
    fn stop_flag(&self) -> Arc<AtomicBool>;
    /// Ends the helper. This can take seconds.
    fn shutdown(&mut self);
}

impl Renderer for PreviewSession {
    fn render(&mut self, request: &PreviewRequest) -> RenderOutcome {
        PreviewSession::render(self, request)
    }

    fn device(&self) -> Option<String> {
        PreviewSession::device(self).map(str::to_owned)
    }

    fn cooldown_left(&self) -> Option<Duration> {
        PreviewSession::cooldown_left(self)
    }

    fn stop_flag(&self) -> Arc<AtomicBool> {
        PreviewSession::stop_flag(self)
    }

    fn shutdown(&mut self) {
        PreviewSession::shutdown(self);
    }
}

struct Mailbox<J, R> {
    /// The last list the UI gave, whatever was taken from it since.
    wanted: Vec<J>,
    waiting: VecDeque<J>,
    running: Option<J>,
    done: Vec<(Option<J>, R)>,
    shutdown: bool,
    /// Set at shutdown, to end a job that is waiting.
    stop: Option<Arc<AtomicBool>>,
}

/// A list of jobs from the UI thread to one worker, and their results back.
pub(super) struct Line<J, R> {
    mailbox: Mutex<Mailbox<J, R>>,
    changed: Condvar,
}

impl<J: Clone + PartialEq, R> Line<J, R> {
    pub(super) fn new() -> Self {
        Self { mailbox: Mutex::new(Mailbox { wanted: Vec::new(), waiting: VecDeque::new(), running: None, done: Vec::new(), shutdown: false, stop: None }), changed: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, Mailbox<J, R>> {
        self.mailbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replaces the jobs that wait. A job that runs, or whose result was not collected yet, is not
    /// waiting again: under this one lock the UI cannot ask twice for something already done.
    pub(super) fn replace(&self, list: Vec<J>) {
        let mut mailbox = self.lock();
        let waiting: VecDeque<J> = list.iter().filter(|job| mailbox.running.as_ref() != Some(*job) && !mailbox.done.iter().any(|(done, _)| done.as_ref() == Some(*job))).cloned().collect();
        mailbox.waiting = waiting;
        mailbox.wanted = list;
        self.changed.notify_all();
    }

    pub(super) fn take_done(&self) -> Vec<(Option<J>, R)> {
        std::mem::take(&mut self.lock().done)
    }

    /// Ends the worker: it sees this before its next job, and a job that waits is stopped.
    pub(super) fn shut_down(&self) {
        let mut mailbox = self.lock();
        mailbox.shutdown = true;
        if let Some(stop) = &mailbox.stop {
            stop.store(true, Ordering::SeqCst);
        }
        self.changed.notify_all();
    }

    /// From here on a shutdown sets `stop`, or sets it now if the shutdown came first.
    fn attach_stop(&self, stop: Arc<AtomicBool>) {
        let mut mailbox = self.lock();
        if mailbox.shutdown {
            stop.store(true, Ordering::SeqCst);
        }
        mailbox.stop = Some(stop);
    }

    /// Waits for a job. `None` means the worker is to end.
    pub(super) fn next(&self) -> Option<J> {
        let mut mailbox = self.lock();
        loop {
            if mailbox.shutdown {
                return None;
            }
            if let Some(job) = mailbox.waiting.pop_front() {
                mailbox.running = Some(job.clone());
                return Some(job);
            }
            mailbox = self.changed.wait(mailbox).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Reports the result of the job that ran.
    pub(super) fn finish(&self, result: R) {
        let mut mailbox = self.lock();
        let job = mailbox.running.take();
        mailbox.done.push((job, result));
    }

    /// Reports something that belongs to no job.
    pub(super) fn note(&self, result: R) {
        self.lock().done.push((None, result));
    }

    /// The job that ran was not done; it waits again if it is still wanted.
    pub(super) fn put_back(&self) {
        let mut mailbox = self.lock();
        if let Some(job) = mailbox.running.take() {
            if mailbox.wanted.contains(&job) && !mailbox.waiting.contains(&job) {
                mailbox.waiting.push_front(job);
            }
        }
    }

    /// Sleeps for `wait`, or until the shutdown. False means the worker is to end.
    pub(super) fn sleep(&self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let mut mailbox = self.lock();
        loop {
            if mailbox.shutdown {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            mailbox = self.changed.wait_timeout(mailbox, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }
}

/// What the loader found.
pub(super) enum Loaded {
    Image(ColorImage),
    /// A loop, laid out by [`strip_to_atlas`].
    Loop { atlas: ColorImage, frames: u32 },
    /// Not on disk, so it has to be rendered.
    Missing(Arc<Resolved>),
    /// It will not be rendered: a package that is not there, or a failure on record.
    Failed(String),
    /// Reading failed, which may pass.
    Retry(String),
    /// The built-in picture the previews are rendered on.
    Original(ColorImage),
}

pub(super) type PackageResolver = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// What the loader works with.
pub(super) struct LoaderContext {
    pub store: PreviewStore,
    pub resolve: PackageResolver,
    /// The adapter of the render thread, once it is known.
    pub adapter: Arc<Mutex<Option<String>>>,
    pub version: String,
}

/// Premultiplies a picture for egui. It runs on the loader thread, so the UI thread only uploads.
pub(super) fn color_image(image: &PreviewImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.rgba)
}

/// Decodes the first built-in picture, RGB or RGBA, to RGBA8.
fn decode_original() -> Option<PreviewImage> {
    let [file, _] = preview_image_files();
    let mut reader = png::Decoder::new(std::io::Cursor::new(file)).read_info().ok()?;
    let mut pixels = vec![0u8; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    let pixels = &pixels[..frame.buffer_size()];
    let rgba = match frame.color_type {
        png::ColorType::Rgba if frame.bit_depth == png::BitDepth::Eight => pixels.to_vec(),
        png::ColorType::Rgb if frame.bit_depth == png::BitDepth::Eight => pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    Some(PreviewImage { width: frame.width, height: frame.height, rgba })
}

/// Moves the frames of a strip, which is taller than the 2048 a texture is sure to be allowed, into
/// rows of [`LOOP_COLUMNS`]. Returns the picture and the number of frames.
pub(super) fn strip_to_atlas(strip: &PreviewImage, spec: PreviewSpec) -> (PreviewImage, u32) {
    let frames = strip.height / spec.height;
    let (columns, rows) = (frames.min(LOOP_COLUMNS), frames.div_ceil(LOOP_COLUMNS));
    let row_bytes = spec.width as usize * 4;
    let (width, height) = (spec.width * columns, spec.height * rows);
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for frame in 0..frames {
        let (column, row) = (frame % LOOP_COLUMNS, frame / LOOP_COLUMNS);
        for y in 0..spec.height {
            let from = (frame * spec.height + y) as usize * row_bytes;
            let to = (row * spec.height + y) as usize * width as usize * 4 + column as usize * row_bytes;
            rgba[to..to + row_bytes].copy_from_slice(&strip.rgba[from..from + row_bytes]);
        }
    }
    (PreviewImage { width, height, rgba }, frames)
}

/// Finds a package and names its previews. Reads the whole package.
pub(super) fn resolve_package(package: &str, resolve: &PackageResolver, spec: PreviewSpec) -> Result<Resolved, Loaded> {
    let dir = resolve(package).ok_or_else(|| Loaded::Failed(format!("the package {package} was not found")))?;
    let digest = package_digest(&dir).map_err(|e| Loaded::Retry(format!("reading the package {package} failed: {e}")))?;
    let images = preview_image_files();
    Ok(Resolved { dir, prefix: package_prefix(package), key: preview_key(&digest, &images, spec) })
}

/// `known` keeps what was worked out about a package for the whole run, so a package edited while
/// the editor runs keeps its old preview until the editor restarts. That is accepted.
fn load(item: &PreviewItem, context: &LoaderContext, known: &mut HashMap<String, Arc<Resolved>>) -> Loaded {
    let spec = PreviewSpec::STANDARD;
    let resolved = match known.get(&item.package) {
        Some(resolved) => resolved.clone(),
        None => match resolve_package(&item.package, &context.resolve, spec) {
            Ok(resolved) => {
                let resolved = Arc::new(resolved);
                known.insert(item.package.clone(), resolved.clone());
                resolved
            }
            Err(loaded) => return loaded,
        },
    };
    let read = match item.kind {
        PreviewJobKind::Still => context.store.read_still(&resolved.prefix, &resolved.key, spec),
        PreviewJobKind::Loop => context.store.read_loop(&resolved.prefix, &resolved.key, spec),
    };
    match read {
        Ok(Some(image)) if item.kind == PreviewJobKind::Loop => {
            let (atlas, frames) = strip_to_atlas(&image, spec);
            Loaded::Loop { atlas: color_image(&atlas), frames }
        }
        Ok(Some(image)) => Loaded::Image(color_image(&image)),
        Err(e) => Loaded::Retry(format!("reading the preview failed: {e}")),
        Ok(None) => {
            let adapter = context.adapter.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            match adapter.and_then(|adapter| context.store.failure_on(&resolved.prefix, &resolved.key, &adapter, &context.version)) {
                Some(reason) => Loaded::Failed(reason),
                None => Loaded::Missing(resolved),
            }
        }
    }
}

/// The body of the loader thread. It also does the one sweep of a run, before anything is read.
pub(super) fn run_loader(line: Arc<Line<PreviewItem, Loaded>>, context: LoaderContext, wake: Waker) {
    if let Some(image) = decode_original() {
        line.note(Loaded::Original(color_image(&image)));
        wake();
    }
    let _ = catch_unwind(AssertUnwindSafe(|| context.store.sweep(DISK_LIMIT, SystemTime::now())));
    let mut known = HashMap::new();
    while let Some(item) = line.next() {
        let loaded = catch_unwind(AssertUnwindSafe(|| load(&item, &context, &mut known))).unwrap_or_else(|_| Loaded::Retry("reading the preview hit an internal error".to_owned()));
        line.finish(loaded);
        wake();
    }
}

/// One preview to render.
#[derive(Debug, Clone)]
pub(super) struct RenderJob {
    pub item: PreviewItem,
    pub resolved: Arc<Resolved>,
}

impl PartialEq for RenderJob {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
    }
}

pub(super) enum Rendered {
    Finished(RenderOutcome),
    /// Nothing can be rendered in this run, with the reason.
    Unavailable(String),
}

pub(super) type RendererFactory = Box<dyn FnOnce() -> Result<Box<dyn Renderer>, String> + Send>;

/// Whether the file a job would make is already there and valid, and if so whether a loop in it is
/// static.
fn already_published(store: &PreviewStore, request: &PreviewRequest) -> Option<bool> {
    match request.kind {
        PreviewJobKind::Still => store.read_still(&request.package_prefix, &request.key, request.spec).ok().flatten().map(|_| false),
        PreviewJobKind::Loop => store.read_loop(&request.package_prefix, &request.key, request.spec).ok().flatten().map(|image| image.height == request.spec.height),
    }
}

/// The body of the render thread. The renderer is made and ended here, never on the UI thread.
pub(super) fn run_renderer(line: Arc<Line<RenderJob, Rendered>>, factory: RendererFactory, store: PreviewStore, adapter: Arc<Mutex<Option<String>>>, wake: Waker) {
    let mut renderer = match factory() {
        Ok(renderer) => renderer,
        Err(reason) => {
            line.note(Rendered::Unavailable(reason));
            wake();
            return;
        }
    };
    line.attach_stop(renderer.stop_flag());
    while let Some(job) = line.next() {
        let request = PreviewRequest { package_dir: job.resolved.dir.clone(), package_prefix: job.resolved.prefix.clone(), key: job.resolved.key.clone(), kind: job.item.kind, spec: PreviewSpec::STANDARD };
        // Another editor instance may have made it since the loader looked.
        if let Some(is_static) = already_published(&store, &request) {
            line.finish(Rendered::Finished(RenderOutcome::Published { is_static }));
            wake();
            continue;
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| renderer.render(&request))).unwrap_or_else(|_| RenderOutcome::Failed { reason: "rendering the preview hit an internal error".to_owned(), recorded: false });
        if let Some(device) = renderer.device() {
            *adapter.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(device);
        }
        match outcome {
            RenderOutcome::CoolingDown(_) => {
                line.put_back();
                let wait = renderer.cooldown_left().unwrap_or(Duration::ZERO).max(MIN_COOLDOWN_SLEEP);
                if !line.sleep(wait) {
                    break;
                }
            }
            RenderOutcome::Stopped(reason) => {
                line.finish(Rendered::Unavailable(reason));
                wake();
                break;
            }
            RenderOutcome::Cancelled => break,
            outcome => {
                line.finish(Rendered::Finished(outcome));
                wake();
            }
        }
    }
    renderer.shutdown();
}
