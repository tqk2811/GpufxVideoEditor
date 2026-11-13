//! The preparing thread and the state it shares with the device callback.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use engine::render::{ClipKey, RenderError};
use engine::{audio_channel, sample_position, AudioClock, AudioConsumer, AudioProducer, AudioSource, Project, Time, MIX_CHANNELS};

/// Shared by every audio thread, so one reopened in the middle of playing starts a different run.
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

use super::plan::{expand_stereo, frames_to_prepare, gate_may_open, output_delay_frames, timed_frames, ClockReading, BLOCK_FRAMES, LEAD_FRAMES, RING_BLOCKS};

/// How long the thread sleeps when the lead is full. A seek or edit wakes it at once.
const IDLE_WAIT: Duration = Duration::from_millis(10);
/// Mixed in one go, so a command is looked at between steps of a long refill.
const STEP_FRAMES: usize = 4 * BLOCK_FRAMES;
/// How often the silent driver pulls when there is no device.
const SILENT_TICK: Duration = Duration::from_millis(10);
/// The most one silent pull takes, 100 ms: a stalled timer must not jump the clock.
const SILENT_MAX_FRAMES: usize = 4_800;

/// What the callback writes and everyone else reads. All wait-free.
#[derive(Default)]
pub struct PlaybackShared {
    gate: AtomicBool,
    played: AtomicU64,
    underrun: AtomicU64,
    /// The underrun that was not the expected wait after a play, seek or edit.
    gaps: AtomicU64,
    /// Set by a seek or edit until real audio comes again.
    settling: AtomicBool,
    past_end: AtomicU64,
    delay: AtomicU64,
    anchor: AtomicI64,
    /// The latest start or seek: each is a new run of the clock.
    runs: AtomicU64,
}

/// Running totals of the callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaybackStats {
    pub played: u64,
    /// Frames of silence because the audio was not prepared in time.
    pub underrun: u64,
    /// Frames of [`Self::underrun`] that were not the expected wait after a seek or edit, since the
    /// last play.
    pub gaps: u64,
    pub delay: u64,
}

impl PlaybackShared {
    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            played: self.played.load(Ordering::Relaxed),
            underrun: self.underrun.load(Ordering::Relaxed),
            gaps: self.gaps.load(Ordering::Relaxed),
            delay: self.delay.load(Ordering::Relaxed),
        }
    }

    fn set_gate(&self, open: bool) {
        self.gate.store(open, Ordering::Release);
    }

    /// Call after the seek is published: an underrun before the next real audio is expected.
    fn expect_silence(&self) {
        self.settling.store(true, Ordering::Relaxed);
    }

    fn begin(&self, anchor: i64) {
        self.past_end.store(0, Ordering::Relaxed);
        self.anchor.store(anchor, Ordering::Relaxed);
        self.runs.store(NEXT_RUN.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
    }

    /// Changes with every play and seek, even one to the place the last one went to.
    pub fn run(&self) -> u64 {
        self.runs.load(Ordering::Relaxed)
    }
}

/// The whole device callback: no lock, no allocation. Silence while the gate is closed, and the
/// consumer does not see those frames; it only drops stale blocks, or the ring could fill up with
/// them before the lead is ready and the gate would never open.
pub fn fill_device_buffer(consumer: &mut AudioConsumer, shared: &PlaybackShared, out: &mut [f32], latency: Option<Duration>) {
    if pull_stereo(consumer, shared, out) {
        shared.delay.store(output_delay_frames(out.len() / MIX_CHANNELS, latency), Ordering::Relaxed);
    }
}

/// Fills `out` and updates the totals. False when the gate was closed.
fn pull_stereo(consumer: &mut AudioConsumer, shared: &PlaybackShared, out: &mut [f32]) -> bool {
    if !shared.gate.load(Ordering::Acquire) {
        out.fill(0.0);
        consumer.discard_stale();
        return false;
    }
    let report = consumer.pull(out);
    shared.played.fetch_add(report.played as u64, Ordering::Relaxed);
    shared.underrun.fetch_add(report.underrun as u64, Ordering::Relaxed);
    let expected = if report.played > 0 { shared.settling.swap(false, Ordering::Relaxed) } else { shared.settling.load(Ordering::Relaxed) };
    if !expected {
        shared.gaps.fetch_add(report.underrun as u64, Ordering::Relaxed);
    }
    if report.past_end > 0 {
        shared.past_end.fetch_add(report.past_end as u64, Ordering::Relaxed);
    } else {
        shared.past_end.store(0, Ordering::Relaxed);
    }
    true
}

