//! The audio clock against a fake device that consumes in blocks, and across real threads.

use engine::render::{ClipKey, RenderError};
use engine::{
    audio_channel, sample_position, sample_time, AudioClock, AudioConsumer, AudioProducer,
    AudioSource, Project, PullReport, Time,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One clip of 48000 samples whose source samples equal their own position.
const PROJECT: &str = r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":3000,"audio":{"sample_rate":48000,"channels":2,"bitrate":128000}},"tracks":[
    {"clips":[{"source":"a","start":0,"source_in":0,"duration":90000}]}]}"#;
const TOTAL: usize = 48_000;

fn project() -> Project {
    Project::from_json(PROJECT).expect("valid project")
}

fn project_of(ticks: i64) -> Project {
    let text = PROJECT.replace("\"duration\":90000", &format!("\"duration\":{ticks}"));
    Project::from_json(&text).expect("valid project")
}

fn channel() -> (AudioProducer, AudioConsumer, AudioClock) {
    audio_channel(project(), 256, 64)
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

    fn consume(&mut self, consumer: &mut AudioConsumer) -> PullReport {
        let mut buffer = vec![1.0; self.block * 2];
        let report = consumer.pull(&mut buffer);
        self.received.extend_from_slice(&buffer);
        report
    }
}

#[test]
fn time_is_derived_from_the_samples_the_device_consumed() {
    let (mut producer, mut consumer, clock) = channel();
    let mut device = FakeDevice::new(480);
    assert_eq!(clock.time(), Time::from_ticks(0));
    let mut consumed = 0;
    for _ in 0..20 {
        producer.prepare(&mut PositionSource, 480).expect("prepare");
        // Prepared samples alone do not move the clock.
        assert_eq!(clock.position(), consumed);
        let report = device.consume(&mut consumer);
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
    let run = |block: usize| {
        let (mut producer, mut consumer, _clock) = channel();
        let mut device = FakeDevice::new(block);
        for _ in 0..2_400 / block {
            producer.prepare(&mut PositionSource, block).expect("prepare");
            device.consume(&mut consumer);
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
    let (mut producer, mut consumer, clock) = channel();
    let mut device = FakeDevice::new(256);
    producer.prepare(&mut PositionSource, 4_000).expect("prepare");
    device.consume(&mut consumer);
    assert_eq!(producer.prepared_frames(), 3_744);

    clock.seek(Time::from_ticks(45_000));
    assert_eq!(producer.prepared_frames(), 0, "prepared samples survive a seek");
    assert_eq!(clock.position(), 24_000);
    assert_eq!(clock.time(), Time::from_ticks(45_000));

    // A pull before anything is prepared again must not hear the old samples.
    let report = device.consume(&mut consumer);
    assert_eq!(report, PullReport { played: 0, underrun: 256, past_end: 0 });
    assert_eq!(clock.position(), 24_000);

    producer.prepare(&mut PositionSource, 256).expect("prepare");
    let mut buffer = [0.0; 512];
    consumer.pull(&mut buffer);
    assert_eq!(buffer[0], value_of(24_000), "wrong sample after the seek");
    assert_eq!(buffer[510], value_of(24_255));
}

#[test]
fn a_seek_drops_the_rest_of_a_half_played_block() {
    let (mut producer, mut consumer, clock) = channel();
    producer.prepare(&mut PositionSource, 256).expect("prepare");
    let mut buffer = [0.0; 200];
    consumer.pull(&mut buffer);
    assert_eq!(clock.position(), 100);

    clock.seek(Time::from_ticks(45_000));
    producer.prepare(&mut PositionSource, 256).expect("prepare");
    let report = consumer.pull(&mut buffer);
    assert_eq!(report.played, 100);
    assert_eq!(buffer[0], value_of(24_000), "the old block kept playing");
}

#[test]
fn a_pull_skips_old_blocks_and_plays_the_blocks_of_the_seek() {
    let (mut producer, mut consumer, clock) = channel();
    producer.prepare(&mut PositionSource, 256).expect("prepare");
    let mut buffer = [0.0; 200];
    consumer.pull(&mut buffer);

    clock.seek(Time::from_ticks(45_000));
    producer.prepare(&mut PositionSource, 256).expect("prepare");
    // The consumer follows the old generation until this pull; the first block is stale.
    let mut long = [0.0; 1_024];
    let report = consumer.pull(&mut long);
    assert_eq!(report, PullReport { played: 256, underrun: 256, past_end: 0 });
    assert_eq!(long[0], value_of(24_000));
    assert_eq!(long[510], value_of(24_255));
    assert_eq!(clock.position(), 24_256);
}

#[test]
fn seeking_outside_the_timeline_is_limited_to_it() {
    let (_producer, _consumer, clock) = channel();
    clock.seek(Time::from_ticks(-90_000));
    assert_eq!(clock.position(), 0);
    clock.seek(Time::from_ticks(900_000));
    assert_eq!(clock.position(), TOTAL as i64);
}

#[test]
fn an_underrun_is_silence_and_the_clock_waits_for_the_audio() {
    let (mut producer, mut consumer, clock) = channel();
    producer.prepare(&mut PositionSource, 100).expect("prepare");
    let mut buffer = [1.0; 512];
    let report = consumer.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 100, underrun: 156, past_end: 0 });
    assert_eq!(clock.position(), 100, "the clock ran during the underrun");
    assert_eq!(buffer[198], value_of(99));
    assert!(buffer[200..].iter().all(|s| *s == 0.0), "the missing frames are not silent");

    // A later block starts exactly where the audio stopped, not 156 frames ahead.
    producer.prepare(&mut PositionSource, 1_000).expect("prepare");
    let report = consumer.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 256, underrun: 0, past_end: 0 });
    assert_eq!(buffer[0], value_of(100), "audio was skipped after the underrun");
    assert_eq!(buffer[510], value_of(355));
}

