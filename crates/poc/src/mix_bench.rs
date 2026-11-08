//! `bench`: the phase 0 measurements of docs/plans/spec-poc-phase0-vi.md, in a real window.
//!
//! Each step of the event loop does one unit of work, so the window keeps pumping messages.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpufx_core::{FrameInfo, Matrix, MemoryStats, PoolStats};
use serde_json::{json, Value};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::bench::{percentile, SeedRandom};
use crate::compositor::MixCompositor;
use crate::decoder::{D3d12Decoder, DecodedFrame, Nv12Image};
use crate::gpu::{AppGpu, VideoMemoryProbe};
use crate::presenter::{fit_image, Presenter};
use crate::validation::ValidationLog;

const SEEK_SEED: u64 = 0x5EED_2026_1109_0002;
const SEEK_COUNT: usize = 200;
/// Discarded seeks first, so pipeline creation and first-use costs do not count.
const SEEK_WARMUP: usize = 5;
const PLAYBACK_FRAMES: usize = 590;
const THREE_CLIP_FRAMES: usize = 150;
const VERIFY_POSITIONS: [(f64, f64); 3] = [(0.0, 3.2), (0.5, 11.7), (1.0, 17.1)];
/// A pixel-grid step that gives about 2000 samples.
const SAMPLE_STEP: usize = 32;
/// Accepted mean difference per 8-bit channel between the output and the CPU reference.
const MAX_MEAN_ERROR: f64 = 5.0;
const MIN_MEAN_BRIGHTNESS: f64 = 10.0;

/// Converts an NV12 image to RGB on the CPU, independently of gpufx.
fn expected_rgb(image: &Nv12Image, matrix: Matrix, x: usize, y: usize) -> [f64; 3] {
    let luma = f64::from(image.luma[y * image.width + x]);
    let chroma_row = (y / 2) * image.width;
    let chroma_column = (x / 2) * 2;
    let u = f64::from(image.chroma[chroma_row + chroma_column]) - 128.0;
    let v = f64::from(image.chroma[chroma_row + chroma_column + 1]) - 128.0;
    let (r_v, g_u, g_v, b_u) = match matrix {
        Matrix::Bt601 => (1.402, 0.344_136, 0.714_136, 1.772),
        Matrix::Bt709 => (1.5748, 0.187_324, 0.468_124, 1.8556),
        Matrix::Bt2020 => (1.4746, 0.164_553, 0.571_353, 1.8814),
    };
    let scaled = (luma - 16.0) * 255.0 / 219.0;
    let scale = 255.0 / 224.0;
    let channels = [scaled + r_v * v * scale, scaled - (g_u * u + g_v * v) * scale, scaled + b_u * u * scale];
    channels.map(|c| c.clamp(0.0, 255.0))
}

struct SampleComparison {
    samples: usize,
    mean_error: f64,
    p99_error: f64,
    mean_brightness: f64,
}

/// Compares `output` (RGBA8) with `expected` on a grid of pixels.
fn compare(output: &[u8], width: usize, height: usize, expected: impl Fn(usize, usize) -> [f64; 3]) -> SampleComparison {
    let mut errors = Vec::new();
    let mut brightness = 0.0;
    for y in (SAMPLE_STEP / 2..height).step_by(SAMPLE_STEP) {
        for x in (SAMPLE_STEP / 2..width).step_by(SAMPLE_STEP) {
            let pixel = &output[(y * width + x) * 4..(y * width + x) * 4 + 3];
            let want = expected(x, y);
            let error = (0..3).map(|c| (f64::from(pixel[c]) - want[c]).abs()).sum::<f64>() / 3.0;
            brightness += pixel.iter().map(|&c| f64::from(c)).sum::<f64>() / 3.0;
            errors.push(error);
        }
    }
    errors.sort_by(f64::total_cmp);
    SampleComparison {
        samples: errors.len(),
        mean_error: errors.iter().sum::<f64>() / errors.len() as f64,
        p99_error: percentile(&errors, 0.99),
        mean_brightness: brightness / errors.len() as f64,
    }
}