/// The device callback for a device of `channels`, 2 or more. `scratch` is allocated before the
/// stream starts, and a callback larger than it is pulled in pieces. No lock, no allocation.
pub fn fill_device_channels(consumer: &mut AudioConsumer, shared: &PlaybackShared, out: &mut [f32], channels: usize, scratch: &mut [f32], latency: Option<Duration>) {
    if channels == MIX_CHANNELS {
        fill_device_buffer(consumer, shared, out, latency);
        return;
    }
    let piece_frames = scratch.len() / MIX_CHANNELS;
    if channels < MIX_CHANNELS || piece_frames == 0 {
        out.fill(0.0);
        return;
    }
    let mut played = false;
    for piece in out.chunks_mut(piece_frames * channels) {
        let stereo = &mut scratch[..piece.len() / channels * MIX_CHANNELS];
        played = pull_stereo(consumer, shared, stereo);
        expand_stereo(stereo, piece, channels);
    }
    if played {
        shared.delay.store(output_delay_frames(out.len() / channels, latency), Ordering::Relaxed);
    }
}

/// Stands in for the device when there is none, so the clock and the picture still advance: pulls
/// the consumer on a timer, at the rate of the mix, and throws the samples away.
pub struct SilentDriver {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl SilentDriver {
    pub fn spawn(mut consumer: AudioConsumer, shared: Arc<PlaybackShared>) -> Result<Self, String> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_stop = stop.clone();
        let handle = thread::Builder::new()
            .name("audio-silent".to_owned())
            .spawn(move || {
                let (lock, wake) = &*thread_stop;
                let mut out = vec![0.0f32; SILENT_MAX_FRAMES * MIX_CHANNELS];
                let (mut last, mut carry) = (Instant::now(), 0);
                let mut stopped = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                while !*stopped {
                    stopped = wake.wait_timeout(stopped, SILENT_TICK).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
                    let now = Instant::now();
                    let (frames, left) = timed_frames(now - last, carry, SILENT_MAX_FRAMES);
                    (last, carry) = (now, left);
                    fill_device_buffer(&mut consumer, &shared, &mut out[..frames * MIX_CHANNELS], Some(Duration::ZERO));
                }
            })
            .map_err(|e| format!("the silent playback thread could not start: {e}"))?;
        Ok(Self { stop, handle: Some(handle) })
    }
}

impl Drop for SilentDriver {
    fn drop(&mut self) {
        let (lock, wake) = &*self.stop;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        wake.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Default)]
struct Commands {
    playing: bool,
    quit: bool,
    project: Option<Arc<Project>>,
    error: Option<String>,
    /// Set when the source could not be made, so the thread is gone; every play reports it again.
    startup_failure: Option<String>,
}

#[derive(Default)]
struct Control {
    commands: Mutex<Commands>,
    wake: Condvar,
}

impl Control {
    fn lock(&self) -> MutexGuard<'_, Commands> {
        self.commands.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A source for projects that make no sound: every frame stays silent.
struct NoAudio;

impl AudioSource for NoAudio {
    fn read(&mut self, _key: ClipKey, _source: &str, _position: i64, _out: &mut [f32]) -> Result<(), RenderError> {
        Ok(())
    }
}

fn sound_is_off(project: &Project) -> bool {
    project.output.audio.is_none()
}

enum Step {
    Worked,
    Idle,
    Failed(String),
}

fn step<S: AudioSource + ?Sized>(producer: &mut AudioProducer, source: &mut S, silent: bool) -> Step {
    let want = frames_to_prepare(producer.prepared_frames(), LEAD_FRAMES, BLOCK_FRAMES).min(STEP_FRAMES);
    if want == 0 {
        return Step::Idle;
    }
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        if silent {
            producer.prepare(&mut NoAudio, want)
        } else {
            producer.prepare(source, want)
        }
    }));
    match attempt {
        Ok(Ok(report)) if report.frames > 0 => Step::Worked,
        Ok(Ok(_)) => Step::Idle,
        Ok(Err(error)) => Step::Failed(error.0),
        Err(_) => Step::Failed("the audio thread hit an internal error".to_owned()),
    }
}