#[test]
fn the_end_of_the_timeline_is_not_an_underrun() {
    let (mut producer, mut consumer, clock) = channel();
    clock.seek(Time::from_ticks(89_100));
    let start = clock.position();
    assert_eq!(start, TOTAL as i64 - 480);

    producer.prepare(&mut PositionSource, 1_000).expect("prepare");
    assert_eq!(producer.prepared_frames(), 480, "prepared past the end of the timeline");
    let mut buffer = [1.0; 1_024];
    let report = consumer.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 480, underrun: 0, past_end: 32 });
    assert_eq!(clock.position(), TOTAL as i64);
    assert!(buffer[960..].iter().all(|s| *s == 0.0));

    let report = consumer.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 0, underrun: 0, past_end: 512 });
    assert_eq!(clock.position(), TOTAL as i64, "the clock ran past the end");

    clock.seek(Time::from_ticks(0));
    producer.prepare(&mut PositionSource, 512).expect("prepare");
    assert_eq!(consumer.pull(&mut buffer).played, 512, "a seek back resumes playback");
}

#[test]
fn an_underrun_just_before_the_end_is_still_an_underrun() {
    let (mut producer, mut consumer, clock) = channel();
    clock.seek(Time::from_ticks(89_100));
    producer.prepare(&mut PositionSource, 100).expect("prepare");
    let mut buffer = [1.0; 1_024];
    let report = consumer.pull(&mut buffer);
    // 480 frames remain, 100 of them are prepared, and the request reaches 32 past the end.
    assert_eq!(report, PullReport { played: 100, underrun: 380, past_end: 32 });
}

#[test]
fn a_full_ring_of_old_blocks_recovers_on_the_next_pull() {
    let (mut producer, mut consumer, clock) = audio_channel(project(), 64, 4);
    assert_eq!(producer.prepare(&mut PositionSource, 1_000).expect("prepare").frames, 256);
    clock.seek(Time::from_ticks(45_000));
    assert_eq!(producer.prepare(&mut PositionSource, 64).expect("prepare").frames, 0);

    let mut buffer = [0.0; 128];
    let report = consumer.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 0, underrun: 64, past_end: 0 });
    assert_eq!(producer.prepare(&mut PositionSource, 64).expect("prepare").frames, 64);
    consumer.pull(&mut buffer);
    assert_eq!(buffer[0], value_of(24_000));
}

#[test]
fn a_timeline_change_rebuilds_the_clock_from_the_new_project() {
    let (mut producer, mut consumer, clock) = channel();
    producer.prepare(&mut PositionSource, 1_000).expect("prepare");
    let mut buffer = [0.0; 512];
    consumer.pull(&mut buffer);
    assert_eq!(clock.position(), 256);

    producer.set_project(project_of(9_000));
    assert_eq!(clock.end(), 4_800);
    assert_eq!(clock.position(), 256, "the position survives a timeline change");
    assert_eq!(producer.prepared_frames(), 0, "samples of the old project survive");

    let report = consumer.pull(&mut buffer);
    assert_eq!(report.played, 0, "samples of the old project were played");

    clock.seek(Time::from_ticks(90_000));
    assert_eq!(clock.position(), 4_800, "seek is limited by the new end");
    let report = consumer.pull(&mut buffer);
    assert_eq!(report, PullReport { played: 0, underrun: 0, past_end: 256 });
}

#[test]
fn a_timeline_change_never_undoes_a_seek() {
    let (mut producer, _consumer, clock) = channel();
    let current = project();
    // Bounded so the 20-bit generation cannot wrap back onto the idle consumer's.
    let changes = std::thread::spawn(move || {
        for _ in 0..300_000 {
            producer.set_project(current.clone());
        }
    });
    let mut round = 0_i64;
    while round < 300_000 && !changes.is_finished() {
        let target = sample_time(1_000 + round % 1_000);
        clock.seek(target);
        assert_eq!(clock.position(), sample_position(target), "round {round}");
        round += 1;
    }
    changes.join().expect("the timeline changes finish");
}

