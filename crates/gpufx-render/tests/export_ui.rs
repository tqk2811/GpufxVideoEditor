//! The editor's export through its child process against a direct CLI run: same project, same frames and PCM. GPU cases skip
//! on a machine without a hardware adapter, or fail if `GPUFX_REQUIRE_GPU` is set.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use d3d12_backend::{probe_media, EncoderChoice, ExportControl, ExportError, ExportJob, ExportReport, Gpu};
use engine::{AudioOutput, Clip, Output, Project, Time, Track, TrackKind, Transition, SUPPORTED_VERSION};
use gpufx_editor::editor::{EditorState, Status};
use gpufx_editor::export::{ExportOutcome, ExportWorker};
use gpufx_editor::project_file::{load_project_file, package_roots, project_paths, save_project_file};
use serde_json::Map;
use sha2::{Digest, Sha256};

const TIMEOUT: Duration = Duration::from_secs(120);

fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// The hardware device, or `None` after reporting a skip.
fn hardware_gpu() -> Option<Arc<Gpu>> {
    let gpu = Gpu::create_hardware().expect("open the GPU");
    if gpu.is_none() {
        assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "GPUFX_REQUIRE_GPU is set, but there is no hardware adapter");
        eprintln!("export_ui: skipped: no hardware adapter");
    }
    gpu.map(Arc::new)
}

fn clip(path: &Path, start: i64, duration: i64) -> Clip {
    Clip { source: path.to_string_lossy().into_owned(), start: Time::from_ticks(start), source_in: Time::ZERO, duration: Time::from_ticks(duration), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0 }
}

fn project_with_transition_and_sound() -> Project {
    let (first, second) = (tone_clip_with_video(), flash_and_beep_clip());
    let clips = vec![clip(&first, 0, 45 * FRAME), clip(&second, 45 * FRAME, 45 * FRAME)];
    let transitions = vec![Transition { after_clip: 0, package: "packages/gpufx/capcut/mix".to_owned(), duration: Time::from_ticks(15 * FRAME), params: Map::new() }];
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: CLIP_WIDTH, height: CLIP_HEIGHT, frame_duration: Time::from_ticks(FRAME), audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 192_000 }) },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions, locked: false, hidden: false }],
    }
}

fn export_job(file: &Path, out: &Path, hashes: Option<&Path>) -> ExportJob {
    let roots = package_roots();
    let project = load_project_file(file, &roots).expect("the saved project opens");
    ExportJob { project, project_file: Some(file.to_path_buf()), paths: Arc::new(project_paths(Some(file), &roots)), out: out.to_path_buf(), encoder: EncoderChoice::Auto, hashes: hashes.map(Path::to_path_buf) }
}

/// Starts the UI's worker on `file`, already past the wait for the preview.
fn start_export(file: &Path, out: &Path, hashes: Option<&Path>) -> ExportWorker {
    let job = export_job(file, out, hashes);
    let (released, gate) = channel();
    released.send(()).expect("the receiver is alive");
    ExportWorker::start(job, package_roots(), PathBuf::from(env!("CARGO_BIN_EXE_gpufx-render")), gate, || {}).expect("the export starts")
}

