//! The queue, the loader and the render thread, with a fake renderer and a real store in a scratch
//! folder. No process starts here.

use super::child::{PreviewRequest, RenderOutcome};
use super::worker::{color_image, resolve_package, run_renderer, Line, PackageResolver, RenderJob, Rendered};
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

const SPEC: PreviewSpec = PreviewSpec::STANDARD;
const WAIT: Duration = Duration::from_secs(10);
const DEVICE: &str = "GPU A";
const VERSION: &str = "1.2.3";

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("gpufx-previews-manager-{}-{name}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn still_image(seed: u8) -> PreviewImage {
    PreviewImage { width: SPEC.width, height: SPEC.height, rgba: vec![seed; (SPEC.width * SPEC.height * 4) as usize] }
}

fn strip_image(frames: u32) -> PreviewImage {
    PreviewImage { width: SPEC.width, height: SPEC.height * frames, rgba: vec![9; (SPEC.width * SPEC.height * frames * 4) as usize] }
}

type Behaviour = Arc<dyn Fn(&PreviewRequest) -> RenderOutcome + Send + Sync>;

struct FakeRenderer {
    log: Arc<Mutex<Vec<(String, PreviewJobKind)>>>,
    behaviour: Behaviour,
    cooldown_until: Arc<Mutex<Option<Instant>>>,
    stop: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
}

impl Renderer for FakeRenderer {
    fn render(&mut self, request: &PreviewRequest) -> RenderOutcome {
        self.log.lock().unwrap().push((request.package_dir.file_name().unwrap().to_string_lossy().into_owned(), request.kind));
        (self.behaviour)(request)
    }

    fn device(&self) -> Option<String> {
        Some(DEVICE.to_owned())
    }

    fn cooldown_left(&self) -> Option<Duration> {
        self.cooldown_until.lock().unwrap().map(|until| until.saturating_duration_since(Instant::now()))
    }

    fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    fn shutdown(&mut self) {
        self.ended.store(true, Ordering::SeqCst);
    }
}

/// A manager on a fake renderer, and what the tests look at.
struct Rig {
    manager: PreviewManager,
    ctx: egui::Context,
    root: PathBuf,
    packages: PathBuf,
    log: Arc<Mutex<Vec<(String, PreviewJobKind)>>>,
    cooldown_until: Arc<Mutex<Option<Instant>>>,
    wakes: Arc<AtomicUsize>,
    ended: Arc<AtomicBool>,
}

const PACKAGES: [&str; 3] = ["a", "b", "c"];

/// Writes the preview files the way a helper would.
fn publishing(root: PathBuf) -> Behaviour {
    Arc::new(move |request| {
        let store = PreviewStore::new(root.clone());
        match request.kind {
            PreviewJobKind::Still => store.write_still(&request.package_prefix, &request.key, SPEC, &still_image(1)).unwrap(),
            PreviewJobKind::Loop => store.write_loop(&request.package_prefix, &request.key, SPEC, &strip_image(SPEC.frames)).unwrap(),
        }
        RenderOutcome::Published { is_static: false }
    })
}

