use super::backend::FfmpegGenerator;
use super::worker::{Done, Generator};
use super::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::Time;

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gpufx-visuals-unit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn clip(name: &str, arguments: &[&str]) -> PathBuf {
    let path = scratch_dir().join(name);
    let tool = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe");
    let status = std::process::Command::new(tool).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(&path).status().expect("ffmpeg runs");
    assert!(status.success(), "generating {name} failed");
    path
}

/// Runs frames of `visuals` until `done`; returns the longest time one frame took on this thread.
fn run_frames(visuals: &mut ClipVisuals, ctx: &egui::Context, mut draw: impl FnMut(&mut ClipVisuals), mut done: impl FnMut(&mut ClipVisuals) -> bool) -> Duration {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut longest = Duration::ZERO;
    loop {
        let started = Instant::now();
        visuals.begin_frame(ctx);
        draw(visuals);
        visuals.end_frame();
        longest = longest.max(started.elapsed());
        if done(visuals) {
            return longest;
        }
        assert!(Instant::now() < deadline, "generation did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_known(visuals: &mut ClipVisuals, ctx: &egui::Context, source: &str) -> SourceView {
    let slot = visuals.slot(source);
    run_frames(visuals, ctx, |v| { v.view(slot); }, |v| v.slots[slot].view != SourceView::Pending);
    visuals.view(slot)
}

fn peaks_of(path: &Path) -> Vec<f32> {
    let mut all = Vec::new();
    FfmpegGenerator.peaks(path, &mut |peaks| { all.extend_from_slice(peaks); true }).expect("read the audio");
    all
}

#[test]
fn silence_has_only_zero_peaks() {
    let path = clip("silence.wav", &["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo:d=1", "-c:a", "pcm_s16le"]);
    let peaks = peaks_of(&path);
    assert!((peaks.len() as i64 - 50).abs() <= 1, "{}", peaks.len());
    assert!(peaks.iter().all(|peak| *peak == 0.0));
}

#[test]
fn varying_audio_has_varying_peaks() {
    let path = clip("varying.wav", &["-f", "lavfi", "-i", "aevalsrc='0.9*sin(440*2*PI*t)*gt(mod(t\\,1)\\,0.5)':s=48000:d=2", "-c:a", "pcm_s16le"]);
    let peaks = peaks_of(&path);
    let quiet = peaks.iter().take(20).copied().fold(0.0_f32, f32::max);
    let loud = peaks.iter().skip(30).take(15).copied().fold(0.0_f32, f32::max);
    assert!(quiet < 0.01, "{quiet}");
    assert!(loud > 0.5, "{loud}");
}

#[test]
fn a_file_without_audio_is_known_to_have_none() {
    let path = clip("no-audio.mp4", &["-f", "lavfi", "-i", "testsrc=s=320x180:r=30:d=2", "-an", "-c:v", "mpeg4"]);
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    match wait_known(&mut visuals, &ctx, path.to_str().unwrap()) {
        SourceView::Known(facts) => {
            assert!(facts.has_video && !facts.has_audio);
            assert!((facts.aspect - 16.0 / 9.0).abs() < 0.02, "{}", facts.aspect);
        }
        other => panic!("{other:?}"),
    }
    let slot = visuals.slot(path.to_str().unwrap());
    assert!(visuals.peaks(slot).is_none());
    assert!(visuals.wanted_audio.is_empty(), "no levels are asked of a file without sound");
    visuals.end_frame();
}

#[test]
fn a_corrupt_file_stays_failed_and_is_not_asked_again() {
    let path = scratch_dir().join("corrupt.mp4");
    std::fs::write(&path, [0x5A_u8; 8192]).expect("write");
    let source = path.to_str().unwrap();
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    assert_eq!(wait_known(&mut visuals, &ctx, source), SourceView::Failed);
    let slot = visuals.slot(source);
    for _ in 0..5 {
        visuals.begin_frame(&ctx);
        assert_eq!(visuals.view(slot), SourceView::Failed);
        assert_eq!(visuals.tile(slot, 0, TICKS_PER_SECOND), None);
        assert!(visuals.wanted_frames.iter().all(|job| matches!(job.key, JobKey::Stamp(_))), "only the slow check of the file remains");
        visuals.end_frame();
    }
}

#[test]
fn a_replaced_file_gets_new_pictures() {
    let path = clip("replaced.mp4", &["-f", "lavfi", "-i", "color=c=red:s=320x180:r=30:d=1", "-an", "-c:v", "mpeg4"]);
    let source = path.to_str().unwrap().to_owned();
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    wait_known(&mut visuals, &ctx, &source);
    let slot = visuals.slot(&source);
    let first = visuals.slots[slot].identity.expect("known");
    clip("replaced.mp4", &["-f", "lavfi", "-i", "color=c=blue:s=320x180:r=30:d=5", "-an", "-c:v", "mpeg4"]);
    visuals.slots[slot].checked = None;
    run_frames(&mut visuals, &ctx, |v| { v.slot(&source); v.view(slot); }, |v| v.slots[slot].identity.is_some_and(|identity| identity != first));
    assert!(!visuals.live_identities.contains(&first));
}

/// Mean of the red and of the blue channel.
fn red_and_blue(image: &ThumbnailImage) -> (u32, u32) {
    let pixels = image.rgba.len() as u32 / 4;
    let sum = |channel: usize| image.rgba.iter().skip(channel).step_by(4).map(|value| u32::from(*value)).sum::<u32>() / pixels;
    (sum(0), sum(2))
}

#[test]
fn a_file_overwritten_in_place_is_decoded_anew_under_its_new_identity() {
    let never = || false;
    let path = clip("overwritten.mp4", &["-f", "lavfi", "-i", "color=c=red:s=320x180:r=30:d=1", "-an", "-c:v", "mpeg4"]);
    let mut local = None;
    let old = FfmpegGenerator.frame(&mut local, 1, &path, Time::ZERO, &never).expect("the old file");
    let (red, blue) = red_and_blue(&old);
    assert!(red > blue + 80, "{red} {blue}");
    clip("overwritten.mp4", &["-f", "lavfi", "-i", "color=c=blue:s=320x180:r=30:d=1", "-an", "-c:v", "mpeg4"]);
    let new = FfmpegGenerator.frame(&mut local, 2, &path, Time::ZERO, &never).expect("the new file");
    let (red, blue) = red_and_blue(&new);
    assert!(blue > red + 80, "the worker still reads the old file: {red} {blue}");
}

#[test]
fn pictures_are_made_in_the_background_and_the_frame_stays_cheap() {
    let path = clip("strip.mp4", &["-f", "lavfi", "-i", "testsrc=s=640x360:r=30:d=8", "-f", "lavfi", "-i", "sine=f=330:d=8", "-c:v", "mpeg4", "-c:a", "aac"]);
    let source = path.to_str().unwrap().to_owned();
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    wait_known(&mut visuals, &ctx, &source);
    let slot = visuals.slot(&source);
    let step = TICKS_PER_SECOND;
    let wanted: Vec<i64> = (0..8).map(|i| i * step).collect();
    let mut frames_seen = 0;
    let longest = run_frames(
        &mut visuals,
        &ctx,
        |v| {
            frames_seen += 1;
            for bucket in &wanted {
                v.tile(slot, *bucket, step);
            }
            v.peaks(slot);
        },
        |v| {
            let identity = v.slots[slot].identity.expect("known");
            let tiles = wanted.iter().all(|bucket| matches!(v.tiles.peek_mut((identity, *bucket)), Some(Tile::Ready(_))));
            let levels = v.peaks.get(&identity).is_some_and(|entry| entry.finished);
            tiles && levels
        },
    );
    println!("visuals frame: longest {longest:?}, {frames_seen} frames");
    assert!(longest < Duration::from_millis(100), "a frame took {longest:?}");
    let identity = visuals.slots[slot].identity.expect("known");
    assert!(visuals.peaks[&identity].data.len() >= 390, "{}", visuals.peaks[&identity].data.len());
    let distinct: std::collections::HashSet<TextureId> = wanted.iter().filter_map(|bucket| visuals.tile(slot, *bucket, step)).collect();
    assert_eq!(distinct.len(), wanted.len(), "every bucket has its own texture");
    visuals.end_frame();
}

#[test]
fn eviction_frees_the_egui_texture() {
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let baseline = ctx.tex_manager().read().allocated().count();
    let mut visuals = ClipVisuals::default();
    visuals.tiles = TileCache::new(2);
    visuals.live_identities.insert(7);
    let image = ThumbnailImage { width: 2, height: 2, rgba: vec![9; 16] };
    for bucket in 0..3 {
        visuals.apply(Done::Frame { identity: 7, bucket, result: Ok(image.clone()) });
        visuals.begin_frame(&ctx);
    }
    assert_eq!(visuals.tiles.len(), 2);
    assert_eq!(ctx.tex_manager().read().allocated().count(), baseline + 2, "the evicted texture was released");
}

#[test]
fn a_tile_borrows_a_neighbour_until_its_own_picture_arrives() {
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    visuals.slots.push(Slot { path: Path::new("a.mp4").into(), stamp: None, identity: Some(3), view: SourceView::Pending, checked: None, stamp_lost: false });
    visuals.live_identities.insert(3);
    let image = ThumbnailImage { width: 2, height: 2, rgba: vec![9; 16] };
    visuals.apply(Done::Frame { identity: 3, bucket: 0, result: Ok(image) });
    visuals.begin_frame(&ctx);
    assert!(visuals.tile(0, TICKS_PER_SECOND, TICKS_PER_SECOND).is_some(), "the neighbour stands in");
    assert_eq!(visuals.wanted_frames.len(), 1, "and the real picture is requested");
    assert!(visuals.tile(0, 900 * TICKS_PER_SECOND, TICKS_PER_SECOND).is_none(), "too far to borrow");
    visuals.end_frame();
}

#[test]
fn a_failed_tile_is_not_requested_again() {
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    visuals.slots.push(Slot { path: Path::new("a.mp4").into(), stamp: None, identity: Some(3), view: SourceView::Pending, checked: None, stamp_lost: false });
    visuals.live_identities.insert(3);
    visuals.apply(Done::Frame { identity: 3, bucket: 0, result: Err(ThumbnailError::Failed("bad".to_owned())) });
    visuals.begin_frame(&ctx);
    assert!(visuals.tile(0, 0, TICKS_PER_SECOND).is_none());
    assert!(visuals.wanted_frames.is_empty());
    visuals.end_frame();
}

#[test]
fn a_tile_that_ran_out_of_time_is_tried_again_a_bounded_number_of_times() {
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, None, 3);
    let timed_out = || Done::Frame { identity: 3, bucket: 0, result: Err(ThumbnailError::TimedOut) };
    let wanted = |visuals: &mut ClipVisuals| {
        visuals.wanted_frames.clear();
        visuals.wanted_keys.clear();
        visuals.tile(0, 0, TICKS_PER_SECOND);
        visuals.wanted_frames.iter().any(|job| job.key == JobKey::Frame(3, 0))
    };
    let due = |visuals: &mut ClipVisuals| match visuals.tiles.peek_mut((3, 0)) {
        Some(Tile::Retry { not_before, .. }) => *not_before = Instant::now(),
        _ => panic!("a retry is pending"),
    };
    visuals.apply(timed_out());
    assert!(matches!(visuals.tiles.peek_mut((3, 0)), Some(Tile::Retry { .. })), "a timeout is not a failure");
    assert!(!wanted(&mut visuals), "not at once, so it cannot spin");
    for _ in 1..=MAX_TIMEOUT_RETRIES {
        due(&mut visuals);
        assert!(wanted(&mut visuals), "asked again when it falls due");
        visuals.apply(timed_out());
    }
    assert!(matches!(visuals.tiles.peek_mut((3, 0)), Some(Tile::Failed)), "the retries end");
    assert!(!wanted(&mut visuals));
    visuals.wanted_frames.clear();
}

#[test]
fn a_retried_tile_keeps_its_picture_when_it_finally_comes() {
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, None, 3);
    visuals.apply(Done::Frame { identity: 3, bucket: 0, result: Err(ThumbnailError::TimedOut) });
    visuals.apply(Done::Frame { identity: 3, bucket: 0, result: Ok(ThumbnailImage { width: 1, height: 1, rgba: vec![0; 4] }) });
    visuals.begin_frame(&ctx);
    assert!(visuals.tile(0, 0, TICKS_PER_SECOND).is_some());
    visuals.wanted_frames.clear();
}

