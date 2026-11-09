//! The preview thread: runs the newest request, publishes the newest result, wakes the UI.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use super::plan::RequestId;
use super::slot::LatestSlot;

/// What the thread runs. Created on the thread, so it need not be `Send`.
pub trait FrameProducer {
    type Request: Send + 'static;
    type Frame: Send + 'static;

    /// Which frame `request` asks for.
    fn request_id(request: &Self::Request) -> RequestId;

    /// The frame for `request`; `None` shows black.
    fn produce(&mut self, request: &Self::Request) -> Result<Option<Self::Frame>, ProduceError>;

    /// Takes back a frame the UI no longer shows.
    fn recycle(&mut self, frame: Self::Frame);

    /// Called after a panic, to drop whatever the panic may have left half done.
    fn reset(&mut self) {}

    /// Drops what `produce` built (decoders, images), for [`PreviewWorker::suspend`]. It is built
    /// again by the next request.
    fn suspend(&mut self) {
        self.reset();
    }

    /// Receives, once, the flag that is set when the request in progress is no longer wanted: a
    /// newer one waits or the app is closing. A long `produce` should watch it.
    fn set_abort(&mut self, _abort: Arc<AtomicBool>) {}

    /// Receives, once, the flag that is set when the app is closing: unlike the abort flag it is not
    /// set by a newer request, so `produce` can stop at once for it and weigh the other.
    fn set_closing(&mut self, _closing: Arc<AtomicBool>) {}
}

/// Why `produce` returned no frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProduceError {
    Failed(String),
    /// Stopped because the frame is no longer wanted. Not an error, and nothing is reported.
    Cancelled,
}

/// A finished request: which one it was and what it produced.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome<F> {
    pub id: RequestId,
    pub result: Result<Option<F>, String>,
}

/// What the thread is asked to do. A suspension replaces a frame that has not started.
enum Job<R> {
    Frame(R),
    /// Drop what the producer holds, then say so.
    Suspend(Sender<()>),
}

pub struct PreviewWorker<P: FrameProducer> {
    requests: Arc<LatestSlot<Job<P::Request>>>,
    /// While set, requests are dropped: the producer must stay empty until [`Self::resume`].
    suspended: AtomicBool,
    results: Arc<LatestSlot<Outcome<P::Frame>>>,
    returned: Sender<P::Frame>,
    handle: Option<JoinHandle<()>>,
    spawn_error: Option<String>,
}

/// Recycles `frame`; a panic in there must not end the thread.
fn recycle_guarded<P: FrameProducer>(producer: &mut P, frame: P::Frame) {
    if catch_unwind(AssertUnwindSafe(|| producer.recycle(frame))).is_err() {
        producer.reset();
    }
}

/// The thread handle, or the message for a thread that could not start.
fn spawned(result: std::io::Result<JoinHandle<()>>) -> (Option<JoinHandle<()>>, Option<String>) {
    match result {
        Ok(handle) => (Some(handle), None),
        Err(e) => (None, Some(format!("the preview thread could not start: {e}"))),
    }
}