fn rig_with(name: &str, make: impl FnOnce(PathBuf) -> Behaviour) -> Rig {
    let dir = scratch(name);
    let (root, packages) = (dir.join("previews"), dir.join("packages"));
    for package in PACKAGES {
        std::fs::create_dir_all(packages.join(package)).unwrap();
        std::fs::write(packages.join(package).join("manifest.json"), package).unwrap();
    }
    let log: Arc<Mutex<Vec<(String, PreviewJobKind)>>> = Arc::default();
    let cooldown_until: Arc<Mutex<Option<Instant>>> = Arc::default();
    let ended: Arc<AtomicBool> = Arc::default();
    let behaviour = make(root.clone());
    let (factory_log, factory_cooldown, factory_ended) = (log.clone(), cooldown_until.clone(), ended.clone());
    let renderer: RendererFactory = Box::new(move || Ok(Box::new(FakeRenderer { log: factory_log, behaviour, cooldown_until: factory_cooldown, stop: Arc::default(), ended: factory_ended }) as Box<dyn Renderer>));
    let wakes: Arc<AtomicUsize> = Arc::default();
    let counter = wakes.clone();
    let mut manager = PreviewManager::default();
    manager.start(backend(root.clone(), packages.clone(), renderer), Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    Rig { manager, ctx: egui::Context::default(), root, packages, log, cooldown_until, wakes, ended }
}

fn resolver(packages: PathBuf) -> PackageResolver {
    Arc::new(move |package| Some(packages.join(package)).filter(|path| path.is_dir()))
}

fn backend(root: PathBuf, packages: PathBuf, renderer: RendererFactory) -> PreviewBackend {
    PreviewBackend { store_root: root, resolve: resolver(packages), renderer, version: VERSION.to_owned() }
}

impl Rig {
    fn frame(&mut self, hovered: Option<&str>, visible: &[&str]) {
        self.manager.begin_frame(&self.ctx);
        self.manager.set_wanted(hovered, visible.iter().copied());
        self.manager.end_frame();
    }

    /// Runs frames until `done` says so.
    fn until(&mut self, hovered: Option<&str>, visible: &[&str], mut done: impl FnMut(&mut PreviewManager) -> bool) {
        let started = Instant::now();
        while !done(&mut self.manager) {
            assert!(started.elapsed() < WAIT, "timed out");
            self.frame(hovered, visible);
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Runs frames for a while.
    fn frames_for(&mut self, hovered: Option<&str>, visible: &[&str], time: Duration) {
        let started = Instant::now();
        while started.elapsed() < time {
            self.frame(hovered, visible);
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn jobs(&self) -> Vec<(String, PreviewJobKind)> {
        self.log.lock().unwrap().clone()
    }

    fn resolved(&self, package: &str) -> Resolved {
        resolve_package(package, &resolver(self.packages.clone()), SPEC).ok().expect("the package is there")
    }

    fn store_still(&self, package: &str) {
        let resolved = self.resolved(package);
        PreviewStore::new(self.root.clone()).write_still(&resolved.prefix, &resolved.key, SPEC, &still_image(2)).unwrap();
    }
}

fn is_still(state: TileState) -> bool {
    matches!(state, TileState::Still(_))
}

fn job(package: &str, kind: PreviewJobKind) -> (String, PreviewJobKind) {
    (package.to_owned(), kind)
}

fn item(package: &str, kind: PreviewJobKind) -> PreviewItem {
    PreviewItem { package: package.to_owned(), kind }
}

// The atlas of a loop.

#[test]
fn a_strip_becomes_rows_and_every_frame_keeps_its_pixels() {
    let frames = 8;
    let mut rgba = Vec::new();
    for frame in 0..frames {
        rgba.extend(vec![frame as u8; (SPEC.width * SPEC.height * 4) as usize]);
    }
    let strip = PreviewImage { width: SPEC.width, height: SPEC.height * frames, rgba };
    let (atlas, count) = super::worker::strip_to_atlas(&strip, SPEC);
    assert_eq!((count, atlas.width, atlas.height), (8, SPEC.width * LOOP_COLUMNS, SPEC.height * 2));
    let pixel = |x: u32, y: u32| atlas.rgba[(y * atlas.width + x) as usize * 4];
    assert_eq!(pixel(0, 0), 0);
    assert_eq!(pixel(SPEC.width * 5, 0), 5);
    assert_eq!(pixel(SPEC.width, SPEC.height + 3), 7);
    assert_eq!(pixel(SPEC.width * 2, SPEC.height), 0, "the rest of the last row is empty");
}

#[test]
fn the_area_of_a_frame_wraps_and_stays_inside_the_texture() {
    // Half a texel of the 6 x 4 frames of the atlas.
    let (half_u, half_v) = (0.5 / (SPEC.width * 6) as f32, 0.5 / (SPEC.height * 4) as f32);
    let near = |a: egui::Pos2, b: egui::Pos2| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6;
    let first = frame_uv(0, 24);
    assert!(near(first.min, egui::pos2(half_u, half_v)), "{first:?}");
    assert!(near(first.max, egui::pos2(1.0 / 6.0 - half_u, 0.25 - half_v)), "{first:?}");
    assert_eq!(frame_uv(24, 24), first, "it wraps");
    let last = frame_uv(23, 24);
    assert!(near(last.min, egui::pos2(5.0 / 6.0 + half_u, 0.75 + half_v)), "{last:?}");
    assert!(near(last.max, egui::pos2(1.0 - half_u, 1.0 - half_v)), "{last:?}");
    let half_u_two = 0.5 / (SPEC.width * 2) as f32;
    assert!(near(frame_uv(1, 2).min, egui::pos2(0.5 + half_u_two, half_v_one())), "fewer frames than a row fill the width");
}

fn half_v_one() -> f32 {
    0.5 / SPEC.height as f32
}

#[test]
fn neighbouring_frames_do_not_share_a_texel_edge() {
    let (a, b) = (frame_uv(0, 24), frame_uv(1, 24));
    assert!(b.min.x - a.max.x >= 1.0 / (SPEC.width * 6) as f32 - 1e-6, "a gap of a texel between the areas");
    let (above, below) = (frame_uv(0, 24), frame_uv(6, 24));
    assert!(below.min.y - above.max.y >= 1.0 / (SPEC.height * 4) as f32 - 1e-6);
}

// The line.

#[test]
fn a_new_list_replaces_what_waits_and_drops_what_is_no_longer_wanted() {
    let line: Line<u32, ()> = Line::new();
    line.replace(vec![1, 2, 3]);
    line.replace(vec![3, 4]);
    assert_eq!(line.next(), Some(3));
    line.finish(());
    assert_eq!(line.next(), Some(4));
}

#[test]
fn a_job_that_runs_or_is_not_collected_yet_does_not_wait_again() {
    let line: Line<u32, ()> = Line::new();
    line.replace(vec![1, 2]);
    assert_eq!(line.next(), Some(1));
    line.replace(vec![1, 2]);
    assert_eq!(line.next(), Some(2));
    line.finish(());
    line.replace(vec![2, 5]);
    assert_eq!(line.next(), Some(5), "2 is done, its result just was not collected");
    line.finish(());
    assert_eq!(line.take_done().len(), 2);
    line.replace(vec![2]);
    assert_eq!(line.next(), Some(2), "once collected it may be asked for again");
}

#[test]
fn a_job_put_back_waits_again_only_when_it_is_still_wanted() {
    let line: Line<u32, ()> = Line::new();
    line.replace(vec![1, 2]);
    assert_eq!(line.next(), Some(1));
    line.put_back();
    assert_eq!(line.next(), Some(1), "first again");
    line.replace(vec![2]);
    line.put_back();
    assert_eq!(line.next(), Some(2));
    line.finish(());
    line.shut_down();
    assert_eq!(line.next(), None);
}

#[test]
fn a_sleep_ends_at_once_on_shutdown_and_not_on_a_new_list() {
    let line: Arc<Line<u32, ()>> = Arc::new(Line::new());
    let sleeper = line.clone();
    let started = Instant::now();
    let handle = thread::spawn(move || sleeper.sleep(Duration::from_secs(30)));
    thread::sleep(Duration::from_millis(30));
    line.replace(vec![1]);
    thread::sleep(Duration::from_millis(30));
    assert!(!handle.is_finished(), "a new list does not end a cooldown");
    line.shut_down();
    assert!(!handle.join().unwrap());
    assert!(started.elapsed() < Duration::from_secs(5));
}

// What is asked for.

#[test]
fn the_hovered_tile_comes_first_with_its_loop_and_only_it_has_one() {
    let items = wanted_items(Some("b"), ["a", "b", "c"]);
    assert_eq!(items, vec![item("b", PreviewJobKind::Still), item("b", PreviewJobKind::Loop), item("a", PreviewJobKind::Still), item("c", PreviewJobKind::Still)]);
}

#[test]
fn the_list_is_bounded_under_the_still_textures() {
    let names: Vec<String> = (0..1000).map(|n| n.to_string()).collect();
    let items = wanted_items(Some("hovered"), names.iter().map(String::as_str));
    assert_eq!(items.len(), MAX_WANTED);
    assert!(MAX_WANTED < STILL_TEXTURES);
    assert_eq!(items[0].package, "hovered");
}

// The cache.

#[test]
fn the_cache_drops_the_least_recently_used_and_a_touch_keeps_one() {
    let mut cache = TileCache::new(2);
    cache.insert("a".to_owned(), 1);
    cache.insert("b".to_owned(), 2);
    assert_eq!(cache.touch("a"), Some(&1));
    cache.insert("c".to_owned(), 3);
    assert!(cache.contains("a"));
    assert!(!cache.contains("b"));
    assert!(cache.contains("c"));
    cache.insert("c".to_owned(), 4);
    assert_eq!(cache.entries.len(), 2, "replacing evicts nothing");
}

#[test]
fn the_limits_are_those_of_the_design() {
    assert_eq!((STILL_TEXTURES, LOOP_TEXTURES, UPLOADS_PER_FRAME), (300, 24, 6));
    assert!(RETRY_DELAY >= Duration::from_secs(30));
}

#[test]
fn a_frame_uploads_at_most_its_budget_and_says_when_more_wait() {
    let mut rig = rig_with("budget", publishing);
    let items: Vec<PreviewItem> = (0..15).map(|n| item(&format!("p{n}"), PreviewJobKind::Still)).collect();
    rig.manager.live = items.iter().cloned().collect();
    for entry in &items {
        rig.manager.stages.insert(entry.clone(), Stage::Uploading);
        rig.manager.uploads.push_back((entry.clone(), color_image(&still_image(3)), 1));
    }
    let ctx = rig.ctx.clone();
    assert!(rig.manager.pump(&ctx), "nine wait");
    assert_eq!(rig.manager.stills.entries.len(), UPLOADS_PER_FRAME);
    assert!(rig.manager.pump(&ctx));
    assert!(!rig.manager.pump(&ctx), "the last three went out");
    assert_eq!(rig.manager.stills.entries.len(), 15);
}

#[test]
fn a_picture_no_longer_wanted_is_not_uploaded_and_costs_no_budget() {
    let mut rig = rig_with("stale-upload", publishing);
    let wanted = item("kept", PreviewJobKind::Still);
    rig.manager.live = [wanted.clone()].into_iter().collect();
    for n in 0..10 {
        let stale = item(&format!("gone{n}"), PreviewJobKind::Still);
        rig.manager.stages.insert(stale.clone(), Stage::Uploading);
        rig.manager.uploads.push_back((stale, color_image(&still_image(3)), 1));
    }
    rig.manager.stages.insert(wanted.clone(), Stage::Uploading);
    rig.manager.uploads.push_back((wanted, color_image(&still_image(3)), 1));
    let ctx = rig.ctx.clone();
    assert!(!rig.manager.pump(&ctx));
    assert_eq!(rig.manager.stills.entries.len(), 1);
    assert!(rig.manager.stills.contains("kept"));
    assert!(matches!(rig.manager.tile("gone0"), TileState::Loading), "it can be read again");
}

// Disk and rendering.

#[test]
fn a_preview_on_disk_costs_no_job() {
    let mut rig = rig_with("disk-hit", publishing);
    rig.store_still("a");
    rig.until(None, &["a"], |manager| is_still(manager.tile("a")));
    assert!(rig.jobs().is_empty());
}

#[test]
fn a_missing_still_is_rendered_once_and_then_read_from_disk() {
    let mut rig = rig_with("render", publishing);
    rig.until(None, &["a"], |manager| is_still(manager.tile("a")));
    rig.frames_for(None, &["a"], Duration::from_millis(50));
    assert_eq!(rig.jobs(), vec![job("a", PreviewJobKind::Still)]);
}

#[test]
fn only_a_hovered_tile_renders_a_loop() {
    let mut rig = rig_with("hover", publishing);
    rig.until(None, &["a", "b"], |manager| is_still(manager.tile("a")) && is_still(manager.tile("b")));
    assert!(rig.jobs().iter().all(|(_, kind)| *kind == PreviewJobKind::Still));
    rig.until(Some("a"), &["a", "b"], |manager| matches!(manager.animation("a"), AnimationState::Frames(_)));
    let AnimationState::Frames(frames) = rig.manager.animation("a") else { panic!("no frames") };
    assert_eq!(frames.frames, SPEC.frames);
    assert!(frames.texture.size().iter().all(|side| *side <= 2048), "{:?}", frames.texture.size());
    assert_eq!(rig.jobs().iter().filter(|(_, kind)| *kind == PreviewJobKind::Loop).count(), 1);
}

#[test]
fn a_strip_of_one_frame_is_static() {
    let mut rig = rig_with("static", |root| {
        Arc::new(move |request| {
            let store = PreviewStore::new(root.clone());
            match request.kind {
                PreviewJobKind::Still => store.write_still(&request.package_prefix, &request.key, SPEC, &still_image(1)).unwrap(),
                PreviewJobKind::Loop => store.write_loop(&request.package_prefix, &request.key, SPEC, &strip_image(1)).unwrap(),
            }
            RenderOutcome::Published { is_static: true }
        })
    });
    rig.until(Some("a"), &["a"], |manager| matches!(manager.animation("a"), AnimationState::Static));
}

#[test]
fn the_hovered_tile_is_rendered_before_the_others() {
    let gate = Arc::new(AtomicBool::new(false));
    let opened = gate.clone();
    let mut rig = rig_with("order", |root| {
        let publish = publishing(root);
        Arc::new(move |request| {
            while !opened.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            publish(request)
        })
    });
    // The first tile holds the render thread until the rest have queued up.
    rig.until(None, &["a"], |manager| !manager.published_jobs.is_empty());
    rig.until(Some("c"), &["b", "c"], |manager| manager.published_jobs.len() == 2);
    gate.store(true, Ordering::SeqCst);
    rig.until(Some("c"), &["b", "c"], |manager| is_still(manager.tile("b")));
    let jobs = rig.jobs();
    let position = |wanted: (String, PreviewJobKind)| jobs.iter().position(|found| *found == wanted).unwrap();
    assert!(position(job("c", PreviewJobKind::Still)) < position(job("b", PreviewJobKind::Still)));
}

#[test]
fn tiles_no_longer_wanted_are_not_rendered() {
    let gate = Arc::new(AtomicBool::new(false));
    let opened = gate.clone();
    let mut rig = rig_with("dropped", |root| {
        let publish = publishing(root);
        Arc::new(move |request| {
            while !opened.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            publish(request)
        })
    });
    rig.until(None, &["a", "b"], |manager| manager.published_jobs.len() == 2);
    rig.frames_for(None, &["c"], Duration::from_millis(30));
    gate.store(true, Ordering::SeqCst);
    rig.until(None, &["c"], |manager| is_still(manager.tile("c")));
    let jobs = rig.jobs();
    assert!(!jobs.contains(&job("b", PreviewJobKind::Still)), "{jobs:?}");
}

#[test]
fn no_new_job_is_sent_while_paused_and_the_jobs_follow_afterwards() {
    let mut rig = rig_with("paused", publishing);
    rig.manager.set_paused(true);
    rig.until(None, &["a"], |manager| matches!(manager.tile("a"), TileState::Pending));
    rig.frames_for(None, &["a"], Duration::from_millis(60));
    assert!(rig.jobs().is_empty());
    rig.manager.set_paused(false);
    rig.until(None, &["a"], |manager| is_still(manager.tile("a")));
    assert_eq!(rig.jobs().len(), 1);
}

#[test]
fn pausing_takes_back_the_jobs_that_wait_and_the_running_one_finishes() {
    let gate = Arc::new(AtomicBool::new(false));
    let opened = gate.clone();
    let mut rig = rig_with("pause-queue", |root| {
        let publish = publishing(root);
        Arc::new(move |request| {
            while !opened.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            publish(request)
        })
    });
    rig.until(None, &["a", "b"], |manager| manager.published_jobs.len() == 2);
    rig.manager.set_paused(true);
    rig.frames_for(None, &["a", "b"], Duration::from_millis(30));
    gate.store(true, Ordering::SeqCst);
    rig.until(None, &["a", "b"], |manager| is_still(manager.tile("a")));
    rig.frames_for(None, &["a", "b"], Duration::from_millis(40));
    assert_eq!(rig.jobs(), vec![job("a", PreviewJobKind::Still)]);
}

#[test]
fn a_failure_on_record_for_this_adapter_costs_no_job() {
    let mut rig = rig_with("known-failure", publishing);
    // A first job makes the adapter known.
    rig.until(None, &["a"], |manager| is_still(manager.tile("a")));
    let resolved = rig.resolved("b");
    PreviewStore::new(rig.root.clone()).mark_failed_on(&resolved.prefix, &resolved.key, DEVICE, VERSION, "it is broken").unwrap();
    rig.until(None, &["b"], |manager| matches!(manager.tile("b"), TileState::Failed(_)));
    let TileState::Failed(reason) = rig.manager.tile("b") else { panic!("not failed") };
    assert_eq!(reason, "it is broken");
    assert_eq!(rig.jobs().len(), 1);
}

#[test]
fn a_failure_that_may_pass_is_not_tried_again_at_once_and_shows_its_reason() {
    let mut rig = rig_with("no-immediate-retry", |_| Arc::new(|_: &PreviewRequest| RenderOutcome::Failed { reason: "shader error".to_owned(), recorded: false }));
    rig.until(None, &["a"], |manager| matches!(manager.tile("a"), TileState::Failed(_)));
    rig.frames_for(None, &["a"], Duration::from_millis(100));
    assert_eq!(rig.jobs().len(), 1, "the delay is 30 s");
    let TileState::Failed(reason) = rig.manager.tile("a") else { panic!("not failed") };
    assert_eq!(reason, "shader error");
}

#[test]
fn a_hovered_tile_whose_still_fails_does_not_render_its_loop() {
    let mut rig = rig_with("hover-failed-still", |_| Arc::new(|_: &PreviewRequest| RenderOutcome::Failed { reason: "package is broken".to_owned(), recorded: false }));
    rig.frames_for(Some("a"), &["a"], Duration::from_millis(300));
    assert_eq!(rig.jobs(), vec![job("a", PreviewJobKind::Still)], "the loop waits for a still that is good");
}

#[test]
fn a_preview_another_instance_already_made_is_not_rendered() {
    for kind in [PreviewJobKind::Still, PreviewJobKind::Loop] {
        let dir = scratch("already");
        let (root, packages) = (dir.join("previews"), dir.join("packages"));
        std::fs::create_dir_all(packages.join("a")).unwrap();
        std::fs::write(packages.join("a").join("manifest.json"), "a").unwrap();
        let resolved = Arc::new(resolve_package("a", &resolver(packages), SPEC).ok().unwrap());
        let store = PreviewStore::new(root.clone());
        match kind {
            PreviewJobKind::Still => store.write_still(&resolved.prefix, &resolved.key, SPEC, &still_image(2)).unwrap(),
            PreviewJobKind::Loop => store.write_loop(&resolved.prefix, &resolved.key, SPEC, &strip_image(SPEC.frames)).unwrap(),
        }
        let log: Arc<Mutex<Vec<(String, PreviewJobKind)>>> = Arc::default();
        let renderer_log = log.clone();
        let factory: RendererFactory = Box::new(move || Ok(Box::new(FakeRenderer { log: renderer_log, behaviour: Arc::new(|_: &PreviewRequest| RenderOutcome::Cancelled), cooldown_until: Arc::default(), stop: Arc::default(), ended: Arc::default() }) as Box<dyn Renderer>));
        let line: Arc<Line<RenderJob, Rendered>> = Arc::new(Line::new());
        let worker_line = line.clone();
        let handle = thread::spawn(move || run_renderer(worker_line, factory, PreviewStore::new(root), Arc::default(), Arc::new(|| {})));
        line.replace(vec![RenderJob { item: item("a", kind), resolved }]);
        let started = Instant::now();
        let done = loop {
            let done = line.take_done();
            if !done.is_empty() {
                break done;
            }
            assert!(started.elapsed() < WAIT, "no result");
            thread::sleep(Duration::from_millis(2));
        };
        line.shut_down();
        handle.join().unwrap();
        assert!(matches!(done[0].1, Rendered::Finished(RenderOutcome::Published { is_static: false })), "{kind:?}");
        assert!(log.lock().unwrap().is_empty(), "{kind:?} was rendered");
    }
}

#[test]
fn a_failure_that_may_pass_is_tried_again_by_a_later_frame_after_the_delay() {
    let mut rig = rig_with("retry", |_| Arc::new(|_: &PreviewRequest| RenderOutcome::Failed { reason: "shader error".to_owned(), recorded: false }));
    rig.manager.retry_delay = Duration::from_millis(60);
    rig.until(None, &["a"], |manager| matches!(manager.tile("a"), TileState::Failed(_)));
    let failed_at = Instant::now();
    // No frame wants it during the delay, and nothing happens by itself.
    thread::sleep(Duration::from_millis(120));
    assert_eq!(rig.jobs().len(), 1);
    let log = rig.log.clone();
    rig.until(None, &["a"], |_| log.lock().unwrap().len() >= 2);
    assert!(failed_at.elapsed() >= Duration::from_millis(60));
}

#[test]
fn final_failures_are_shown_and_not_tried_again() {
    for outcome in [RenderOutcome::GivenUp("given up".to_owned()), RenderOutcome::NotPreviewable("not previewable".to_owned()), RenderOutcome::AlreadyFailed("failed before".to_owned()), RenderOutcome::Failed { reason: "recorded".to_owned(), recorded: true }] {
        let shown = outcome.clone();
        let mut rig = rig_with("final", move |_| {
            let outcome = shown.clone();
            Arc::new(move |_: &PreviewRequest| outcome.clone())
        });
        rig.manager.retry_delay = Duration::ZERO;
        rig.until(None, &["a"], |manager| matches!(manager.tile("a"), TileState::Failed(_)));
        rig.frames_for(None, &["a"], Duration::from_millis(30));
        assert_eq!(rig.jobs().len(), 1, "{outcome:?}");
    }
}

#[test]
fn a_package_that_cannot_be_found_is_failed_without_a_job() {
    let mut rig = rig_with("no-package", publishing);
    rig.until(None, &["nowhere"], |manager| matches!(manager.tile("nowhere"), TileState::Failed(_)));
    assert!(rig.jobs().is_empty());
}

#[test]
fn a_cooldown_sleeps_and_does_not_spin() {
    let until = Arc::new(Mutex::new(None));
    let shared = until.clone();
    let mut rig = rig_with("cooldown", |root| {
        let publish = publishing(root);
        Arc::new(move |request| {
            let cooling = shared.lock().unwrap().is_some_and(|until: Instant| Instant::now() < until);
            if cooling {
                RenderOutcome::CoolingDown("too many deaths".to_owned())
            } else {
                publish(request)
            }
        })
    });
    let end = Instant::now() + Duration::from_millis(300);
    *until.lock().unwrap() = Some(end);
    *rig.cooldown_until.lock().unwrap() = Some(end);
    rig.until(None, &["a"], |manager| is_still(manager.tile("a")));
    assert!(Instant::now() >= end, "it waited the cooldown out");
    assert_eq!(rig.jobs().len(), 2, "one attempt to see the cooldown, one after it: {:?}", rig.jobs());
}

#[test]
fn a_shutdown_ends_a_cooldown_and_the_renderer_ends_on_its_thread() {
    let mut rig = rig_with("cooldown-shutdown", |_| Arc::new(|_: &PreviewRequest| RenderOutcome::CoolingDown("too many deaths".to_owned())));
    *rig.cooldown_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs(600));
    rig.frames_for(None, &["a"], Duration::from_millis(50));
    let ended = rig.ended.clone();
    let started = Instant::now();
    drop(rig);
    assert!(started.elapsed() < Duration::from_secs(1));
    let waited = Instant::now();
    while !ended.load(Ordering::SeqCst) {
        assert!(waited.elapsed() < WAIT, "the renderer never ended");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn dropping_the_manager_does_not_wait_for_a_stuck_job() {
    let finish = Arc::new(AtomicBool::new(false));
    let release = finish.clone();
    let mut rig = rig_with("stuck", |_| {
        Arc::new(move |_: &PreviewRequest| {
            while !release.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
            RenderOutcome::Cancelled
        })
    });
    rig.until(None, &["a"], |manager| !manager.published_jobs.is_empty());
    thread::sleep(Duration::from_millis(20));
    let started = Instant::now();
    drop(rig);
    assert!(started.elapsed() < Duration::from_secs(1), "the UI thread waited {:?}", started.elapsed());
    finish.store(true, Ordering::SeqCst);
}

#[test]
fn a_shutdown_sets_the_stop_flag_of_the_renderer() {
    let stop: Arc<AtomicBool> = Arc::default();
    let flag = stop.clone();
    let root = scratch("stop").join("previews");
    let renderer: RendererFactory = Box::new(move || Ok(Box::new(FakeRenderer { log: Arc::default(), behaviour: Arc::new(|_: &PreviewRequest| RenderOutcome::Cancelled), cooldown_until: Arc::default(), stop: flag, ended: Arc::default() }) as Box<dyn Renderer>));
    let mut manager = PreviewManager::default();
    manager.start(backend(root, scratch("stop-packages"), renderer), Arc::new(|| {}));
    drop(manager);
    let waited = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        assert!(waited.elapsed() < WAIT, "the flag was never set");
        thread::sleep(Duration::from_millis(2));
    }
}

// What there is when nothing can run.

#[test]
fn a_manager_that_was_not_started_runs_nothing_and_says_so() {
    let mut manager = PreviewManager::default();
    let ctx = egui::Context::default();
    manager.begin_frame(&ctx);
    manager.set_wanted(Some("a"), ["a"]);
    manager.end_frame();
    assert!(manager.threads.is_none());
    assert!(matches!(manager.tile("a"), TileState::Unavailable(_)));
    assert!(matches!(manager.animation("a"), AnimationState::Unavailable));
}

#[test]
fn without_a_helper_what_is_on_disk_still_shows_and_the_rest_is_unavailable() {
    let dir = scratch("no-helper");
    let (root, packages) = (dir.join("previews"), dir.join("packages"));
    for package in ["a", "b"] {
        std::fs::create_dir_all(packages.join(package)).unwrap();
        std::fs::write(packages.join(package).join("manifest.json"), package).unwrap();
    }
    let resolved = resolve_package("a", &resolver(packages.clone()), SPEC).ok().unwrap();
    PreviewStore::new(root.clone()).write_still(&resolved.prefix, &resolved.key, SPEC, &still_image(2)).unwrap();
    let renderer: RendererFactory = Box::new(|| Err("the preview helper does not exist".to_owned()));
    let mut manager = PreviewManager::default();
    manager.start(backend(root, packages, renderer), Arc::new(|| {}));
    let ctx = egui::Context::default();
    let started = Instant::now();
    loop {
        manager.begin_frame(&ctx);
        manager.set_wanted(None, ["a", "b"]);
        manager.end_frame();
        if is_still(manager.tile("a")) && matches!(manager.tile("b"), TileState::Unavailable(_)) {
            break;
        }
        assert!(started.elapsed() < WAIT, "timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_stopped_session_makes_previews_unavailable_with_its_reason() {
    let mut rig = rig_with("stopped", |_| Arc::new(|_: &PreviewRequest| RenderOutcome::Stopped("another protocol".to_owned())));
    rig.until(None, &["a"], |manager| matches!(manager.tile("a"), TileState::Unavailable(_)));
    let TileState::Unavailable(reason) = rig.manager.tile("a") else { panic!("available") };
    assert_eq!(reason, "another protocol");
}

// Startup and repaints.

#[test]
fn a_stale_temporary_file_is_swept_at_startup() {
    let dir = scratch("sweep");
    let root = dir.join("previews");
    std::fs::create_dir_all(&root).unwrap();
    let stale = root.join("render-1-1.tmp");
    let file = std::fs::File::create(&stale).unwrap();
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60)).unwrap();
    drop(file);
    let renderer: RendererFactory = Box::new(|| Err("no helper".to_owned()));
    let mut manager = PreviewManager::default();
    manager.start(backend(root, dir.join("packages"), renderer), Arc::new(|| {}));
    let started = Instant::now();
    while stale.exists() {
        assert!(started.elapsed() < WAIT, "not swept");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_built_in_picture_is_decoded_off_the_ui_thread() {
    let mut rig = rig_with("original", publishing);
    rig.until(None, &[], |manager| manager.original().is_some());
    assert_eq!(rig.manager.original().unwrap().size(), [SPEC.width as usize, SPEC.height as usize]);
}

#[test]
fn nothing_wakes_the_ui_once_everything_has_arrived() {
    let mut rig = rig_with("quiet", publishing);
    rig.until(None, &["a"], |manager| is_still(manager.tile("a")));
    let ctx = rig.ctx.clone();
    assert!(!rig.manager.pump(&ctx), "nothing waits");
    rig.frames_for(None, &["a"], Duration::from_millis(20));
    let wakes = rig.wakes.load(Ordering::SeqCst);
    rig.frames_for(None, &["a"], Duration::from_millis(60));
    assert_eq!(rig.wakes.load(Ordering::SeqCst), wakes, "no work, no wake");
}