fn wait(worker: &mut ExportWorker) -> ExportOutcome {
    let started = Instant::now();
    loop {
        if let Some(outcome) = worker.poll() {
            return outcome;
        }
        assert!(started.elapsed() < TIMEOUT, "the export did not end");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn decoded_pcm(file: &Path) -> Vec<u8> {
    let output = Command::new(ffmpeg_tool()).args(["-hide_banner", "-v", "error", "-i"]).arg(file).args(["-map", "a:0", "-f", "f32le", "-"]).output().expect("run ffmpeg");
    assert!(output.status.success(), "decoding failed: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

#[test]
fn the_ui_export_matches_the_cli_in_frames_and_pcm() {
    let _lock = gpu_lock();
    if hardware_gpu().is_none() {
        return;
    }
    let dir = scratch("export-matches-cli");
    let file = dir.join("project.json");
    save_project_file(&project_with_transition_and_sound(), &file).expect("save the project");

    let (cli_out, cli_hashes) = (dir.join("cli.mp4"), dir.join("cli.txt"));
    let cli = Command::new(env!("CARGO_BIN_EXE_gpufx-render")).arg(&file).arg("--out").arg(&cli_out).arg("--hashes").arg(&cli_hashes).output().expect("run gpufx-render");
    assert!(cli.status.success(), "render failed: {}", String::from_utf8_lossy(&cli.stderr));
    let printed = String::from_utf8_lossy(&cli.stdout).into_owned();
    let cli_pcm = printed.lines().find_map(|line| line.strip_prefix("pcm sha256: ")).expect("the CLI reports the PCM hash");

    // A folder name outside ASCII, which must survive the command line and the protocol.
    let folder = dir.join("xuất thử");
    std::fs::create_dir_all(&folder).expect("create the folder");
    let (ui_out, ui_hashes) = (folder.join("ui.mp4"), folder.join("ui.txt"));
    let mut worker = start_export(&file, &ui_out, Some(&ui_hashes));
    let report = match wait(&mut worker) {
        ExportOutcome::Finished(report) => report,
        other => panic!("the export did not finish: {other:?}"),
    };

    let cli_text = std::fs::read_to_string(&cli_hashes).expect("read the CLI hashes");
    assert_eq!(cli_text.lines().count(), 90);
    assert_eq!(cli_text, std::fs::read_to_string(&ui_hashes).expect("read the UI hashes"), "frame hashes differ");
    assert_eq!(report.pcm_sha256.as_deref(), Some(cli_pcm), "the mixed PCM differs");

    let (cli_pcm, ui_pcm) = (decoded_pcm(&cli_out), decoded_pcm(&ui_out));
    assert!(cli_pcm.chunks_exact(4).any(|sample| f32::from_le_bytes(sample.try_into().expect("four bytes")).abs() > 0.01), "the sound must not be silence");
    assert_eq!(Sha256::digest(&cli_pcm), Sha256::digest(&ui_pcm), "the encoded audio differs");
}

#[test]
fn the_outcome_is_settled_and_reported_before_the_gpu_is_released() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let dir = scratch("export-settled-before-teardown");
    let file = dir.join("project.json");
    save_project_file(&project_with_transition_and_sound(), &file).expect("save the project");
    let out = dir.join("settled.mp4");
    let job = export_job(&file, &out, None);
    let seen = std::cell::Cell::new(None);
    let settled = |result: &Result<ExportReport, String>| seen.set(Some((Arc::strong_count(&gpu), result.is_ok(), out.is_file())));
    job.run(gpu.clone(), ExportControl { settled: Some(&settled), ..ExportControl::default() }).expect("the export succeeds");
    let (holders, succeeded, published) = seen.get().expect("the outcome was reported");
    assert!(succeeded && published, "the file was at its target when it was reported");
    // This test, `run`, `render`, and the compositor that is torn down last.
    assert!(holders >= 4, "the compositor had already released the GPU: {holders} holders");
}

/// A validation error that no error scope captures: a buffer cannot map both ways.
fn raise_wgpu_error(gpu: &Gpu) {
    let _ = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::MAP_WRITE, mapped_at_creation: false });
}

#[test]
fn a_failure_before_the_rename_is_reported_once_before_the_gpu_is_released() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let dir = scratch("export-failure-before-teardown");
    let file = dir.join("project.json");
    save_project_file(&project_with_transition_and_sound(), &file).expect("save the project");
    let out = dir.join("failed.mp4");
    let job = export_job(&file, &out, None);
    let raiser = gpu.clone();
    let progress = move |done: i64, total: i64| {
        if done == total {
            raise_wgpu_error(&raiser);
        }
    };
    let seen = std::cell::RefCell::new(Vec::new());
    let settled = |result: &Result<ExportReport, String>| seen.borrow_mut().push((Arc::strong_count(&gpu), result.clone().err()));
    let result = job.run(gpu.clone(), ExportControl { progress: Some(&progress), settled: Some(&settled), ..ExportControl::default() });
    let Err(ExportError::Failed(message)) = result else { panic!("the export must fail: {result:?}") };
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 1, "exactly one final event: {seen:?}");
    assert_eq!(seen[0].1.as_deref(), Some(message.as_str()), "the reason that was returned");
    // The progress closure's clone, this test, `run`, `render`, and the compositor that is torn down last.
    assert!(seen[0].0 >= 5, "the compositor had already released the GPU: {} holders", seen[0].0);
    assert!(!out.exists(), "a failed export publishes nothing");
}

