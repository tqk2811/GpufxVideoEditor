//! The preview under a renderer slower than the clock, on the real worker thread. No GPU: the
//! fake producer moves a virtual clock by the cost of each frame, so the test does not depend on
//! how fast the machine is.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpufx_editor::preview::plan::{is_current, PlaybackFrames, PlaybackRun, RequestId, RequestTracker};
use gpufx_editor::preview::worker::{FrameProducer, PreviewWorker, ProduceError};

const REVISION: u64 = 1;
const RUN: Option<PlaybackRun> = Some(PlaybackRun { started: 1, revision: REVISION });
/// Output frames that pass on the clock while one frame is rendered.
const COST: i64 = 5;
/// Where the playback of the test ends.
const END: i64 = 300;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Renders one frame in `COST` frames of clock.
struct SlowRenderer {
    clock: Arc<AtomicI64>,
    rendered: Arc<Mutex<Vec<i64>>>,
}

impl FrameProducer for SlowRenderer {
    type Request = i64;
    type Frame = i64;

    fn request_id(request: &i64) -> RequestId {
        RequestId { revision: REVISION, frame: *request }
    }

    fn produce(&mut self, request: &i64) -> Result<Option<i64>, ProduceError> {
        self.rendered.lock().unwrap().push(*request);
        self.clock.fetch_add(COST, Ordering::SeqCst);
        Ok(Some(*request))
    }

    fn recycle(&mut self, _frame: i64) {}
}

/// A worker on the fake renderer and the receiver of its wake-ups.
fn start() -> (PreviewWorker<SlowRenderer>, Receiver<()>, Arc<AtomicI64>, Arc<Mutex<Vec<i64>>>) {
    let clock = Arc::new(AtomicI64::new(0));
    let rendered: Arc<Mutex<Vec<i64>>> = Arc::default();
    let (woken, wake) = channel();
    let (thread_clock, thread_rendered) = (clock.clone(), rendered.clone());
    let worker = PreviewWorker::spawn(move || SlowRenderer { clock: thread_clock, rendered: thread_rendered }, move || {
        let _ = woken.send(());
    });
    (worker, wake, clock, rendered)
}

/// What reached the screen and what was rendered while the clock runs ahead of the renderer.
fn play_behind_the_clock() -> (Vec<i64>, Vec<i64>) {
    let (worker, wake, clock, rendered) = start();
    let (mut frames, mut tracker, mut shown) = (PlaybackFrames::default(), RequestTracker::default(), None);
    let mut on_screen = Vec::new();
    let started = Instant::now();
    while on_screen.last().copied().unwrap_or(-1) + COST < END {
        assert!(started.elapsed() < TIMEOUT, "the picture stopped at {on_screen:?}");
        let frame = frames.pick(RUN, clock.load(Ordering::SeqCst));
        if tracker.should_send(REVISION, frame) {
            worker.request(frame);
        }
        let _ = wake.recv_timeout(Duration::from_millis(1));
        if let Some(outcome) = worker.poll() {
            if is_current(tracker.wanted(), shown, outcome.id) {
                shown = Some(outcome.id);
                on_screen.push(outcome.id.frame);
            }
        }
    }
    drop(worker);
    let rendered = rendered.lock().unwrap().clone();
    (on_screen, rendered)
}

/// Follows the frames a clock says are due, one look at a time, each finished before the next.
fn follow(dues: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let (worker, wake, _clock, rendered) = start();
    let (mut frames, mut tracker, mut shown) = (PlaybackFrames::default(), RequestTracker::default(), None);
    let mut on_screen = Vec::new();
    for due in dues {
        let frame = frames.pick(RUN, *due);
        if !tracker.should_send(REVISION, frame) {
            continue;
        }
        worker.request(frame);
        wake.recv_timeout(TIMEOUT).expect("the worker finished the frame");
        let outcome = worker.poll().expect("a result after the wake");
        if is_current(tracker.wanted(), shown, outcome.id) {
            shown = Some(outcome.id);
            on_screen.push(outcome.id.frame);
        }
    }
    drop(worker);
    let rendered = rendered.lock().unwrap().clone();
    (on_screen, rendered)
}

fn assert_only_forward(frames: &[i64], what: &str) {
    assert!(frames.windows(2).all(|pair| pair[0] < pair[1]), "{what} went back or repeated: {frames:?}");
}

#[test]
fn a_slow_renderer_skips_the_frames_it_has_no_time_for() {
    let (on_screen, rendered) = play_behind_the_clock();
    assert_only_forward(&rendered, "the rendered frames");
    assert_only_forward(&on_screen, "the picture");
    assert!(rendered.len() as i64 <= END / COST + 2, "late frames were rendered: {rendered:?}");
    assert!(on_screen.len() as i64 >= END / COST - 2, "the picture must keep moving: {on_screen:?}");
}

#[test]
fn a_clock_that_steps_back_renders_nothing_twice_and_shows_nothing_older() {
    // The device delay the clock subtracts grows a little after each new frame.
    let (on_screen, rendered) = follow(&[0, 1, 0, 2, 1, 3, 2, 4]);
    assert_only_forward(&rendered, "the rendered frames");
    assert_only_forward(&on_screen, "the picture");
    assert_eq!(on_screen, [0, 1, 2, 3, 4]);
}
