//! Audio clock and pull API, independent of any playback device.
//!
//! [`audio_channel`] splits the audio path into three handles over a single-producer
//! single-consumer ring of fixed blocks:
//!
//! * [`AudioProducer`], on a preparing thread, decodes and mixes. It may allocate and take time.
//! * [`AudioConsumer`], in the device callback. [`AudioConsumer::pull`] never locks, never
//!   allocates and finishes in a bounded number of steps, so it cannot wait for the producer.
//! * [`AudioClock`], for everyone else, reads the position and asks for seeks.
//!
//! # Rules
//!
//! * **Position.** `position` is the timeline sample the device consumes next. It starts at 0 and
//!   moves only when real samples are consumed.
//! * **Time.** Sample `s` maps to `ceil(s * 15 / 8)` ticks, the earliest tick with
//!   `sample_position(tick) == s`, see [`sample_time`].
//! * **Seek.** [`AudioClock::seek`] moves the position to the sample of the time asked for, limited
//!   to the timeline. Nothing prepared before a seek can be heard after it, see below.
//! * **Underrun.** When fewer samples are prepared than the device asks for, the missing part of
//!   its buffer is silence and the clock stands still for it: the next real sample continues where
//!   the audio stopped, so a stall delays the audio and never skips it. [`PullReport::underrun`]
//!   counts those frames.
//! * **End of the timeline.** After the last sample, a pull is silence that is not an underrun.
//!   The clock stays at the end until a seek.
//! * **Timeline change.** [`AudioProducer::set_project`] rebuilds the clock from the new project:
//!   it takes the new end, drops what was mixed from the old project and resumes at the same
//!   position, limited to the new timeline.
//!
//! # Why a seek needs no lock
//!
//! A seek bumps a generation counter stored together with the target position in one atomic. Every
//! block carries the generation it was mixed for. The consumer plays only blocks of the generation
//! it follows and drops the others. It adopts a new generation at the start of a pull, and also
//! when it meets a block of another generation: the producer read that generation before
//! publishing the block, so re-reading the atomic after seeing the block cannot return something
//! older. A block that still differs after that re-read is stale and is dropped.
//!
//! The ring holds whole blocks, so stale blocks occupy it until the consumer drops them on its
//! next pull; [`AudioProducer::prepare`] writes less while the ring is full. Generations wrap at
//! 2^20: a stale block could only be taken for current if it survived that many seeks unconsumed.

use crate::audio_mix::{mix_audio, AudioSource, MixReport};
use crate::audio_schedule::{audio_sample_count, sample_position, MIX_CHANNELS};
use crate::project::Project;
use crate::render::RenderError;
use crate::time::Time;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

const POSITION_BITS: u32 = 44;
const POSITION_MASK: u64 = (1 << POSITION_BITS) - 1;
const GENERATION_MASK: u32 = (1 << 20) - 1;

/// Positions must fit next to the generation; 2^44 samples are over eleven years of audio.
const MAX_POSITION: i64 = POSITION_MASK as i64;

fn pack(generation: u32, position: i64) -> u64 {
    (u64::from(generation) << POSITION_BITS) | (position as u64 & POSITION_MASK)
}

fn unpack(value: u64) -> (u32, i64) {
    ((value >> POSITION_BITS) as u32, (value & POSITION_MASK) as i64)
}

/// The earliest time whose [`sample_position`] is `sample`, computed with integers only.
pub fn sample_time(sample: i64) -> Time {
    let ticks = (i128::from(sample) * 15 + 7).div_euclid(8);
    Time::from_ticks(ticks as i64)
}

/// What one [`AudioConsumer::pull`] did. The three counts add up to the frames asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PullReport {
    /// Frames of real audio handed to the device.
    pub played: usize,
    /// Frames of silence because the audio was not prepared in time.
    pub underrun: usize,
    /// Frames of silence after the end of the timeline.
    pub past_end: usize,
}

/// What one [`AudioProducer::prepare`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrepareReport {
    /// Frames written to the ring; fewer than asked when the ring is full or the timeline ends.
    pub frames: usize,
    /// What the mixer reported, summed over those frames.
    pub mix: MixReport,
}

/// One slot of the ring. Ownership passes between the two threads through `head` and `tail`; the
/// fields are atomics only so that this needs no unsafe code, and relaxed access is enough.
struct Block {
    generation: AtomicU32,
    start: AtomicI64,
    frames: AtomicUsize,
    samples: Box<[AtomicU32]>,
}

struct Shared {
    blocks: Box<[Block]>,
    block_frames: usize,
    /// Blocks consumed so far, written by the consumer. Wraps; compared with `wrapping_sub`.
    head: AtomicUsize,
    /// Blocks published so far, written by the producer.
    tail: AtomicUsize,
    /// The newest seek: generation and target position.
    seek: AtomicU64,
    /// What the consumer follows: generation and the position it consumes next.
    consumed: AtomicU64,
    end: AtomicI64,
}