fn statistics(values: &[f64]) -> Value {
    if values.is_empty() {
        return json!({ "n": 0 });
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "n": sorted.len(),
        "p50_ms": percentile(&sorted, 0.50),
        "p95_ms": percentile(&sorted, 0.95),
        "p99_ms": percentile(&sorted, 0.99),
        "max_ms": sorted[sorted.len() - 1],
    })
}

fn memory_json(memory: &MemoryStats, pool: &PoolStats) -> Value {
    json!({
        "pass_textures": memory.pass_textures,
        "pass_texture_bytes": memory.pass_texture_bytes,
        "input_textures": memory.input_textures,
        "input_texture_bytes": memory.input_texture_bytes,
        "readback_bytes": memory.readback_bytes,
        "pool_idle_bytes": pool.idle_bytes,
        "pool_borrowed_bytes": pool.borrowed_bytes,
        "pool_peak_bytes": pool.peak_bytes,
        "pool_created": pool.created,
    })
}

enum Phase {
    Verify,
    VerifyAfterLoad,
    SeekWarmup(usize),
    Seek(usize),
    PlaybackStart,
    Playback(usize),
    ThreeClips(usize),
    Finished,
}

struct Bench {
    gpu: AppGpu,
    compositor: MixCompositor,
    decoders: Vec<D3d12Decoder>,
    presenter: Presenter,
    probe: VideoMemoryProbe,
    baseline_vram: u64,
    peak_vram: u64,
    random: SeedRandom,
    phase: Phase,
    results: serde_json::Map<String, Value>,
    wrong_seek_timestamps: usize,
    seek_times: Vec<f64>,
    frame_times: Vec<f64>,
    frame_times_no_present: Vec<f64>,
    latencies_present: Vec<f64>,
    latencies_gpu_done: Vec<f64>,
    acquire_times: Vec<f64>,
    present_times: Vec<f64>,
    frame_times_unpaced: Vec<f64>,
    latencies_unpaced: Vec<f64>,
    skipped_presents: usize,
    copy_note: String,
    validation: Option<ValidationLog>,
    mixes_with_decoder_running: usize,
    mixes_total: usize,
}