#[test]
fn a_late_result_for_a_replaced_file_is_ignored() {
    let mut visuals = ClipVisuals::default();
    visuals.apply(Done::Frame { identity: 9, bucket: 0, result: Ok(ThumbnailImage { width: 1, height: 1, rgba: vec![0; 4] }) });
    assert!(visuals.tiles.is_empty());
    visuals.apply(Done::Peaks { identity: 9, run: 0, peaks: vec![0.5], finished: true });
    assert!(visuals.peaks.is_empty());
}

#[test]
fn a_cancelled_run_of_levels_leaves_nothing_and_the_next_starts_over() {
    let facts = SourceFacts { has_video: false, has_audio: true, aspect: 1.0, length: None };
    let mut visuals = ClipVisuals::default();
    visuals.slots.push(Slot { path: Path::new("a.mp4").into(), stamp: None, identity: Some(1), view: SourceView::Known(facts), checked: None, stamp_lost: false });
    visuals.live_identities.insert(1);
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: vec![0.1], finished: false });
    let project = crate::editor::EditorState::default().project().clone();
    visuals.sync_project(1, &project);
    // The worker's last words about the run it was told to stop.
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: vec![0.2], finished: false });
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: Vec::new(), finished: false });
    assert!(visuals.peaks.is_empty(), "no entry may come back, or the clip would stay without bars");
    assert!(visuals.peaks(0).is_none());
    assert_eq!(visuals.wanted_audio[0].key, JobKey::Peaks(1, 1), "undo asks for a new run");
    visuals.apply(Done::Peaks { identity: 1, run: 1, peaks: vec![0.7], finished: false });
    assert_eq!(visuals.peaks[&1].data.max_between(0, 1), 0.7, "the first bar is the start of the file");
    visuals.end_frame();
}