impl<P: FrameProducer + 'static> PreviewWorker<P> {
    /// Starts the thread. `make` runs on it; `wake` is called after each finished request.
    pub fn spawn(make: impl FnOnce() -> P + Send + 'static, wake: impl Fn() + Send + 'static) -> Self {
        let requests: Arc<LatestSlot<Job<P::Request>>> = Arc::default();
        let results: Arc<LatestSlot<Outcome<P::Frame>>> = Arc::default();
        let (returned, returned_frames) = channel::<P::Frame>();
        let (thread_requests, thread_results) = (requests.clone(), results.clone());
        let abort = requests.pending();
        let closing = requests.closing();
        let handle = thread::Builder::new().name("preview".to_owned()).spawn(move || {
            let mut producer = make();
            producer.set_abort(abort);
            producer.set_closing(closing);
            while let Some(job) = thread_requests.wait() {
                for frame in returned_frames.try_iter() {
                    recycle_guarded(&mut producer, frame);
                }
                let request = match job {
                    Job::Frame(request) => request,
                    Job::Suspend(done) => {
                        if catch_unwind(AssertUnwindSafe(|| producer.suspend())).is_err() {
                            producer.reset();
                        }
                        let _ = done.send(());
                        continue;
                    }
                };
                let result = match catch_unwind(AssertUnwindSafe(|| producer.produce(&request))) {
                    Ok(Ok(frame)) => Ok(frame),
                    Ok(Err(ProduceError::Failed(message))) => Err(message),
                    Ok(Err(ProduceError::Cancelled)) => continue,
                    Err(_) => {
                        producer.reset();
                        Err("the preview thread hit an internal error on this frame".to_owned())
                    }
                };
                let outcome = Outcome { id: P::request_id(&request), result };
                if let Some(Outcome { result: Ok(Some(unread)), .. }) = thread_results.put(outcome) {
                    recycle_guarded(&mut producer, unread);
                }
                wake();
            }
        });
        let (handle, spawn_error) = spawned(handle);
        Self { requests, suspended: AtomicBool::new(false), results, returned, handle, spawn_error }
    }

    /// Asks for `request` instead of whatever has not started yet. Never blocks on the work.
    pub fn request(&self, request: P::Request) {
        if !self.suspended.load(Ordering::SeqCst) {
            self.requests.put(Job::Frame(request));
        }
    }

    /// Stops the preview so something else can use the GPU and decoders. Unstarted work is dropped;
    /// the frame in progress is told it is no longer wanted. The returned receiver fires once the
    /// producer has dropped its decoders and images. Requests are ignored until [`Self::resume`].
    pub fn suspend(&self) -> Receiver<()> {
        let (done, released) = channel();
        self.suspended.store(true, Ordering::SeqCst);
        if self.handle.is_some() {
            self.requests.put(Job::Suspend(done));
        }
        released
    }

    /// Lets requests through again; the producer rebuilds what it dropped on the next one.
    pub fn resume(&self) {
        self.suspended.store(false, Ordering::SeqCst);
    }

    /// The newest finished result, if one arrived since the last call.
    pub fn poll(&self) -> Option<Outcome<P::Frame>> {
        self.results.take()
    }

    /// Why the thread could not start, if it could not. No request will ever finish then.
    pub fn spawn_error(&self) -> Option<&str> {
        self.spawn_error.as_deref()
    }

    /// Hands back a frame the UI has stopped showing.
    pub fn give_back(&self, frame: P::Frame) {
        // The receiver only goes away with the thread, and then the frame is simply dropped.
        let _ = self.returned.send(frame);
    }
}

