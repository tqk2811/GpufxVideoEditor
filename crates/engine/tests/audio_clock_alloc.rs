//! `AudioConsumer::pull` must not allocate: it runs inside a real-time device callback. This is its
//! own test binary because it replaces the global allocator.

use engine::render::{ClipKey, RenderError};
use engine::{audio_channel, AudioSource, Project, Time};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct Silence;

impl AudioSource for Silence {
    fn read(&mut self, _key: ClipKey, _source: &str, _position: i64, _out: &mut [f32]) -> Result<(), RenderError> {
        Ok(())
    }
}

#[test]
fn pull_does_not_allocate() {
    let project = Project::from_json(
        r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":3000,"audio":{"sample_rate":48000,"channels":2,"bitrate":128000}},"tracks":[
        {"clips":[{"source":"a","start":0,"source_in":0,"duration":90000}]}]}"#,
    )
    .expect("valid project");
    let (mut producer, mut consumer, clock) = audio_channel(project, 256, 8);
    producer.prepare(&mut Silence, 1_024).expect("prepare");
    let mut buffer = [0.0_f32; 512];

    let before = ALLOCATIONS.with(Cell::get);
    consumer.pull(&mut buffer); // played
    consumer.pull(&mut buffer); // played
    clock.seek(Time::from_ticks(45_000)); // adopted and dropped in the next pull
    consumer.pull(&mut buffer); // stale blocks and an underrun
    let after = ALLOCATIONS.with(Cell::get);
    assert_eq!(after, before, "pull allocated");
}

#[test]
fn discard_stale_does_not_allocate() {
    let project = Project::from_json(
        r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":3000,"audio":{"sample_rate":48000,"channels":2,"bitrate":128000}},"tracks":[
        {"clips":[{"source":"a","start":0,"source_in":0,"duration":90000}]}]}"#,
    )
    .expect("valid project");
    let (mut producer, mut consumer, clock) = audio_channel(project, 256, 8);
    producer.prepare(&mut Silence, 1_024).expect("prepare");

    let before = ALLOCATIONS.with(Cell::get);
    consumer.discard_stale(); // nothing stale
    clock.seek(Time::from_ticks(45_000));
    consumer.discard_stale(); // adopted, and the blocks dropped
    let after = ALLOCATIONS.with(Cell::get);
    assert_eq!(after, before, "discard_stale allocated");
}