fn known_slot(visuals: &mut ClipVisuals, stamp: Option<FileStamp>, identity: u32) {
    let facts = SourceFacts { has_video: true, has_audio: true, aspect: 1.0, length: None };
    visuals.slots.push(Slot { path: Path::new("a.mp4").into(), stamp, identity: Some(identity), view: SourceView::Known(facts), checked: None, stamp_lost: false });
    visuals.live_identities.insert(identity);
    visuals.interned.insert("a.mp4".to_owned(), visuals.slots.len() - 1);
}

#[test]
fn a_moment_without_a_stamp_keeps_what_was_made_of_the_source() {
    let stamp = FileStamp { size: 10, modified: None };
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, Some(stamp), 1);
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: vec![0.1], finished: true });
    visuals.apply(Done::Stamp { slot: 0, stamp: None });
    assert_eq!(visuals.slots[0].identity, Some(1), "the file may be locked for a moment");
    assert!(!visuals.peaks.is_empty());
    visuals.apply(Done::Stamp { slot: 0, stamp: Some(stamp) });
    assert_eq!(visuals.slots[0].identity, Some(1));
    visuals.apply(Done::Stamp { slot: 0, stamp: Some(FileStamp { size: 11, modified: None }) });
    assert_eq!(visuals.slots[0].identity, None, "a real change still forgets it");
    assert!(visuals.peaks.is_empty());
}