fn run<S: AudioSource + ?Sized>(mut producer: AudioProducer, source: &mut S, clock: &AudioClock, shared: &PlaybackShared, control: &Control, mut silent: bool) {
    let mut commands = control.lock();
    loop {
        if commands.quit {
            return;
        }
        if let Some(project) = commands.project.take() {
            silent = sound_is_off(&project);
            // The clone can be long; the UI must not wait for it to seek or stop.
            drop(commands);
            producer.set_project((*project).clone());
            shared.expect_silence();
            commands = control.lock();
            continue;
        }
        if !commands.playing {
            commands = control.wake.wait(commands).unwrap_or_else(|poisoned| poisoned.into_inner());
            continue;
        }
        drop(commands);
        let done = step(&mut producer, source, silent);
        commands = control.lock();
        // Decided under the lock: play and seek take it, so what is measured is what they left.
        if gate_may_open(commands.playing, commands.project.is_some(), producer.prepared_frames(), clock.end() - clock.position(), LEAD_FRAMES) {
            shared.set_gate(true);
        }
        match done {
            Step::Worked => {}
            Step::Idle => {
                let (guard, _) = control.wake.wait_timeout(commands, IDLE_WAIT).unwrap_or_else(|poisoned| poisoned.into_inner());
                commands = guard;
            }
            Step::Failed(message) => {
                shared.set_gate(false);
                commands.playing = false;
                commands.error = Some(message);
            }
        }
    }
}

/// Owns the preparing thread and the clock.
pub struct PreparingThread {
    control: Arc<Control>,
    shared: Arc<PlaybackShared>,
    clock: AudioClock,
    handle: Option<JoinHandle<()>>,
}

impl PreparingThread {
    /// Starts the thread on `project` and returns the consumer for the device. `make` runs on the
    /// thread, because sources need not be `Send`.
    pub fn spawn<S, M>(project: &Project, make: M) -> Result<(Self, AudioConsumer), String>
    where
        S: AudioSource,
        M: FnOnce() -> S + Send + 'static,
    {
        let (producer, consumer, clock) = audio_channel(project.clone(), BLOCK_FRAMES, RING_BLOCKS);
        let control = Arc::new(Control::default());
        let shared = Arc::new(PlaybackShared::default());
        let silent = sound_is_off(project);
        let (thread_control, thread_shared, thread_clock) = (control.clone(), shared.clone(), clock.clone());
        let handle = thread::Builder::new()
            .name("audio-prepare".to_owned())
            .spawn(move || {
                let Ok(mut source) = catch_unwind(AssertUnwindSafe(make)) else {
                    let message = "the audio source could not be created".to_owned();
                    let mut commands = thread_control.lock();
                    commands.error = Some(message.clone());
                    commands.startup_failure = Some(message);
                    return;
                };
                run(producer, &mut source, &thread_clock, &thread_shared, &thread_control, silent);
            })
            .map_err(|e| format!("the audio thread could not start: {e}"))?;
        Ok((Self { control, shared, clock, handle: Some(handle) }, consumer))
    }

    pub fn shared(&self) -> &Arc<PlaybackShared> {
        &self.shared
    }

    /// Prepares from `from`; the gate opens when the lead is ready.
    pub fn play(&self, from: Time) {
        let mut commands = self.control.lock();
        if let Some(message) = commands.startup_failure.clone() {
            commands.error = Some(message);
            return;
        }
        self.shared.begin(sample_position(from));
        self.shared.gaps.store(0, Ordering::Relaxed);
        self.clock.seek(from);
        self.shared.expect_silence();
        commands.error = None;
        commands.playing = true;
        self.control.wake.notify_all();
    }

    pub fn stop(&self) {
        let mut commands = self.control.lock();
        self.shared.set_gate(false);
        commands.playing = false;
        self.control.wake.notify_all();
    }

    pub fn seek(&self, time: Time) {
        // Held so the seek cannot land between the thread's gate decision and its wait.
        let _commands = self.control.lock();
        self.shared.begin(sample_position(time));
        self.clock.seek(time);
        self.shared.expect_silence();
        self.control.wake.notify_all();
    }