impl Bench {
    fn open(event_loop: &ActiveEventLoop, clips: &[PathBuf]) -> Result<Self, String> {
        let gpu = AppGpu::create()?;
        let validation = gpu.validation_log()?;
        let device = gpu.raw_d3d12_device()?;
        let probe = VideoMemoryProbe::for_device(&device)?;
        let baseline_vram = probe.current_usage()?;
        let decoders = clips.iter().map(|clip| D3d12Decoder::open(clip, &device)).collect::<Result<Vec<_>, _>>()?;
        let compositor = MixCompositor::create(&gpu, &device, 1920, 1080)?;
        let attributes = Window::default_attributes().with_title("poc bench").with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("creating the window failed: {e}"))?);
        let presenter = Presenter::new(&gpu, window, compositor.output(), false)?;
        let mut results = serde_json::Map::new();
        results.insert("adapter".to_owned(), json!(gpu.adapter_name));
        results.insert("clips".to_owned(), json!(clips.iter().map(|c| c.display().to_string()).collect::<Vec<_>>()));
        results.insert("present_mode".to_owned(), json!(format!("{:?}", presenter.present_mode())));
        results.insert("present_modes_available".to_owned(), json!(format!("{:?}", presenter.present_modes())));
        Ok(Self {
            gpu,
            compositor,
            decoders,
            presenter,
            probe,
            baseline_vram,
            peak_vram: baseline_vram,
            random: SeedRandom(SEEK_SEED),
            phase: Phase::Verify,
            results,
            wrong_seek_timestamps: 0,
            seek_times: Vec::with_capacity(SEEK_COUNT),
            frame_times: Vec::with_capacity(PLAYBACK_FRAMES),
            frame_times_no_present: Vec::with_capacity(PLAYBACK_FRAMES),
            latencies_present: Vec::with_capacity(PLAYBACK_FRAMES),
            latencies_gpu_done: Vec::with_capacity(PLAYBACK_FRAMES),
            acquire_times: Vec::with_capacity(PLAYBACK_FRAMES),
            present_times: Vec::with_capacity(PLAYBACK_FRAMES),
            frame_times_unpaced: Vec::with_capacity(PLAYBACK_FRAMES),
            latencies_unpaced: Vec::with_capacity(PLAYBACK_FRAMES),
            skipped_presents: 0,
            copy_note: String::new(),
            validation,
            mixes_with_decoder_running: 0,
            mixes_total: 0,
        })
    }

    /// Later decodes may still read a frame the decoder has output; `compositor.mix` needs them done.
    fn wait_decoders_idle(&mut self, clips: &[usize]) -> Result<(), String> {
        let mut running = false;
        for &clip in clips {
            running |= self.decoders[clip].wait_until_idle()?;
        }
        self.mixes_total += 1;
        self.mixes_with_decoder_running += usize::from(running);
        Ok(())
    }

    fn sample_vram(&mut self) -> Result<(), String> {
        self.peak_vram = self.peak_vram.max(self.probe.current_usage()?);
        Ok(())
    }

    fn usable_seconds(&self) -> f64 {
        self.decoders.iter().map(|d| d.duration_seconds()).fold(f64::MAX, f64::min) - 0.1
    }

    fn note_copy_path(&mut self, frame: &DecodedFrame) -> Result<(), String> {
        if !self.copy_note.is_empty() {
            return Ok(());
        }
        let layout = frame.texture_layout()?;
        let exact = layout.texture_width == u64::from(frame.width()) && layout.texture_height == frame.height() && layout.array_size == 1;
        self.copy_note = format!(
            "{} (decoder texture {}x{}, array size {}, frame {}x{})",
            if exact { "zero-copy" } else { "one GPU copy per plane" },
            layout.texture_width,
            layout.texture_height,
            layout.array_size,
            frame.width(),
            frame.height()
        );
        Ok(())
    }

    /// One blend at a known position, checked against an independent CPU conversion of both frames.
    fn verify_one(&mut self, progress: f64, seconds: f64) -> Result<Value, String> {
        let mut first = self.decoders[0].seek_accurate(seconds)?;
        let mut second = self.decoders[1].seek_accurate(seconds)?;
        self.wait_decoders_idle(&[0, 1])?;
        self.note_copy_path(&first.frame)?;
        let first_image = first.frame.read_nv12()?;
        let second_image = second.frame.read_nv12()?;
        let first_matrix = first.frame.color_info()?.matrix;
        let second_matrix = second.frame.color_info()?.matrix;
        let info = FrameInfo::new(seconds, 0);
        self.compositor.mix(&mut first.frame, &mut second.frame, progress, &info)?;
        let output = self.compositor.output().read_pixels(&self.gpu)?;
        let width = first_image.width;
        let height = first_image.height;
        let blended = compare(&output, width, height, |x, y| {
            let a = expected_rgb(&first_image, first_matrix, x, y);
            let b = expected_rgb(&second_image, second_matrix, x, y);
            [0, 1, 2].map(|c| a[c] * (1.0 - progress) + b[c] * progress)
        });
        let against_first = compare(&output, width, height, |x, y| expected_rgb(&first_image, first_matrix, x, y));
        let against_second = compare(&output, width, height, |x, y| expected_rgb(&second_image, second_matrix, x, y));
        let matches_blend = blended.mean_error <= MAX_MEAN_ERROR;
        let not_black = blended.mean_brightness >= MIN_MEAN_BRIGHTNESS;
        Ok(json!({
            "progress": progress,
            "seconds": seconds,
            "first_matrix": format!("{first_matrix:?}"),
            "samples": blended.samples,
            "error_vs_expected_blend": { "mean": blended.mean_error, "p99": blended.p99_error },
            "error_vs_first": against_first.mean_error,
            "error_vs_second": against_second.mean_error,
            "mean_brightness": blended.mean_brightness,
            "passed": matches_blend && not_black,
        }))
    }

    fn run_verification(&mut self, key: &str) -> Result<(), String> {
        let mut outcomes = Vec::new();
        let mut all_passed = true;
        for (progress, seconds) in VERIFY_POSITIONS {
            let outcome = self.verify_one(progress, seconds)?;
            all_passed &= outcome["passed"].as_bool().unwrap_or(false);
            println!("verify progress {progress}: {outcome}");
            outcomes.push(outcome);
        }
        self.results.insert(key.to_owned(), json!({ "max_mean_error": MAX_MEAN_ERROR, "all_passed": all_passed, "cases": outcomes }));
        if !all_passed {
            return Err("the blend verification failed; see the printed cases".to_owned());
        }
        Ok(())
    }

    /// One random seek of both clips through the effect, timed up to the finished output.
    fn seek_once(&mut self, measured: bool) -> Result<(), String> {
        let usable = self.usable_seconds();
        let (first_seconds, second_seconds, progress) = (self.random.next_unit() * usable, self.random.next_unit() * usable, self.random.next_unit());
        let started = Instant::now();
        let mut first = self.decoders[0].seek_accurate(first_seconds)?;
        let mut second = self.decoders[1].seek_accurate(second_seconds)?;
        self.wait_decoders_idle(&[0, 1])?;
        let info = FrameInfo::new(first_seconds, 0);
        self.compositor.mix(&mut first.frame, &mut second.frame, progress, &info)?;
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;

        if measured {
            self.seek_times.push(elapsed);
            let duration = self.decoders[0].frame_duration_pts();
            let in_range = |frame_pts: i64, target: i64| frame_pts <= target && target < frame_pts + duration;
            self.wrong_seek_timestamps += usize::from(!in_range(first.frame.pts, first.target_pts) || !in_range(second.frame.pts, second.target_pts));
        }
        self.sample_vram()
    }

    fn start_playback(&mut self) -> Result<(), String> {
        for decoder in &mut self.decoders[..2] {
            decoder.seek_accurate(0.0)?;
        }
        Ok(())
    }

    fn playback_frame(&mut self, index: usize) -> Result<bool, String> {
        let frame_started = Instant::now();
        let Some(mut first) = self.decoders[0].next_frame()? else { return Ok(false) };
        let decoder_returned = Instant::now();
        let Some(mut second) = self.decoders[1].next_frame()? else { return Ok(false) };
        self.note_copy_path(&first)?;
        self.wait_decoders_idle(&[0, 1])?;
        let progress = index as f64 / (PLAYBACK_FRAMES - 1) as f64;
        let info = FrameInfo::new(index as f64 / 30.0, index as u64);
        self.compositor.mix(&mut first, &mut second, progress, &info)?;
        let mix_done = Instant::now();

        let size = egui::vec2(self.compositor.output().width() as f32, self.compositor.output().height() as f32);
        let submission = self.presenter.render(&self.gpu, self.compositor.output(), |root, texture_id| {
            egui::CentralPanel::default().show(root, |ui| {
                ui.label(format!("bench frame {index}, progress {progress:.3}"));
                ui.image(egui::load::SizedTexture::new(texture_id, fit_image(size, ui.available_size())));
            });
        })?;
        let presented = Instant::now();

        let Some(submission) = submission else {
            self.skipped_presents += 1;
            return Ok(true);
        };
        // The draw must finish before the next mix writes the output, and this makes the
        // recorded frame time include all of its GPU work.
        self.gpu.wait_for(submission)?;
        let gpu_done = Instant::now();

        self.frame_times.push(gpu_done.duration_since(frame_started).as_secs_f64() * 1000.0);
        self.frame_times_no_present.push(mix_done.duration_since(frame_started).as_secs_f64() * 1000.0);
        self.latencies_present.push(presented.duration_since(decoder_returned).as_secs_f64() * 1000.0);
        self.latencies_gpu_done.push(gpu_done.duration_since(decoder_returned).as_secs_f64() * 1000.0);
        let (acquire, present) = self.presenter.last_blocking();
        let blocked_ms = (acquire + present).as_secs_f64() * 1000.0;
        self.acquire_times.push(acquire.as_secs_f64() * 1000.0);
        self.present_times.push(present.as_secs_f64() * 1000.0);
        self.frame_times_unpaced.push(gpu_done.duration_since(frame_started).as_secs_f64() * 1000.0 - blocked_ms);
        self.latencies_unpaced.push(gpu_done.duration_since(decoder_returned).as_secs_f64() * 1000.0 - blocked_ms);
        self.sample_vram()?;
        Ok(true)
    }

    fn three_clip_frame(&mut self) -> Result<(), String> {
        let mut frames = Vec::with_capacity(3);
        for decoder in &mut self.decoders {
            let frame = match decoder.next_frame()? {
                Some(frame) => frame,
                None => {
                    decoder.seek_accurate(0.0)?;
                    decoder.next_frame()?.ok_or("no frame after rewinding")?
                }
            };
            frames.push(frame);
        }
        self.wait_decoders_idle(&[0, 1])?;
        let (first, rest) = frames.split_at_mut(1);
        let info = FrameInfo::new(0.0, 0);
        self.compositor.mix(&mut first[0], &mut rest[0], 0.5, &info)?;
        self.sample_vram()
    }

    fn drain_validation(&mut self) -> Result<(), String> {
        self.validation.as_mut().map_or(Ok(()), ValidationLog::drain)
    }

    fn finish(&mut self) -> Result<(), String> {
        self.drain_validation()?;
        if let Some(validation) = &self.validation {
            let summary = validation.summary();
            println!("d3d12 validation: {summary}");
            self.results.insert("d3d12_validation".to_owned(), summary);
        }
        let memory = self.compositor.graph_memory();
        let pool = self.compositor.pool_stats();
        let current = self.probe.current_usage()?;
        self.results.insert("frame_path".to_owned(), json!(self.copy_note));
        self.results.insert(
            "decoder_idle_wait".to_owned(),
            json!({ "mixes": self.mixes_total, "mixes_where_a_decode_was_still_running": self.mixes_with_decoder_running }),
        );
        self.results.insert(
            "synchronization".to_owned(),
            json!("CPU waits: the output fence after each mix, and the app submission after each draw"),
        );
        self.results.insert(
            "seek".to_owned(),
            json!({
                "seed": format!("{SEEK_SEED:#x}"),
                "warmup_discarded": SEEK_WARMUP,
                "definition": "both clips seek accurately to random positions, then mix, until the GPU finished the output",
                "wrong_timestamps": self.wrong_seek_timestamps,
                "timing": statistics(&self.seek_times),
            }),
        );
        self.results.insert(
            "playback".to_owned(),
            json!({
                "definition": "decode next frame of 2 clips + mix + egui draw + present + GPU idle wait; progress ramps 0 to 1",
                "skipped_presents": self.skipped_presents,
                "frame_time": statistics(&self.frame_times),
                "up_to_mix_done": statistics(&self.frame_times_no_present),
                "blocked_in_surface_acquire": statistics(&self.acquire_times),
                "blocked_in_present": statistics(&self.present_times),
                "frame_time_minus_acquire_and_present_blocking": statistics(&self.frame_times_unpaced),
            }),
        );
        self.results.insert(
            "latency".to_owned(),
            json!({
                "definition": "from the first decoder returning its frame",
                "until_present_returned": statistics(&self.latencies_present),
                "until_gpu_finished": statistics(&self.latencies_gpu_done),
                "until_gpu_finished_minus_acquire_and_present_blocking": statistics(&self.latencies_unpaced),
            }),
        );
        self.results.insert(
            "vram".to_owned(),
            json!({
                "clips_open": self.decoders.len(),
                "baseline_bytes": self.baseline_vram,
                "peak_bytes": self.peak_vram,
                "peak_over_baseline_bytes": self.peak_vram - self.baseline_vram,
                "final_bytes": current,
                "gpufx": memory_json(&memory, &pool),
            }),
        );
        Ok(())
    }

    /// Advances the benchmark by one unit; returns true when everything has run.
    fn step(&mut self) -> Result<bool, String> {
        self.drain_validation()?;
        match self.phase {
            Phase::Verify => {
                self.run_verification("blend_verification")?;
                self.phase = Phase::SeekWarmup(0);
            }
            Phase::SeekWarmup(done) => {
                self.seek_once(false)?;
                self.phase = if done + 1 == SEEK_WARMUP { Phase::Seek(0) } else { Phase::SeekWarmup(done + 1) };
            }
            Phase::Seek(done) => {
                self.seek_once(true)?;
                self.phase = if done + 1 == SEEK_COUNT { Phase::PlaybackStart } else { Phase::Seek(done + 1) };
            }
            Phase::PlaybackStart => {
                self.start_playback()?;
                self.phase = Phase::Playback(0);
            }
            Phase::Playback(done) => {
                let more = self.playback_frame(done)?;
                self.phase = if more && done + 1 < PLAYBACK_FRAMES { Phase::Playback(done + 1) } else { Phase::ThreeClips(0) };
            }
            Phase::ThreeClips(done) => {
                self.three_clip_frame()?;
                self.phase = if done + 1 == THREE_CLIP_FRAMES { Phase::VerifyAfterLoad } else { Phase::ThreeClips(done + 1) };
            }
            Phase::VerifyAfterLoad => {
                self.run_verification("blend_verification_after_load")?;
                self.phase = Phase::Finished;
            }
            Phase::Finished => {
                self.finish()?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

struct BenchApp {
    clips: Vec<PathBuf>,
    out: PathBuf,
    bench: Option<Bench>,
    error: Option<String>,
}

impl BenchApp {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        if let Some(bench) = self.bench.as_mut() {
            let _ = bench.drain_validation();
            if let Some(validation) = &bench.validation {
                println!("d3d12 validation at failure: {}", validation.summary());
            }
        }
        self.error = Some(message);
        event_loop.exit();
    }
}

impl ApplicationHandler for BenchApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.bench.is_some() {
            return;
        }
        match Bench::open(event_loop, &self.clips) {
            Ok(bench) => self.bench = Some(bench),
            Err(message) => self.fail(event_loop, message),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window: WindowId, event: WindowEvent) {
        let Some(bench) = self.bench.as_mut() else { return };
        bench.presenter.on_window_event(&event);
        match event {
            WindowEvent::CloseRequested => self.fail(event_loop, "the window was closed during the benchmark".to_owned()),
            WindowEvent::Resized(size) => bench.presenter.resize(&bench.gpu, size.width, size.height),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // winit can call this again after exit() was requested.
        if event_loop.exiting() {
            return;
        }
        let Some(bench) = self.bench.as_mut() else { return };
        let finished = match bench.step() {
            Ok(finished) => finished,
            Err(message) => return self.fail(event_loop, message),
        };
        if finished {
            let json_text = Value::Object(std::mem::take(&mut bench.results));
            let text = serde_json::to_string_pretty(&json_text).unwrap_or_default();
            match std::fs::write(&self.out, text) {
                Ok(()) => println!("results written to {}", self.out.display()),
                Err(e) => self.error = Some(format!("writing {} failed: {e}", self.out.display())),
            }
            event_loop.exit();
        }
    }
}

pub fn run(clips: Vec<PathBuf>, out: PathBuf) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("creating the event loop failed: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = BenchApp { clips, out, bench: None, error: None };
    event_loop.run_app(&mut app).map_err(|e| format!("the event loop failed: {e}"))?;
    app.error.map_or(Ok(()), Err)
}