#[test]
fn a_drive_that_comes_back_retries_only_what_failed() {
    let ctx = egui::Context::default();
    let stamp = FileStamp { size: 10, modified: None };
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, Some(stamp), 1);
    visuals.apply(Done::Frame { identity: 1, bucket: 0, result: Ok(ThumbnailImage { width: 1, height: 1, rgba: vec![0; 4] }) });
    visuals.begin_frame(&ctx);
    visuals.apply(Done::Frame { identity: 1, bucket: 7, result: Err("gone".to_owned().into()) });
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: Vec::new(), finished: true });
    visuals.apply(Done::Stamp { slot: 0, stamp: None });
    assert!(matches!(visuals.tiles.peek_mut((1, 7)), Some(Tile::Failed)), "nothing changes while the drive is away");
    visuals.apply(Done::Stamp { slot: 0, stamp: Some(stamp) });
    assert_eq!(visuals.slots[0].identity, Some(1), "the same file, so the same identity");
    assert!(visuals.tiles.peek_mut((1, 7)).is_none(), "the failure is forgotten");
    assert!(matches!(visuals.tiles.peek_mut((1, 0)), Some(Tile::Ready(_))), "a good picture stays");
    assert!(visuals.peaks.is_empty(), "levels that read nothing are read again");
    visuals.tile(0, 7, TICKS_PER_SECOND);
    assert!(visuals.wanted_frames.iter().any(|job| job.key == JobKey::Frame(1, 7)), "and the picture is asked for again");
    visuals.wanted_frames.clear();
}