#[test]
fn a_timeline_change_past_the_position_moves_it_to_the_new_end() {
    let (mut producer, _consumer, clock) = channel();
    clock.seek(Time::from_ticks(45_000));
    producer.set_project(project_of(9_000));
    assert_eq!(clock.position(), 4_800);
    producer.set_project(project_of(180_000));
    assert_eq!(clock.end(), 96_000);
    assert_eq!(clock.position(), 4_800);
    assert_eq!(producer.prepare(&mut PositionSource, 512).expect("prepare").frames, 512);
}

/// Encodes a position exactly in an `f32` inside `[-1, 1]`, which the mixer does not change.
fn exact_value(position: i64) -> f32 {
    position as f32 / 16_777_216.0
}

fn exact_position(value: f32) -> i64 {
    (value as f64 * 16_777_216.0).round() as i64
}

struct ExactSource;

impl AudioSource for ExactSource {
    fn read(&mut self, _key: ClipKey, _source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            frame[0] = exact_value(position + i as i64);
            frame[1] = exact_value(position + i as i64);
        }
        Ok(())
    }
}

/// Runs a producer until `stop`, with a pause now and then so the consumer also underruns.
fn spawn_producer(mut producer: AudioProducer, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut rounds = 0_u64;
        while !stop.load(Ordering::Relaxed) {
            let written = producer.prepare(&mut ExactSource, 256).expect("prepare").frames;
            rounds += 1;
            if written == 0 {
                std::thread::yield_now();
            } else if rounds % 16 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    })
}

#[test]
fn two_threads_play_every_sample_once_in_order() {
    const FRAMES: i64 = 200_000;
    let (producer, mut consumer, clock) = audio_channel(project_of(1_000_000), 256, 8);
    let stop = Arc::new(AtomicBool::new(false));
    let producing = spawn_producer(producer, Arc::clone(&stop));

    let mut expected = 0;
    let mut underrun = 0_usize;
    let mut buffer = [0.0; 480];
    let deadline = Instant::now() + Duration::from_secs(60);
    while expected < FRAMES {
        assert!(Instant::now() < deadline, "stalled at {expected}");
        let report = consumer.pull(&mut buffer);
        assert_eq!(report.played + report.underrun + report.past_end, 240);
        underrun += report.underrun;
        for frame in buffer[..report.played * 2].chunks_exact(2) {
            assert_eq!(exact_position(frame[0]), expected, "left channel");
            assert_eq!(exact_position(frame[1]), expected, "right channel");
            expected += 1;
        }
        assert_eq!(clock.position(), expected, "position is not the samples consumed");
    }
    stop.store(true, Ordering::Relaxed);
    producing.join().expect("producer");
    assert!(underrun > 0, "the test never saw an underrun");
}

#[test]
fn seeks_never_replay_or_resurrect_old_samples() {
    const SEEKS: i64 = 400;
    const STEP: i64 = 20_000;
    const FIRST: i64 = 1_000;
    let target = |k: i64| FIRST + k * STEP;
    let ticks = sample_time(target(SEEKS) + 400_000).ticks();
    let (producer, mut consumer, clock) = audio_channel(project_of(ticks), 256, 8);
    let stop = Arc::new(AtomicBool::new(false));
    let producing = spawn_producer(producer, Arc::clone(&stop));

    let device_stop = Arc::clone(&stop);
    let device = std::thread::spawn(move || {
        let mut last: Option<i64> = None;
        let mut jumps = 0;
        let mut frames = 0_u64;
        let mut buffer = [0.0; 128];
        while !device_stop.load(Ordering::Relaxed) {
            let report = consumer.pull(&mut buffer);
            for frame in buffer[..report.played * 2].chunks_exact(2) {
                let position = exact_position(frame[0]);
                frames += 1;
                match last {
                    Some(previous) if position == previous + 1 => {}
                    Some(previous) => {
                        assert!(position > previous, "{position} replayed after {previous}");
                        assert_eq!(
                            (position - FIRST).rem_euclid(STEP),
                            0,
                            "{position} after {previous} is not the target of a seek"
                        );
                        jumps += 1;
                    }
                    // A seek may land before the device plays anything.
                    None => assert!(
                        position == 0 || (position - FIRST).rem_euclid(STEP) == 0,
                        "playback started at {position}"
                    ),
                }
                last = Some(position);
            }
        }
        (last, jumps, frames)
    });

    for k in 1..=SEEKS {
        clock.seek(sample_time(target(k)));
        std::thread::sleep(Duration::from_micros(500));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while clock.position() <= target(SEEKS) && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(clock.position() > target(SEEKS), "playback never reached the last seek");
    stop.store(true, Ordering::Relaxed);
    producing.join().expect("producer");
    let (last, jumps, frames) = device.join().expect("device");
    let last = last.expect("no sample was played");
    assert!(last >= target(SEEKS), "last sample {last} is before the last seek");
    assert!(jumps > 0 && frames > 0, "jumps {jumps}, frames {frames}");
}