impl<P: FrameProducer> Drop for PreviewWorker<P> {
    fn drop(&mut self) {
        self.requests.close();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(10);
    /// How long the fake's long frame runs when nothing stops it.
    const LONG_FRAME: Duration = Duration::from_secs(5);
    /// The request of the fake's long frame.
    const LONG: i32 = -3;
    /// A frame the fake's recycle panics on.
    const PANICS_IN_RECYCLE: i32 = -4;

    #[derive(Default)]
    struct Log {
        produced: Mutex<Vec<i32>>,
        recycled: AtomicUsize,
        suspended: AtomicUsize,
        /// Frames produced after the last suspension.
        produced_after_suspend: AtomicUsize,
        hold_first: AtomicBool,
        long_started: (Mutex<bool>, Condvar),
        first_started: (Mutex<bool>, Condvar),
        release_first: (Mutex<bool>, Condvar),
    }

    fn set(flag: &(Mutex<bool>, Condvar)) {
        *flag.0.lock().unwrap() = true;
        flag.1.notify_all();
    }

    fn wait_for(flag: &(Mutex<bool>, Condvar)) {
        let guard = flag.0.lock().unwrap();
        let (guard, timed_out) = flag.1.wait_timeout_while(guard, TIMEOUT, |set| !*set).unwrap();
        assert!(*guard && !timed_out.timed_out(), "timed out");
    }

    struct Fake(Arc<Log>, Option<Arc<AtomicBool>>);

    impl FrameProducer for Fake {
        type Request = i32;
        type Frame = i32;

        fn request_id(request: &i32) -> RequestId {
            RequestId { revision: 7, frame: i64::from(*request) }
        }

        fn produce(&mut self, request: &i32) -> Result<Option<i32>, ProduceError> {
            if *request == LONG {
                set(&self.0.long_started);
                let deadline = Instant::now() + LONG_FRAME;
                while Instant::now() < deadline {
                    if self.1.as_ref().is_some_and(|abort| abort.load(Ordering::SeqCst)) {
                        return Err(ProduceError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                self.0.produced.lock().unwrap().push(LONG);
                return Ok(Some(LONG));
            }
            if self.0.hold_first.swap(false, Ordering::SeqCst) {
                set(&self.0.first_started);
                wait_for(&self.0.release_first);
            }
            self.0.produced.lock().unwrap().push(*request);
            self.0.produced_after_suspend.fetch_add(1, Ordering::SeqCst);
            match request {
                -1 => Err(ProduceError::Failed("broken frame".to_owned())),
                -2 => panic!("deliberate test panic"),
                0 => Ok(None),
                value => Ok(Some(*value)),
            }
        }

        fn recycle(&mut self, frame: i32) {
            assert_ne!(frame, PANICS_IN_RECYCLE, "deliberate test panic");
            self.0.recycled.fetch_add(1, Ordering::SeqCst);
        }

        fn set_abort(&mut self, abort: Arc<AtomicBool>) {
            self.1 = Some(abort);
        }

        fn suspend(&mut self) {
            self.0.suspended.fetch_add(1, Ordering::SeqCst);
            self.0.produced_after_suspend.store(0, Ordering::SeqCst);
        }
    }

    fn start(log: &Arc<Log>) -> (PreviewWorker<Fake>, std::sync::mpsc::Receiver<()>) {
        let (woken, wake_rx) = channel();
        let producer_log = log.clone();
        let worker = PreviewWorker::spawn(move || Fake(producer_log, None), move || {
            let _ = woken.send(());
        });
        (worker, wake_rx)
    }

    fn next(worker: &PreviewWorker<Fake>, wake: &std::sync::mpsc::Receiver<()>) -> Result<Option<i32>, String> {
        wake.recv_timeout(TIMEOUT).expect("the worker finished a request");
        worker.poll().expect("a result waits after a wake").result
    }

    #[test]
    fn suspend_drops_the_producer_state_after_the_frame_in_progress() {
        let log = Arc::new(Log::default());
        log.hold_first.store(true, Ordering::SeqCst);
        let (worker, wake) = start(&log);
        worker.request(1);
        wait_for(&log.first_started);
        let released = worker.suspend();
        assert!(released.try_recv().is_err(), "not before the frame finished");
        assert_eq!(log.suspended.load(Ordering::SeqCst), 0);
        set(&log.release_first);
        assert_eq!(next(&worker, &wake), Ok(Some(1)));
        released.recv_timeout(TIMEOUT).expect("released after the frame");
        assert_eq!(log.suspended.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn suspend_replaces_a_frame_that_has_not_started_and_ignores_requests_until_resume() {
        let log = Arc::new(Log::default());
        log.hold_first.store(true, Ordering::SeqCst);
        let (worker, wake) = start(&log);
        worker.request(1);
        wait_for(&log.first_started);
        worker.request(2);
        let released = worker.suspend();
        set(&log.release_first);
        released.recv_timeout(TIMEOUT).expect("released");
        worker.request(3);
        assert_eq!(*log.produced.lock().unwrap(), vec![1], "2 was replaced");
        let _ = wake.try_iter().count();
        assert_eq!(worker.poll().map(|outcome| outcome.result), Some(Ok(Some(1))));
        assert_eq!(log.produced_after_suspend.load(Ordering::SeqCst), 0, "3 did not run while suspended");

        worker.resume();
        worker.request(4);
        assert_eq!(next(&worker, &wake), Ok(Some(4)));
        assert_eq!(log.produced_after_suspend.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_stale_request_is_dropped_while_the_worker_is_busy() {
        let log = Arc::new(Log::default());
        log.hold_first.store(true, Ordering::SeqCst);
        let (worker, wake) = start(&log);
        worker.request(1);
        wait_for(&log.first_started);
        worker.request(2);
        worker.request(3);
        set(&log.release_first);
        // Result 3 may replace an unread result 1, so wait for the newest rather than for each.
        loop {
            wake.recv_timeout(TIMEOUT).expect("the worker finished a request");
            if worker.poll().map(|outcome| outcome.result) == Some(Ok(Some(3))) {
                break;
            }
        }
        assert_eq!(*log.produced.lock().unwrap(), vec![1, 3], "2 was never decoded");
    }

    #[test]
    fn requesting_does_not_wait_for_the_work() {
        let log = Arc::new(Log::default());
        log.hold_first.store(true, Ordering::SeqCst);
        let (worker, _wake) = start(&log);
        worker.request(1);
        wait_for(&log.first_started);
        // The worker is held inside produce, and these still return.
        worker.request(2);
        assert_eq!(worker.poll(), None);
        set(&log.release_first);
    }

    #[test]
    fn a_panic_in_recycle_does_not_end_the_thread() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.give_back(PANICS_IN_RECYCLE);
        worker.give_back(8);
        worker.request(5);
        assert_eq!(next(&worker, &wake), Ok(Some(5)));
        assert_eq!(log.recycled.load(Ordering::SeqCst), 1, "the frame after the panic was recycled");
    }

    #[test]
    fn a_thread_that_cannot_start_is_reported() {
        let (handle, message) = spawned(Err(std::io::Error::other("out of threads")));
        assert!(handle.is_none());
        assert!(message.expect("a message").contains("out of threads"));
    }

    #[test]
    fn a_started_thread_reports_nothing() {
        let log = Arc::new(Log::default());
        let (worker, _wake) = start(&log);
        assert_eq!(worker.spawn_error(), None);
    }

    #[test]
    fn a_result_names_its_request() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.request(4);
        wake.recv_timeout(TIMEOUT).expect("the worker finished a request");
        let outcome = worker.poll().expect("a result waits after a wake");
        assert_eq!(outcome.id, RequestId { revision: 7, frame: 4 });
    }

    #[test]
    fn a_failure_names_its_request() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.request(-1);
        wake.recv_timeout(TIMEOUT).expect("the worker finished a request");
        let outcome = worker.poll().expect("a result waits after a wake");
        assert_eq!(outcome.id, RequestId { revision: 7, frame: -1 });
        assert!(outcome.result.is_err());
    }

    #[test]
    fn closing_during_a_long_frame_returns_promptly() {
        let log = Arc::new(Log::default());
        let (worker, _wake) = start(&log);
        worker.request(LONG);
        wait_for(&log.long_started);
        let closing = Instant::now();
        drop(worker);
        assert!(closing.elapsed() < LONG_FRAME / 2, "close waited {:?}", closing.elapsed());
    }

    #[test]
    fn a_newer_request_cancels_a_long_frame_silently_and_runs_next() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.request(LONG);
        wait_for(&log.long_started);
        worker.request(4);
        // The first thing the UI hears of is the newer frame: no outcome for the cancelled one.
        assert_eq!(next(&worker, &wake), Ok(Some(4)));
        assert_eq!(*log.produced.lock().unwrap(), vec![4]);
        assert_eq!(worker.poll(), None);
    }

    #[test]
    fn a_failed_frame_does_not_stop_later_requests() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.request(-1);
        assert_eq!(next(&worker, &wake), Err("broken frame".to_owned()));
        worker.request(4);
        assert_eq!(next(&worker, &wake), Ok(Some(4)));
    }

    #[test]
    fn a_panicking_frame_is_reported_and_the_thread_lives_on() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.request(-2);
        assert!(next(&worker, &wake).is_err());
        worker.request(5);
        assert_eq!(next(&worker, &wake), Ok(Some(5)));
    }

    #[test]
    fn an_empty_frame_is_reported_as_none() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.request(0);
        assert_eq!(next(&worker, &wake), Ok(None));
    }

    #[test]
    fn given_back_frames_are_recycled_before_the_next_request() {
        let log = Arc::new(Log::default());
        let (worker, wake) = start(&log);
        worker.give_back(9);
        worker.request(1);
        next(&worker, &wake).unwrap();
        assert_eq!(log.recycled.load(Ordering::SeqCst), 1);
    }
}