impl Shared {
    /// The pair of states to derive a position from. `consumed` is read first: the consumer only
    /// publishes a generation it saw in `seek`, so the `seek` read after it is never older.
    fn states(&self) -> (u64, u64) {
        let consumed = self.consumed.load(Ordering::Acquire);
        (consumed, self.seek.load(Ordering::Acquire))
    }

    fn position(&self) -> i64 {
        let (consumed, seek) = self.states();
        position_of(consumed, seek)
    }

    /// Publishes a seek to `target`, or to the current position when it is `None`. The position
    /// and the end are read again on every attempt, so a racing seek or timeline change is kept.
    fn publish_seek(&self, target: Option<i64>) {
        loop {
            let (consumed, current) = self.states();
            let position = target
                .unwrap_or_else(|| position_of(consumed, current))
                .clamp(0, self.end.load(Ordering::Acquire));
            let generation = unpack(current).0.wrapping_add(1) & GENERATION_MASK;
            if self
                .seek
                .compare_exchange_weak(current, pack(generation, position), Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }
}

fn position_of(consumed: u64, seek: u64) -> i64 {
    let (generation, position) = unpack(consumed);
    let (seek_generation, seek_position) = unpack(seek);
    if generation == seek_generation {
        position
    } else {
        seek_position
    }
}

fn end_of(project: &Project) -> i64 {
    audio_sample_count(project).clamp(0, MAX_POSITION)
}

/// Builds the three handles for `project`. The ring holds `block_count` blocks of `block_frames`
/// frames, allocated here and never again; both counts are at least 1.
pub fn audio_channel(
    project: Project,
    block_frames: usize,
    block_count: usize,
) -> (AudioProducer, AudioConsumer, AudioClock) {
    let block_frames = block_frames.max(1);
    let end = end_of(&project);
    let blocks = (0..block_count.max(1))
        .map(|_| Block {
            generation: AtomicU32::new(0),
            start: AtomicI64::new(0),
            frames: AtomicUsize::new(0),
            samples: (0..block_frames * MIX_CHANNELS).map(|_| AtomicU32::new(0)).collect(),
        })
        .collect();
    let shared = Arc::new(Shared {
        blocks,
        block_frames,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        seek: AtomicU64::new(pack(0, 0)),
        consumed: AtomicU64::new(pack(0, 0)),
        end: AtomicI64::new(end),
    });
    let producer = AudioProducer {
        shared: Arc::clone(&shared),
        project,
        end,
        generation: 0,
        next: 0,
        tail: 0,
        scratch: Vec::new(),
    };
    let consumer = AudioConsumer {
        shared: Arc::clone(&shared),
        generation: 0,
        position: 0,
        head: 0,
        offset: 0,
    };
    (producer, consumer, AudioClock { shared })
}

/// The position and seek control, shareable with any thread.
#[derive(Clone)]
pub struct AudioClock {
    shared: Arc<Shared>,
}

impl AudioClock {
    /// The timeline sample the device consumes next, or the target of a seek it has not seen yet.
    pub fn position(&self) -> i64 {
        self.shared.position()
    }

    /// The time of [`AudioClock::position`], derived from the samples the device consumed.
    pub fn time(&self) -> Time {
        sample_time(self.position())
    }

    /// The sample count of the timeline.
    pub fn end(&self) -> i64 {
        self.shared.end.load(Ordering::Acquire)
    }

    /// Moves to the sample of `time`, limited to the timeline, and discards the prepared samples.
    pub fn seek(&self, time: Time) {
        self.shared.publish_seek(Some(sample_position(time)));
    }
}

/// The half that decodes and mixes. Owns the project it mixes.
pub struct AudioProducer {
    shared: Arc<Shared>,
    project: Project,
    end: i64,
    generation: u32,
    /// The timeline sample the next block starts at.
    next: i64,
    tail: usize,
    scratch: Vec<f32>,
}

impl AudioProducer {
    fn sync(&mut self) {
        let (generation, position) = unpack(self.shared.seek.load(Ordering::Acquire));
        if generation != self.generation {
            self.generation = generation;
            self.next = position;
        }
    }

    /// Frames mixed for the current position and waiting for the device.
    pub fn prepared_frames(&self) -> usize {
        let (consumed, seek) = self.shared.states();
        if self.generation != unpack(seek).0 {
            return 0;
        }
        (self.next - position_of(consumed, seek)).max(0) as usize
    }

    /// Mixes up to `frames` more frames after what is prepared, stopping at the end of the timeline
    /// or when the ring is full. Ask again until enough is prepared. Each call fills whole blocks
    /// where it can but a short request still takes a block, so ask in multiples of the block size.
    pub fn prepare<S: AudioSource + ?Sized>(
        &mut self,
        source: &mut S,
        frames: usize,
    ) -> Result<PrepareReport, RenderError> {
        let mut report = PrepareReport::default();
        while report.frames < frames {
            self.sync();
            let shared = &self.shared;
            let capacity = shared.blocks.len();
            if self.tail.wrapping_sub(shared.head.load(Ordering::Acquire)) >= capacity {
                break;
            }
            let count = (frames - report.frames)
                .min(shared.block_frames)
                .min((self.end - self.next).max(0) as usize);
            if count == 0 {
                break;
            }
            self.scratch.clear();
            self.scratch.resize(count * MIX_CHANNELS, 0.0);
            let mix = mix_audio(&self.project, source, self.next, &mut self.scratch)?;
            report.mix.clamped_samples += mix.clamped_samples;

            // A seek that arrives during the mix makes this block stale, and the consumer drops it.
            let block = &shared.blocks[self.tail % capacity];
            block.generation.store(self.generation, Ordering::Relaxed);
            block.start.store(self.next, Ordering::Relaxed);
            block.frames.store(count, Ordering::Relaxed);
            for (slot, sample) in block.samples.iter().zip(&self.scratch) {
                slot.store(sample.to_bits(), Ordering::Relaxed);
            }
            self.tail = self.tail.wrapping_add(1);
            shared.tail.store(self.tail, Ordering::Release);
            self.next += count as i64;
            report.frames += count;
        }
        Ok(report)
    }

    /// Replaces the project. The clock takes the new end, what was prepared from the old project
    /// is dropped, and playback resumes at the same position, limited to the new timeline.
    pub fn set_project(&mut self, project: Project) {
        let end = end_of(&project);
        self.project = project;
        self.end = end;
        self.shared.end.store(end, Ordering::Release);
        self.shared.publish_seek(None);
        self.sync();
    }
}

/// The half for the device callback.
pub struct AudioConsumer {
    shared: Arc<Shared>,
    generation: u32,
    /// The position of the next frame, kept for the callback and published through `consumed`.
    position: i64,
    head: usize,
    /// Frames already played from the block at `head`.
    offset: usize,
}

impl AudioConsumer {
    fn adopt(&mut self, state: u64) {
        let (generation, position) = unpack(state);
        self.generation = generation;
        self.position = position;
        self.offset = 0;
        self.shared.consumed.store(state, Ordering::Release);
    }

    /// Fills `out` as the device consumes it, interleaved stereo; a trailing odd element is zeroed.
    /// Never locks, never allocates, and takes a bounded number of steps.
    pub fn pull(&mut self, out: &mut [f32]) -> PullReport {
        out.fill(0.0);
        let frames = out.len() / MIX_CHANNELS;
        let state = self.shared.seek.load(Ordering::Acquire);
        if unpack(state).0 != self.generation {
            self.adopt(state);
        }
        let end = self.shared.end.load(Ordering::Acquire);
        let mut remaining = (end - self.position).max(0) as usize;
        let mut played = 0;
        // Every step plays at least one frame, drops one block or adopts one seek.
        let capacity = self.shared.blocks.len();
        let mut steps = frames.saturating_add(capacity.saturating_mul(2));
        while played < frames && steps > 0 {
            steps -= 1;
            if self.head == self.shared.tail.load(Ordering::Acquire) {
                break;
            }
            let index = self.head % capacity;
            let generation = self.shared.blocks[index].generation.load(Ordering::Relaxed);
            if generation != self.generation {
                let state = self.shared.seek.load(Ordering::Acquire);
                if unpack(state).0 != self.generation {
                    self.adopt(state);
                    remaining = ((end - self.position).max(0) as usize).saturating_add(played);
                } else {
                    self.offset = 0;
                    self.head = self.head.wrapping_add(1);
                    self.shared.head.store(self.head, Ordering::Release);
                }
                continue;
            }
            let block = &self.shared.blocks[index];
            let start = block.start.load(Ordering::Relaxed);
            let block_frames = block.frames.load(Ordering::Relaxed);
            let take = (frames - played).min(block_frames - self.offset);
            let from = self.offset * MIX_CHANNELS;
            for (target, slot) in out[played * MIX_CHANNELS..(played + take) * MIX_CHANNELS]
                .iter_mut()
                .zip(&block.samples[from..from + take * MIX_CHANNELS])
            {
                *target = f32::from_bits(slot.load(Ordering::Relaxed));
            }
            played += take;
            self.offset += take;
            self.position = start + self.offset as i64;
            self.shared
                .consumed
                .store(pack(self.generation, self.position), Ordering::Release);
            if self.offset == block_frames {
                self.offset = 0;
                self.head = self.head.wrapping_add(1);
                self.shared.head.store(self.head, Ordering::Release);
            }
        }
        let past_end = (frames - played).saturating_sub(remaining.saturating_sub(played));
        PullReport {
            played,
            underrun: frames - played - past_end,
            past_end,
        }
    }
}
