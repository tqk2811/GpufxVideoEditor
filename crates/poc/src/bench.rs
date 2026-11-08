//! `decode-bench`: full decode throughput and accurate seek timing for one clip.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::decoder::{D3d12Decoder, DecodedFrame};
use crate::gpu::AppGpu;

const SEEK_SEED: u64 = 0x5EED_2026_1109_0001;
/// Height and cell width of the frame-number bar that scripts/setup-poc.ps1 burns in.
const INDICATOR_ROW: u32 = 32;
const INDICATOR_CELL_WIDTH: usize = 120;
const INDICATOR_BITS: usize = 16;

pub struct SeedRandom(pub u64);

impl SeedRandom {
    pub fn next_unit(&mut self) -> f64 {
        // xorshift64: a fixed sequence for a fixed seed.
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index]
}

fn read_burnt_in_index(frame: &DecodedFrame) -> Result<i64, String> {
    let row = frame.read_luma_row(INDICATOR_ROW)?;
    let mut value = 0;
    for bit in 0..INDICATOR_BITS {
        let center = bit * INDICATOR_CELL_WIDTH + INDICATOR_CELL_WIDTH / 2;
        let luma = *row.get(center).ok_or("the frame is narrower than the indicator")?;
        value = (value << 1) | i64::from(luma > 128);
    }
    Ok(value)
}

pub fn run(clip: &Path, seek_count: usize) -> Result<(), String> {
    let gpu = AppGpu::create()?;
    let device = gpu.raw_d3d12_device()?;
    println!("adapter: {}", gpu.adapter_name);

    let mut decoder = D3d12Decoder::open(clip, &device)?;
    println!("clip: {}, duration {:.3} s", clip.display(), decoder.duration_seconds());

    let started = Instant::now();
    let mut decoded_frames = 0u64;
    let mut textures = BTreeSet::new();
    let mut slices = BTreeSet::new();
    let mut first_layout = None;
    let mut layouts_differ = false;
    let mut formats = None;
    while let Some(frame) = decoder.next_frame()? {
        frame.wait_until_ready()?;
        decoded_frames += 1;
        textures.insert(frame.texture_ptr() as usize);
        slices.insert(frame.subresource_index());
        let layout = frame.texture_layout()?;
        layouts_differ |= first_layout.is_some_and(|known| known != layout);
        first_layout.get_or_insert(layout);
        formats.get_or_insert_with(|| (frame.pixel_format_name(), frame.software_format_name(), frame.width(), frame.height()));
    }
    let elapsed = started.elapsed();
    let (format, software_format, width, height) = formats.ok_or("the clip has no frames")?;
    let layout = first_layout.ok_or("the clip has no frames")?;

    println!("pixel format: {format} (surface {software_format}), {width}x{height}");
    println!(
        "full decode: {decoded_frames} frames in {:.3} s = {:.1} FPS (each frame waited on its fence)",
        elapsed.as_secs_f64(),
        decoded_frames as f64 / elapsed.as_secs_f64()
    );
    println!(
        "textures: {} distinct, DepthOrArraySize {}, subresource indices seen {} (max {}), frames context texture_array {}, frame flag TEXTURE_ARRAY {}, layout varies {}",
        textures.len(),
        layout.array_size,
        slices.len(),
        slices.iter().next_back().copied().unwrap_or(0),
        layout.frames_context_is_array,
        layout.texture_array_flag,
        layouts_differ
    );
    println!("frames are texture array slices: {}", layout.array_size > 1);
    println!(
        "texture size {}x{} for a {width}x{height} frame, D3D12_RESOURCE_FLAGS {:#x}",
        layout.texture_width, layout.texture_height, layout.resource_flags
    );

    run_seeks(&mut decoder, seek_count)?;
    drop(decoder);
    drop(gpu);
    Ok(())
}

fn run_seeks(decoder: &mut D3d12Decoder, seek_count: usize) -> Result<(), String> {
    let mut random = SeedRandom(SEEK_SEED);
    let usable_seconds = (decoder.duration_seconds() - 0.1).max(0.0);
    let mut timings_ms = Vec::with_capacity(seek_count);
    let mut skipped_total = 0u64;
    let mut wrong_timestamp = 0usize;
    let mut wrong_pixels = 0usize;
    let mut read_back_time = Duration::ZERO;

    for _ in 0..seek_count {
        let position = random.next_unit() * usable_seconds;
        let started = Instant::now();
        let result = decoder.seek_accurate(position)?;
        result.frame.wait_until_ready()?;
        timings_ms.push(started.elapsed().as_secs_f64() * 1000.0);

        skipped_total += u64::from(result.skipped_frames);
        let in_range = result.frame.pts <= result.target_pts && result.target_pts < result.frame.pts + decoder.frame_duration_pts();
        wrong_timestamp += usize::from(!in_range);

        let read_back_started = Instant::now();
        let burnt_in = read_burnt_in_index(&result.frame)?;
        read_back_time += read_back_started.elapsed();
        let expected = decoder.frame_index_of(result.frame.pts);
        if burnt_in != expected {
            wrong_pixels += 1;
            eprintln!("pixel mismatch at {position:.3} s: bar reads {burnt_in}, timestamp says {expected}");
        }
    }

    timings_ms.sort_by(f64::total_cmp);
    println!(
        "seek (n={seek_count}, seed {SEEK_SEED:#x}): p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms, mean {:.1} frames skipped",
        percentile(&timings_ms, 0.50),
        percentile(&timings_ms, 0.95),
        percentile(&timings_ms, 0.99),
        timings_ms[timings_ms.len() - 1],
        skipped_total as f64 / seek_count as f64
    );
    println!(
        "seek accuracy: {wrong_timestamp} wrong timestamps, {wrong_pixels} wrong burnt-in frame numbers (read-back excluded from timing, {:.0} ms total)",
        read_back_time.as_secs_f64() * 1000.0
    );
    Ok(())
}