#[test]
fn levels_with_content_survive_a_drive_that_comes_back() {
    let stamp = FileStamp { size: 10, modified: None };
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, Some(stamp), 1);
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: vec![0.3], finished: true });
    visuals.apply(Done::Stamp { slot: 0, stamp: None });
    visuals.apply(Done::Stamp { slot: 0, stamp: Some(stamp) });
    assert!(!visuals.peaks.is_empty());
}

#[test]
fn a_stamp_that_was_never_lost_clears_nothing() {
    let stamp = FileStamp { size: 10, modified: None };
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, Some(stamp), 1);
    visuals.apply(Done::Frame { identity: 1, bucket: 7, result: Err("damaged".to_owned().into()) });
    visuals.apply(Done::Stamp { slot: 0, stamp: Some(stamp) });
    assert!(matches!(visuals.tiles.peek_mut((1, 7)), Some(Tile::Failed)), "a damaged file is not read again every five seconds");
}

#[test]
fn a_stamp_request_lost_to_busy_workers_is_made_again() {
    let wanted = |visuals: &ClipVisuals| visuals.wanted_frames.iter().any(|job| matches!(job.key, JobKey::Stamp(0)));
    let mut visuals = ClipVisuals::default();
    known_slot(&mut visuals, None, 1);
    visuals.slot("a.mp4");
    assert!(wanted(&visuals));
    visuals.wanted_frames.clear();
    visuals.wanted_keys.clear();
    visuals.slot("a.mp4");
    assert!(wanted(&visuals), "no answer came, so the next frame asks again");
    visuals.wanted_frames.clear();
    visuals.wanted_keys.clear();
    visuals.apply(Done::Stamp { slot: 0, stamp: None });
    visuals.slot("a.mp4");
    assert!(!wanted(&visuals), "an answer waits for the next interval");
}

#[test]
fn level_data_drawn_last_frame_is_not_evicted() {
    let entry = |used| PeakEntry { data: { let mut data = PeakData::default(); data.append(&[0.1; 3]); data }, finished: true, used };
    let mut visuals = ClipVisuals::default();
    visuals.clock = 10;
    visuals.peaks.insert(1, entry(9));
    visuals.peaks.insert(2, entry(5));
    visuals.peak_bytes_limit = 16;
    visuals.enforce_peak_limit();
    assert!(visuals.peaks.contains_key(&1) && !visuals.peaks.contains_key(&2), "the older one goes");
    visuals.peak_bytes_limit = 0;
    visuals.enforce_peak_limit();
    assert!(visuals.peaks.contains_key(&1), "what is on screen stays, over the limit if need be");
}

#[test]
fn nothing_new_is_generated_while_paused_even_from_a_pool_started_then() {
    let ctx = egui::Context::default();
    let mut visuals = ClipVisuals::default();
    visuals.set_paused(true);
    let slot = visuals.slot("a-file-that-does-not-exist.mp4");
    visuals.view(slot);
    visuals.end_frame();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(visuals.pool.as_ref().expect("started").outstanding(), 1, "the inspection waits");
    visuals.set_paused(false);
    run_frames(&mut visuals, &ctx, |v| { v.view(slot); }, |v| v.slots[slot].view == SourceView::Failed);
}

