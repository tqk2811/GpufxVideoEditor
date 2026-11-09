//! The audio clock against a fake device that consumes in blocks.

use engine::render::{ClipKey, RenderError};
use engine::{sample_position, sample_time, AudioClock, AudioSource, Project, PullReport, Time};

/// One clip of 48000 samples whose source samples equal their own position.
const PROJECT: &str = r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":3000,"audio":{"sample_rate":48000,"channels":2,"bitrate":128000}},"tracks":[
    {"clips":[{"source":"a","start":0,"source_in":0,"duration":90000}]}]}"#;
const TOTAL: usize = 48_000;

fn project() -> Project {
    Project::from_json(PROJECT).expect("valid project")
}

fn value_of(position: i64) -> f32 {
    position as f32 * 1e-5
}

struct PositionSource;

impl AudioSource for PositionSource {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            frame[0] = value_of(position + i as i64);
            frame[1] = value_of(position + i as i64);
        }
        Ok(())
    }
}

/// Keeps the blocks it was given, like a sound card reading its buffer.
struct FakeDevice {
    block: usize,
    received: Vec<f32>,
}

impl FakeDevice {
    fn new(block: usize) -> Self {
        Self { block, received: Vec::new() }
    }

    fn consume(&mut self, clock: &mut AudioClock) -> PullReport {
        let mut buffer = vec![1.0; self.block * 2];
        let report = clock.pull(&mut buffer);
        self.received.extend_from_slice(&buffer);
        report
    }
}

#[test]
fn time_is_derived_from_the_samples_the_device_consumed() {
    let project = project();
    let mut clock = AudioClock::new(&project);
    let mut device = FakeDevice::new(480);
    assert_eq!(clock.time(), Time::from_ticks(0));
    let mut consumed = 0;
    for _ in 0..20 {
        clock.prepare(&project, &mut PositionSource, 480).expect("prepare");
        // Prepared samples alone do not move the clock.
        assert_eq!(clock.position(), consumed);
        let report = device.consume(&mut clock);
        assert_eq!(report, PullReport { played: 480, underrun: 0, past_end: 0 });
        consumed += 480;
        assert_eq!(clock.position(), consumed);
        assert_eq!(clock.time(), sample_time(consumed));
    }
    // 9600 samples are 0.2 s, exactly 18000 ticks.
    assert_eq!(clock.time(), Time::from_ticks(18_000));
    for (i, frame) in device.received.chunks_exact(2).enumerate() {
        assert_eq!(frame, [value_of(i as i64), value_of(i as i64)], "sample {i} is out of order");
    }
}

#[test]
fn block_size_does_not_change_the_samples() {
    let project = project();
    let run = |block: usize| {
        let mut clock = AudioClock::new(&project);
        let mut device = FakeDevice::new(block);
        for _ in 0..2_400 / block {
            clock.prepare(&project, &mut PositionSource, block).expect("prepare");
            device.consume(&mut clock);
        }
        device.received
    };
    let a = run(160);
    let b = run(100);
    assert_eq!(a.len(), 4_800);
    assert_eq!(a, b);
}

#[test]
fn sample_times_round_trip_exactly() {
    for sample in 0..10_000 {
        let time = sample_time(sample);
        assert_eq!(sample_position(time), sample, "sample {sample}");
        if sample > 0 {
            assert!(time > sample_time(sample - 1), "not increasing at {sample}");
        }
    }
    // The earliest such tick: one tick earlier belongs to the sample before.
    assert_eq!(sample_position(Time::from_ticks(sample_time(7).ticks() - 1)), 6);
}

#[test]
fn seek_resets_the_position_and_discards_what_was_prepared() {
    let project = project();
    let mut clock = AudioClock::new(&project);
    let mut device = FakeDevice::new(256);
    clock.prepare(&project, &mut PositionSource, 4_000).expect("prepare");
    device.consume(&mut clock);
    assert_eq!(clock.prepared_frames(), 3_744);

    clock.seek(Time::from_ticks(45_000));
    assert_eq!(clock.prepared_frames(), 0, "prepared samples survive a seek");
    assert_eq!(clock.position(), 24_000);
    assert_eq!(clock.time(), Time::from_ticks(45_000));

    // A pull before anything is prepared again must not hear the old samples.
    let report = device.consume(&mut clock);
    assert_eq!(report, PullReport { played: 0, underrun: 256, past_end: 0 });
    assert_eq!(clock.position(), 24_000);

    clock.prepare(&project, &mut PositionSource, 256).expect("prepare");
    let mut buffer = [0.0; 512];
    clock.pull(&mut buffer);
    assert_eq!(buffer[0], value_of(24_000), "wrong sample after the seek");
    assert_eq!(buffer[510], value_of(24_255));
}

#[test]
fn seeking_outside_the_timeline_is_limited_to_it() {
    let project = project();
    let mut clock = AudioClock::new(&project);
    clock.seek(Time::from_ticks(-90_000));
    assert_eq!(clock.position(), 0);
    clock.seek(Time::from_ticks(900_000));
    assert_eq!(clock.position(), TOTAL as i64);
}

#[test]
fn an_underrun_is_silence_and_the_clock_waits_for_the_audio() {
    let project = project();
    let mut clock = AudioClock::new(&project);
    clock.prepare(&project, &mut PositionSource, 100).expect("prepare");
    let mut buffer = [1.0; 512];
    let report = clock.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 100, underrun: 156, past_end: 0 });
    assert_eq!(clock.position(), 100, "the clock ran during the underrun");
    assert_eq!(buffer[198], value_of(99));
    assert!(buffer[200..].iter().all(|s| *s == 0.0), "the missing frames are not silent");

    // A later block starts exactly where the audio stopped, not 156 frames ahead.
    clock.prepare(&project, &mut PositionSource, 1_000).expect("prepare");
    let report = clock.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 256, underrun: 0, past_end: 0 });
    assert_eq!(buffer[0], value_of(100), "audio was skipped after the underrun");
    assert_eq!(buffer[510], value_of(355));
}

#[test]
fn the_end_of_the_timeline_is_not_an_underrun() {
    let project = project();
    let mut clock = AudioClock::new(&project);
    clock.seek(Time::from_ticks(89_100));
    let start = clock.position();
    assert_eq!(start, TOTAL as i64 - 480);

    clock.prepare(&project, &mut PositionSource, 1_000).expect("prepare");
    assert_eq!(clock.prepared_frames(), 480, "prepared past the end of the timeline");
    let mut buffer = [1.0; 1_024];
    let report = clock.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 480, underrun: 0, past_end: 32 });
    assert_eq!(clock.position(), TOTAL as i64);
    assert!(buffer[960..].iter().all(|s| *s == 0.0));

    let report = clock.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 0, underrun: 0, past_end: 512 });
    assert_eq!(clock.position(), TOTAL as i64, "the clock ran past the end");

    clock.seek(Time::from_ticks(0));
    clock.prepare(&project, &mut PositionSource, 512).expect("prepare");
    assert_eq!(clock.pull(&mut buffer).played, 512, "a seek back resumes playback");
}

#[test]
fn an_underrun_just_before_the_end_is_still_an_underrun() {
    let project = project();
    let mut clock = AudioClock::new(&project);
    clock.seek(Time::from_ticks(89_100));
    clock.prepare(&project, &mut PositionSource, 100).expect("prepare");
    let mut buffer = [1.0; 1_024];
    let report = clock.pull(&mut buffer);
    // 480 frames remain, 100 of them are prepared, and the request reaches 32 past the end.
    assert_eq!(report, PullReport { played: 100, underrun: 380, past_end: 32 });
}