#[test]
fn a_wgpu_error_belongs_to_the_thread_that_raised_it() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let other = gpu.clone();
    std::thread::spawn(move || {
        raise_wgpu_error(&other);
        assert!(other.check_errors().is_err(), "the raising thread sees it");
        assert!(other.take_error().is_some());
        raise_wgpu_error(&other);
    })
    .join()
    .expect("the thread finished");
    assert!(gpu.check_errors().is_ok(), "another thread's error is not this thread's");
    assert_eq!(gpu.take_error(), None);
}

#[test]
fn a_wgpu_error_from_before_the_export_does_not_fail_it() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let dir = scratch("export-error-before");
    let file = dir.join("project.json");
    save_project_file(&project_with_transition_and_sound(), &file).expect("save the project");
    raise_wgpu_error(&gpu);
    export_job(&file, &dir.join("out.mp4"), None).run(gpu, ExportControl::default()).expect("an earlier error is not the export's");
}

#[test]
fn a_wgpu_error_after_the_last_frame_is_the_exports_and_is_taken() {
    let _lock = gpu_lock();
    let Some(gpu) = hardware_gpu() else { return };
    let dir = scratch("export-error-after");
    let file = dir.join("project.json");
    save_project_file(&project_with_transition_and_sound(), &file).expect("save the project");
    let out = dir.join("out.mp4");
    let raiser = gpu.clone();
    let progress = move |done: i64, total: i64| {
        if done == total {
            raise_wgpu_error(&raiser);
        }
    };
    let result = export_job(&file, &out, None).run(gpu.clone(), ExportControl { progress: Some(&progress), ..Default::default() });
    match result {
        Err(ExportError::Failed(message)) => assert!(message.contains("wgpu error"), "{message}"),
        other => panic!("the export must report it: {other:?}"),
    }
    assert_eq!(gpu.take_error(), None, "the next frame is not blamed");
    assert!(!out.exists(), "a failed export publishes nothing");
}

#[test]
fn a_real_file_without_sound_cannot_be_added_to_an_audio_track() {
    let (video_only, _) = test_clips();
    let dir = scratch("audio-track-refusal");
    let file = dir.join("project.json");
    let mut state = EditorState::default();
    state.add_track(TrackKind::Audio);
    state.add_probed_file(0, &video_only, &probe_media(&video_only).expect("probe the clip"));
    assert!(state.project().tracks[0].clips.is_empty(), "{:?}", state.status());
    assert!(matches!(state.status(), Status::Refused(_)));
    let with_sound = tone_clip_with_video();
    state.add_probed_file(0, &with_sound, &probe_media(&with_sound).expect("probe the clip"));
    assert_eq!(state.project().tracks[0].clips.len(), 1, "{:?}", state.status());
    save_project_file(state.project(), &file).expect("save the project");
    load_project_file(&file, &package_roots()).expect("what the UI let through opens again");
}

#[test]
fn a_cancelled_export_keeps_the_old_file_and_removes_the_partial_one() {
    let _lock = gpu_lock();
    if hardware_gpu().is_none() {
        return;
    }
    let dir = scratch("export-cancel");
    let file = dir.join("project.json");
    save_project_file(&project_with_transition_and_sound(), &file).expect("save the project");
    let out = dir.join("out.mp4");
    std::fs::write(&out, b"earlier").expect("write the earlier file");
    let bystander = dir.join("out.tmp.mp4");
    std::fs::write(&bystander, b"user video").expect("write the file");

    let mut worker = start_export(&file, &out, None);
    let started = Instant::now();
    while worker.progress().done < 5 {
        assert!(started.elapsed() < TIMEOUT, "the export did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.cancel();

    assert_eq!(wait(&mut worker), ExportOutcome::Cancelled);
    assert_eq!(std::fs::read(&out).expect("read the earlier file"), b"earlier");
    assert_eq!(std::fs::read(&bystander).expect("read the file"), b"user video", "a file this run did not create stays");
    let leftovers: Vec<_> = std::fs::read_dir(&dir).expect("list the directory").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".tmp") && n != "out.tmp.mp4").collect();
    assert!(leftovers.is_empty(), "the partial file is gone: {leftovers:?}");
}