#[test]
fn levels_read_before_an_error_are_handed_over() {
    let mut calls = 0;
    let mut got = Vec::new();
    let result = super::backend::read_peaks(
        &mut |_, block| {
            calls += 1;
            block.fill(0.5);
            Some(if calls <= 2 { Ok(()) } else { Err("damaged".to_owned()) })
        },
        &mut |peaks| {
            got.extend_from_slice(peaks);
            true
        },
    );
    assert_eq!(result, Err("damaged".to_owned()));
    assert_eq!(got, vec![0.5, 0.5]);
}

#[test]
fn levels_of_a_clip_removed_from_the_project_are_dropped() {
    let mut visuals = ClipVisuals::default();
    visuals.slots.push(Slot { path: Path::new("a.mp4").into(), stamp: None, identity: Some(1), view: SourceView::Pending, checked: None, stamp_lost: false });
    visuals.live_identities.insert(1);
    visuals.apply(Done::Peaks { identity: 1, run: 0, peaks: vec![0.1], finished: false });
    let project = crate::editor::EditorState::default().project().clone();
    visuals.sync_project(1, &project);
    assert!(visuals.peaks.is_empty(), "the project does not use it");
}

/// Blocks the read of levels before it says anything, until told to go on.
#[derive(Default)]
struct SilentUntilReleased {
    started: std::sync::atomic::AtomicBool,
    release: std::sync::atomic::AtomicBool,
}

impl Generator for SilentUntilReleased {
    type Local = ();

    fn stamp(&self, _: &Path) -> Option<FileStamp> {
        None
    }

    fn inspect(&self, _: &Path) -> Result<SourceFacts, String> {
        Err("unused".to_owned())
    }

    fn frame(&self, _: &mut (), _: u32, _: &Path, _: Time, _: &dyn Fn() -> bool) -> Result<ThumbnailImage, ThumbnailError> {
        Err(ThumbnailError::Failed("unused".to_owned()))
    }

    fn peaks(&self, _: &Path, emit: &mut dyn FnMut(&[f32]) -> bool) -> Result<(), String> {
        self.started.store(true, std::sync::atomic::Ordering::SeqCst);
        while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        emit(&[0.5]);
        Ok(())
    }
}

#[test]
fn a_run_of_a_removed_source_ends_even_before_it_has_said_anything() {
    use std::sync::atomic::Ordering;
    let ctx = egui::Context::default();
    let generator = Arc::new(SilentUntilReleased::default());
    let mut visuals = ClipVisuals::with_generator(generator.clone());
    known_slot(&mut visuals, None, 1);
    visuals.peaks(0);
    visuals.end_frame();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !generator.started.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the read never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(visuals.peaks.is_empty(), "no entry exists yet");
    let project = crate::editor::EditorState::default().project().clone();
    visuals.sync_project(1, &project);
    generator.release.store(true, Ordering::SeqCst);
    run_frames(&mut visuals, &ctx, |_| {}, |v| v.pool.as_ref().expect("started").outstanding() == 0);
    visuals.begin_frame(&ctx);
    assert!(visuals.peaks.is_empty(), "what the cancelled run still said is ignored");
}

#[test]
fn the_same_file_spelled_differently_is_one_source() {
    if !cfg!(windows) {
        return;
    }
    let mut visuals = ClipVisuals::default();
    assert_eq!(visuals.slot("C:/v/A.mp4"), visuals.slot("c:\\V\\a.MP4"));
}

#[test]
fn a_library_tile_waits_for_the_source_to_be_known() {
    let mut visuals = ClipVisuals::default();
    assert_eq!(visuals.poster("a.mp4", None), None);
    assert!(visuals.wanted_frames.iter().any(|job| matches!(job.key, JobKey::Inspect(_))));
    let _: Time = Time::ZERO;
    visuals.end_frame();
}