    /// Passes a changed project to the thread, which keeps running.
    pub fn set_project(&self, project: Arc<Project>) {
        self.control.lock().project = Some(project);
        self.control.wake.notify_all();
    }

    /// A callable for the device error callback.
    pub fn error_reporter(&self) -> impl Fn(String) + Send + 'static {
        let (control, shared) = (self.control.clone(), self.shared.clone());
        move |message| {
            let mut commands = control.lock();
            shared.set_gate(false);
            commands.playing = false;
            commands.error = Some(message);
            control.wake.notify_all();
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.control.lock().error.take()
    }

    /// The figures for the playhead.
    pub fn reading(&self) -> ClockReading {
        ClockReading {
            position: self.clock.position(),
            end: self.clock.end(),
            past_end: self.shared.past_end.load(Ordering::Relaxed),
            delay: self.shared.delay.load(Ordering::Relaxed),
            anchor: self.shared.anchor.load(Ordering::Relaxed),
        }
    }
}

impl Drop for PreparingThread {
    fn drop(&mut self) {
        self.control.lock().quit = true;
        self.control.wake.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{AudioOutput, Clip, Output, Track, TrackKind, SUPPORTED_VERSION};
    use std::sync::atomic::AtomicUsize;

    const TIMEOUT: Duration = Duration::from_secs(10);
    /// A device of ten millisecond buffers.
    const DEVICE_FRAMES: usize = 480;
    /// A clip of this source reads from `OTHER_SOURCE_BASE` up, so its samples are recognisable.
    const OTHER_SOURCE: &str = "b.wav";
    const OTHER_SOURCE_BASE: i64 = 2_000_000;

    /// Writes the source sample number into every channel so a pull shows where it came from.
    #[derive(Clone, Default)]
    struct Counting {
        reads: Arc<AtomicUsize>,
        /// The source position of the last read.
        latest: Arc<AtomicI64>,
        fail_at: Option<i64>,
        panic_at: Option<i64>,
    }

    impl AudioSource for Counting {
        fn read(&mut self, _key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.latest.store(position, Ordering::SeqCst);
            assert_ne!(self.panic_at, Some(position), "deliberate test panic");
            if self.fail_at == Some(position) {
                return Err(RenderError("broken source".to_owned()));
            }
            let base = if source == OTHER_SOURCE { OTHER_SOURCE_BASE } else { 0 };
            for (i, frame) in out.chunks_exact_mut(2).enumerate() {
                let value = (base + position + i as i64) as f32 / 1e7;
                frame[0] = value;
                frame[1] = value;
            }
            Ok(())
        }
    }

    fn project(seconds: i64, audio: bool, clip_track: TrackKind) -> Project {
        project_of("a.wav", seconds, audio, clip_track)
    }

    fn project_of(source: &str, seconds: i64, audio: bool, clip_track: TrackKind) -> Project {
        let duration = Time::from_seconds(seconds).unwrap();
        let clip = Clip { source: source.to_owned(), start: Time::ZERO, source_in: Time::ZERO, duration, effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None, animation: Default::default() };
        Project {
            version: SUPPORTED_VERSION,
            output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: audio.then_some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 192_000 }) },
            tracks: vec![Track { kind: clip_track, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false }],
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool, what: &str) {
        let start = Instant::now();
        while !condition() {
            assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn pull(consumer: &mut AudioConsumer, shared: &PlaybackShared, frames: usize) -> Vec<f32> {
        let mut out = vec![1.0f32; frames * 2];
        fill_device_buffer(consumer, shared, &mut out, Some(Duration::ZERO));
        out
    }

    fn open_gate(thread: &PreparingThread) {
        let shared = thread.shared().clone();
        wait_until(move || shared.gate.load(Ordering::Acquire), "the gate");
    }

    fn sample_of(value: f32) -> i64 {
        (value * 1e7).round() as i64
    }

    /// Reads only with a permit, so a test decides how far the thread gets. The wait ends by itself
    /// so a failed test can still join the thread.
    #[derive(Clone, Default)]
    struct Permits {
        granted: Arc<AtomicI64>,
        used: Arc<AtomicUsize>,
    }

    impl Permits {
        fn grant(&self, count: i64) {
            self.granted.fetch_add(count, Ordering::SeqCst);
        }

        fn unlimited(&self) {
            self.grant(i64::MAX / 2);
        }

        fn used(&self) -> usize {
            self.used.load(Ordering::SeqCst)
        }

        fn take(&self) -> bool {
            let left = self.granted.load(Ordering::SeqCst);
            left > 0 && self.granted.compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
        }

        fn wait_for(&self, used: usize) {
            wait_until(|| self.used() >= used, "the permitted reads");
        }
    }

    impl AudioSource for Permits {
        fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
            let start = Instant::now();
            while !self.take() && start.elapsed() < TIMEOUT {
                thread::sleep(Duration::from_millis(1));
            }
            self.used.fetch_add(1, Ordering::SeqCst);
            for (i, frame) in out.chunks_exact_mut(2).enumerate() {
                frame.fill((position + i as i64) as f32 / 1e7);
            }
            Ok(())
        }
    }

    #[test]
    fn the_silence_after_a_seek_is_not_a_gap_but_a_later_one_is() {
        let permits = Permits::default();
        let source = permits.clone();
        let (thread, mut consumer) = PreparingThread::spawn(&project(8, true, TrackKind::Video), move || source).unwrap();
        thread.play(Time::ZERO);
        permits.grant(10);
        open_gate(&thread);
        pull(&mut consumer, thread.shared(), 480);
        thread.seek(Time::from_seconds(1).unwrap());
        pull(&mut consumer, thread.shared(), 480);
        let stats = thread.shared().stats();
        assert!(stats.underrun > 0, "nothing is prepared at the target yet");
        assert_eq!(stats.gaps, 0, "the wait after a seek is expected");

        let used = permits.used();
        permits.grant(5);
        permits.wait_for(used + 5);
        wait_until(
            || {
                pull(&mut consumer, thread.shared(), 480);
                thread.shared().stats().played > 480
            },
            "the audio at the target",
        );
        wait_until(
            || {
                pull(&mut consumer, thread.shared(), 480);
                thread.shared().stats().gaps > 0
            },
            "a gap when the audio runs out",
        );

        thread.play(Time::ZERO);
        assert_eq!(thread.shared().stats().gaps, 0, "reset on each play");
        permits.unlimited();
    }

    #[test]
    fn a_panicking_source_constructor_is_reported_on_every_play() {
        let (thread, _consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), || -> Counting { panic!("deliberate test panic") }).unwrap();
        thread.play(Time::ZERO);
        wait_until(|| thread.control.lock().error.is_some(), "the error");
        assert!(thread.take_error().expect("an error").contains("could not be created"));
        thread.play(Time::ZERO);
        assert!(thread.take_error().expect("a second report").contains("could not be created"));
    }

    #[test]
    fn a_six_channel_callback_larger_than_the_scratch_is_filled_in_pieces() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        thread.play(Time::from_seconds(1).unwrap());
        open_gate(&thread);
        let mut scratch = vec![0.0f32; 64 * MIX_CHANNELS];
        let mut out = vec![1.0f32; 200 * 6];
        fill_device_channels(&mut consumer, thread.shared(), &mut out, 6, &mut scratch, Some(Duration::ZERO));
        for (i, frame) in out.chunks_exact(6).enumerate() {
            assert_eq!(sample_of(frame[0]), 48_000 + i as i64, "frame {i}");
            assert_eq!(sample_of(frame[1]), 48_000 + i as i64, "frame {i}");
            assert_eq!(&frame[2..], &[0.0; 4], "frame {i}");
        }
        assert_eq!(thread.shared().stats().played, 200);
        assert_eq!(thread.reading().delay, 200, "the whole callback, not the last piece");
    }

    #[test]
    fn a_stereo_callback_does_not_need_the_scratch() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        thread.play(Time::ZERO);
        open_gate(&thread);
        let mut out = vec![1.0f32; 100 * 2];
        fill_device_channels(&mut consumer, thread.shared(), &mut out, 2, &mut [], Some(Duration::ZERO));
        assert_eq!(sample_of(out[198]), 99);
    }

    #[test]
    fn a_six_channel_callback_is_silent_with_the_gate_closed() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        let mut scratch = vec![1.0f32; 64 * MIX_CHANNELS];
        let mut out = vec![1.0f32; 200 * 6];
        fill_device_channels(&mut consumer, thread.shared(), &mut out, 6, &mut scratch, Some(Duration::ZERO));
        assert!(out.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn the_silent_driver_moves_the_clock_and_finishes() {
        let (thread, consumer) = PreparingThread::spawn(&project(1, true, TrackKind::Video), Counting::default).unwrap();
        let driver = SilentDriver::spawn(consumer, thread.shared().clone()).unwrap();
        thread.play(Time::from_ticks(engine::TICKS_PER_SECOND - 30_000));
        wait_until(|| thread.reading().finished(), "the end of the timeline");
        assert_eq!(thread.reading().position, thread.reading().end);
        drop(driver);
    }

    #[test]
    fn the_silent_driver_keeps_the_gate_opening_after_seeks() {
        let permits = Permits::default();
        let source = permits.clone();
        let (thread, consumer) = PreparingThread::spawn(&project(8, true, TrackKind::Video), move || source).unwrap();
        let _driver = SilentDriver::spawn(consumer, thread.shared().clone()).unwrap();
        thread.play(Time::ZERO);
        for round in 0..RING_BLOCKS as i64 {
            let used = permits.used();
            permits.grant(4);
            permits.wait_for(used + 4);
            thread.seek(Time::from_seconds(1 + round % 5).unwrap());
        }
        permits.unlimited();
        open_gate(&thread);
    }

    #[test]
    fn a_new_generation_just_before_the_gate_decision_does_not_open_it() {
        let permits = Permits::default();
        let source = permits.clone();
        let (thread, _consumer) = PreparingThread::spawn(&project(4, true, TrackKind::Video), move || source).unwrap();
        thread.play(Time::ZERO);
        // The lead is one read short. Holding the lock lets the last read finish and leaves the
        // thread waiting for the lock with a whole lead of the old generation.
        permits.grant(9);
        permits.wait_for(9);
        let guard = thread.control.lock();
        permits.grant(1);
        permits.wait_for(10);
        thread::sleep(Duration::from_millis(50));
        // What stop and play do, without giving the lock away.
        thread.shared.set_gate(false);
        thread.clock.seek(Time::from_seconds(2).unwrap());
        drop(guard);
        thread::sleep(Duration::from_millis(100));
        let open = thread.shared().gate.load(Ordering::Acquire);
        permits.unlimited();
        assert!(!open, "the gate opened for a generation with no lead");
    }

    #[test]
    fn stale_blocks_of_several_seeks_do_not_keep_the_gate_closed() {
        let permits = Permits::default();
        let source = permits.clone();
        let (thread, mut consumer) = PreparingThread::spawn(&project(8, true, TrackKind::Video), move || source).unwrap();
        thread.play(Time::ZERO);
        // Four blocks per seek never make a lead, and together they are more than the ring holds.
        let mut target = Time::ZERO;
        for round in 0..RING_BLOCKS as i64 {
            let used = permits.used();
            permits.grant(4);
            permits.wait_for(used + 4);
            target = Time::from_seconds(1 + round % 5).unwrap();
            thread.seek(target);
            assert!(pull(&mut consumer, thread.shared(), 480).iter().all(|sample| *sample == 0.0));
        }
        permits.unlimited();
        let mut first = None;
        wait_until(
            || {
                first = pull(&mut consumer, thread.shared(), 480).first().copied().filter(|sample| *sample != 0.0);
                first.is_some()
            },
            "the gate after the seeks",
        );
        assert_eq!(sample_of(first.unwrap()), sample_position(target));
    }

    #[test]
    fn a_new_audio_thread_does_not_repeat_the_run_of_the_last_one() {
        let (first, _first_consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        first.play(Time::ZERO);
        let (second, _second_consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        second.play(Time::ZERO);
        assert_ne!(first.shared().run(), second.shared().run());
    }

    #[test]
    fn every_play_and_seek_is_a_new_run_even_to_the_same_place() {
        let (thread, _consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        let before = thread.shared().run();
        thread.play(Time::ZERO);
        let played = thread.shared().run();
        assert_ne!(played, before);
        thread.seek(Time::ZERO);
        assert_ne!(thread.shared().run(), played, "the same place again");
    }

    #[test]
    fn a_closed_gate_outputs_silence_and_does_not_move_the_clock() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        let out = pull(&mut consumer, thread.shared(), 480);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert_eq!(thread.reading().position, 0);
        assert_eq!(thread.shared().stats(), PlaybackStats::default());
    }

    #[test]
    fn the_gate_opens_only_after_the_lead_is_prepared() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        thread.play(Time::from_seconds(1).unwrap());
        open_gate(&thread);
        let out = pull(&mut consumer, thread.shared(), 480);
        assert_eq!(sample_of(out[0]), 48_000);
        assert_eq!(sample_of(out[958]), 48_000 + 479);
        let stats = thread.shared().stats();
        assert_eq!(stats.underrun, 0);
        assert_eq!(stats.played, 480);
        assert_eq!(thread.reading().delay, 480, "buffer plus a zero latency");
    }

    #[test]
    fn stopping_closes_the_gate_at_once_and_keeps_the_clock() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        thread.play(Time::ZERO);
        open_gate(&thread);
        pull(&mut consumer, thread.shared(), 480);
        thread.stop();
        let position = thread.reading().position;
        let out = pull(&mut consumer, thread.shared(), 480);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert_eq!(thread.reading().position, position);
    }

    #[test]
    fn a_seek_after_a_full_lead_plays_from_the_target_without_silence() {
        let source = Counting::default();
        let latest = source.latest.clone();
        let (thread, mut consumer) = PreparingThread::spawn(&project(4, true, TrackKind::Video), move || source).unwrap();
        thread.play(Time::ZERO);
        open_gate(&thread);
        pull(&mut consumer, thread.shared(), 480);
        let target = Time::from_seconds(2).unwrap();
        thread.seek(target);
        assert_eq!(thread.reading().anchor, sample_position(target));
        // Stale blocks are in the ring; the fresh lead must fit beside them. Blocks are mixed in
        // order, so once the tenth is being read the nine before it are in the ring.
        let tenth_block = sample_position(target) + 9 * BLOCK_FRAMES as i64;
        wait_until(|| latest.load(Ordering::SeqCst) >= tenth_block, "the lead at the target");
        let out = pull(&mut consumer, thread.shared(), 480);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            assert_eq!(sample_of(frame[0]), sample_position(target) + i as i64, "frame {i}");
        }
        assert_eq!(thread.shared().stats().underrun, 0, "a whole lead was ready after the seek");
    }

    #[test]
    fn a_changed_project_reaches_the_thread_and_it_stays_alive() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), Counting::default).unwrap();
        thread.play(Time::ZERO);
        open_gate(&thread);
        thread.set_project(Arc::new(project_of(OTHER_SOURCE, 1, true, TrackKind::Video)));
        wait_until(|| thread.reading().end == 48_000, "the new end");
        let mut seen_new = false;
        wait_until(
            || {
                for sample in pull(&mut consumer, thread.shared(), 480).into_iter().filter(|sample| *sample != 0.0) {
                    let is_new = sample_of(sample) >= OTHER_SOURCE_BASE;
                    assert!(is_new || !seen_new, "audio of the old project after the new one");
                    seen_new |= is_new;
                }
                seen_new
            },
            "audio of the new project",
        );
        thread.stop();
    }

    #[test]
    fn reaching_the_end_counts_past_the_end_and_finishes() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(1, true, TrackKind::Video), Counting::default).unwrap();
        thread.play(sample_time_near_end());
        open_gate(&thread);
        wait_until(
            || {
                pull(&mut consumer, thread.shared(), 480);
                thread.reading().finished()
            },
            "the end",
        );
        let reading = thread.reading();
        assert_eq!(reading.position, reading.end);
        assert!(reading.past_end >= reading.delay);
    }

    fn sample_time_near_end() -> Time {
        Time::from_ticks(engine::TICKS_PER_SECOND - 3000)
    }

    #[test]
    fn a_decoder_error_reaches_the_ui_and_closes_the_gate() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), || Counting { fail_at: Some(0), ..Default::default() }).unwrap();
        thread.play(Time::ZERO);
        wait_until(|| thread.control.lock().error.is_some(), "the error");
        let message = thread.take_error().expect("an error");
        assert!(message.contains("broken source"), "{message}");
        assert!(pull(&mut consumer, thread.shared(), 480).iter().all(|sample| *sample == 0.0));
        assert_eq!(thread.take_error(), None, "reported once");
    }

    #[test]
    fn a_panicking_source_is_reported_not_fatal() {
        let (thread, _consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Video), || Counting { panic_at: Some(0), ..Default::default() }).unwrap();
        thread.play(Time::ZERO);
        wait_until(|| thread.control.lock().error.is_some(), "the error");
        assert!(thread.take_error().expect("an error").contains("internal error"));
    }

    #[test]
    fn a_project_without_output_audio_plays_silence_without_reading_sources() {
        let source = Counting::default();
        let reads = source.reads.clone();
        let (thread, mut consumer) = PreparingThread::spawn(&project(1, false, TrackKind::Video), move || source).unwrap();
        thread.play(Time::ZERO);
        open_gate(&thread);
        let out = pull(&mut consumer, thread.shared(), 480);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert_eq!(thread.shared().stats().played, 480, "the clock still runs for the picture");
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_audio_only_project_has_no_length_and_does_not_panic() {
        let (thread, mut consumer) = PreparingThread::spawn(&project(2, true, TrackKind::Audio), Counting::default).unwrap();
        assert_eq!(thread.reading().end, 0);
        thread.play(Time::ZERO);
        open_gate(&thread);
        pull(&mut consumer, thread.shared(), 480);
        assert!(thread.reading().finished());
        assert_eq!(thread.reading().playhead(), Time::ZERO);
    }

    /// Plays a real-time device against the thread and returns the silence that a seek and an edit
    /// caused, in frames. `read_cost` stands in for the time a decoder takes.
    fn measure(read_cost: Duration) -> (u64, u64) {
        #[derive(Clone)]
        struct Slow(Duration);
        impl AudioSource for Slow {
            fn read(&mut self, _key: ClipKey, _source: &str, _position: i64, out: &mut [f32]) -> Result<(), RenderError> {
                thread::sleep(self.0);
                out.fill(0.25);
                Ok(())
            }
        }
        let (thread, mut consumer) = PreparingThread::spawn(&project(20, true, TrackKind::Video), move || Slow(read_cost)).unwrap();
        thread.play(Time::ZERO);
        open_gate(&thread);
        let device = |consumer: &mut AudioConsumer, duration: Duration| {
            let mut out = vec![0.0f32; DEVICE_FRAMES * 2];
            let start = Instant::now();
            let mut next = start;
            while start.elapsed() < duration {
                fill_device_buffer(consumer, thread.shared(), &mut out, Some(Duration::ZERO));
                next += Duration::from_millis(10);
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    thread::sleep(wait);
                }
            }
        };
        device(&mut consumer, Duration::from_millis(300));
        let before = thread.shared().stats().underrun;
        thread.seek(Time::from_seconds(5).unwrap());
        device(&mut consumer, Duration::from_millis(300));
        let seek = thread.shared().stats().underrun - before;
        let before = thread.shared().stats().underrun;
        thread.set_project(Arc::new(project(20, true, TrackKind::Video)));
        device(&mut consumer, Duration::from_millis(300));
        (seek, thread.shared().stats().underrun - before)
    }

    #[test]
    #[ignore = "real time, the thresholds fail on a loaded machine"]
    fn a_seek_or_edit_causes_only_brief_silence_on_a_fast_source() {
        let (seek, edit) = measure(Duration::ZERO);
        eprintln!("silence after a seek: {seek} frames, after an edit: {edit} frames (no decode cost)");
        // Tolerates a scheduler hiccup on a busy machine; anything near the lead is a regression.
        assert!(seek <= 4_800, "seek silence {seek} frames");
        assert!(edit <= 4_800, "edit silence {edit} frames");
    }

    #[test]
    #[ignore = "real time, for reading the printed numbers"]
    fn measure_silence_with_a_slow_source() {
        for millis in [0, 5, 20] {
            let (seek, edit) = measure(Duration::from_millis(millis));
            eprintln!("read cost {millis} ms: silence after a seek {seek} frames, after an edit {edit} frames");
        }
    }
}
