//! Runs `gpufx-render` as a child process and follows it: the temporary project, the job object
//! that takes the child down with the editor, the pipes, and stopping it.

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use d3d12_backend::{reserve_temporary, EncoderChoice, ExportJob, ProjectPaths, CANCEL_LINE, MAX_EVENT_LINE};
use engine::Project;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use super::machine::{is_own_partial, ChildMachine, ExportOutcome};
use crate::i18n::Message;

/// The renderer's file name, next to the editor in every layout.
const RENDER_EXE: &str = "gpufx-render.exe";
/// How long a child gets to act on a cancel line before it is killed.
pub const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// How long a child that closed stdout gets to exit.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long a pipe of a child that is gone or killed may stay open, held by something it started
/// or a process the system could not stop, before its reader is left behind.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// How often the cancel flag is looked at while the child is quiet.
const POLL: Duration = Duration::from_millis(20);
/// Longer lines of stdout are not events. Their bytes are dropped, not kept.
const MAX_LINE: usize = MAX_EVENT_LINE;
/// How much of stderr is kept for the error message.
const STDERR_TAIL: usize = 1024;

/// `gpufx-render.exe` beside the running editor. Whether it exists is checked when it is run.
pub fn sibling_render_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent().map_or_else(|| PathBuf::from(RENDER_EXE), |folder| folder.join(RENDER_EXE))
}

/// `project` with every source and package absolute, so it means the same from any folder.
/// A package that is not found stays as it is: only a hidden track can get here with one.
fn absolute_project(project: &Project, paths: &ProjectPaths) -> Project {
    let mut stored = project.clone();
    for track in &mut stored.tracks {
        for clip in &mut track.clips {
            let source = paths.source_path(&clip.source);
            clip.source = std::path::absolute(&source).unwrap_or(source).to_string_lossy().into_owned();
            for effect in &mut clip.effects {
                absolute_package(&mut effect.package, paths);
            }
        }
        for transition in &mut track.transitions {
            absolute_package(&mut transition.package, paths);
        }
    }
    stored
}

fn absolute_package(package: &mut String, paths: &ProjectPaths) {
    if let Some(found) = paths.package_path(package) {
        *package = std::path::absolute(&found).unwrap_or(found).to_string_lossy().into_owned();
    }
}

/// The project as a file of the system's temporary folder, under a name no file had. Removed on drop.
struct TemporaryProject(PathBuf);

impl TemporaryProject {
    fn write(project: &Project, paths: &ProjectPaths) -> Result<Self, String> {
        let text = serde_json::to_string_pretty(&absolute_project(project, paths)).map_err(|e| format!("serialising the project failed: {e}"))?;
        let target = std::env::temp_dir().join("gpufx-export.json");
        let path = reserve_temporary(&target).map_err(|e| format!("creating the temporary project failed: {e}"))?;
        let file = Self(path);
        std::fs::write(&file.0, text).map_err(|e| format!("writing {} failed: {e}", file.0.display()))?;
        Ok(file)
    }
}

impl Drop for TemporaryProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A job object that kills what is in it when its last handle closes, which the system also does
/// for a process that crashes.
pub(super) struct KillOnClose(HANDLE);

// A job handle may be used and closed from any thread.
unsafe impl Send for KillOnClose {}

impl KillOnClose {
    pub(super) fn new() -> Result<Self, String> {
        // SAFETY: both pointers are null or valid for the call; the handle is owned by the result.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!("creating the job object failed: {}", std::io::Error::last_os_error()));
        }
        let job = Self(handle);
        // SAFETY: an all-zero value is valid for this plain C struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the pointer and size describe `limits`, the class matches its type.
        let set = unsafe { SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, (&raw const limits).cast::<c_void>(), size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32) };
        if set == 0 {
            return Err(format!("configuring the job object failed: {}", std::io::Error::last_os_error()));
        }
        Ok(job)
    }

    pub(super) fn assign(&self, child: &Child) -> Result<(), String> {
        // SAFETY: both handles are alive for the call.
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) } == 0 {
            return Err(format!("adding the export process to its job failed: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

impl Drop for KillOnClose {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Calls `on_line` for each line of `reader`, without keeping more than [`MAX_LINE`] of one. An
/// over-long line is passed on cut short, so it counts as malformed.
fn pump_lines(reader: impl Read, mut on_line: impl FnMut(&[u8])) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut over = false;
    loop {
        let Ok(chunk) = reader.fill_buf() else { break };
        if chunk.is_empty() {
            break;
        }
        let (taken, ended) = chunk.iter().position(|&b| b == b'\n').map_or((chunk.len(), false), |at| (at + 1, true));
        let body = &chunk[..if ended { taken - 1 } else { taken }];
        let room = MAX_LINE.saturating_sub(line.len());
        over |= body.len() > room;
        line.extend_from_slice(&body[..body.len().min(room)]);
        reader.consume(taken);
        if ended {
            on_line(&line);
            line.clear();
            over = false;
        }
    }
    if !line.is_empty() || over {
        on_line(&line);
    }
}

/// Drains `reader`, returning the last [`STDERR_TAIL`] bytes as text.
fn tail_of(mut reader: impl Read) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(read) = reader.read(&mut chunk) {
        if read == 0 {
            break;
        }
        kept.extend_from_slice(&chunk[..read]);
        if kept.len() > STDERR_TAIL * 4 {
            kept.drain(..kept.len() - STDERR_TAIL);
        }
    }
    let start = kept.len().saturating_sub(STDERR_TAIL);
    String::from_utf8_lossy(&kept[start..]).into_owned()
}

enum Incoming {
    Line(Vec<u8>),
    Closed,
}

/// The child's exit code. It is killed after `grace`; one that still does not go is left to the
/// job object, with no code to report.
fn wait_for_exit(child: &mut Child, grace: Duration) -> Option<i32> {
    let mut started = Instant::now();
    let mut killed = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if started.elapsed() < if killed { PIPE_GRACE } else { grace } => thread::sleep(POLL),
            Ok(None) if !killed => {
                let _ = child.kill();
                killed = true;
                started = Instant::now();
            }
            _ => return None,
        }
    }
}

/// The thread's result if it ends within `limit`; otherwise it is left running on its own.
fn join_within<T>(handle: thread::JoinHandle<T>, limit: Duration) -> Option<T> {
    let started = Instant::now();
    while !handle.is_finished() {
        if started.elapsed() >= limit {
            return None;
        }
        thread::sleep(POLL);
    }
    handle.join().ok()
}

fn take_line(machine: &mut ChildMachine, done: &AtomicI64, line: &[u8]) {
    machine.take_line(line);
    done.store(machine.frames(), Ordering::SeqCst);
}

/// Follows a started child until it is gone and works out the outcome. The child's pipes are
/// read on threads of their own, so a full pipe never blocks it. `grace` is how long a child has
/// to act on a cancel line, and to exit after reporting its end, before it is killed.
fn supervise(mut child: Child, cancel: &AtomicBool, done: &AtomicI64, grace: Duration, pipe_grace: Duration, outputs: &[&Path]) -> ExportOutcome {
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let tail = thread::spawn(move || tail_of(stderr));
    let (sender, lines) = channel();
    let pump = thread::spawn(move || {
        pump_lines(stdout, |line| {
            let _ = sender.send(Incoming::Line(line.to_vec()));
        });
        let _ = sender.send(Incoming::Closed);
    });

    let mut machine = ChildMachine::default();
    let mut cancel_sent: Option<Instant> = None;
    let mut ended_at: Option<Instant> = None;
    // When the child was seen gone or was killed: its stdout should close right after.
    let mut gone_at: Option<Instant> = None;
    loop {
        match lines.recv_timeout(POLL) {
            Ok(Incoming::Line(line)) => take_line(&mut machine, done, &line),
            Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if cancel.load(Ordering::SeqCst) && cancel_sent.is_none() {
            cancel_sent = Some(Instant::now());
            if let Some(pipe) = &mut stdin {
                // A child already gone cannot read it; the end of stdout then ends this loop.
                let _ = writeln!(pipe, "{CANCEL_LINE}").and_then(|()| pipe.flush());
            }
        }
        if machine.ended() && ended_at.is_none() {
            ended_at = Some(Instant::now());
        }
        if gone_at.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            gone_at = Some(Instant::now());
        }
        let overdue = |since: Option<Instant>| since.is_some_and(|at| at.elapsed() >= grace);
        if gone_at.is_none() && (machine.mismatched() || overdue(cancel_sent) || overdue(ended_at)) {
            let _ = child.kill();
            gone_at = Some(Instant::now());
        }
        if gone_at.is_some_and(|at| at.elapsed() >= pipe_grace) {
            break;
        }
    }
    drop(stdin);
    // A child that cannot be killed has no exit code here, and may keep its output file and its
    // VRAM until the job object ends it when the editor exits.
    let code = wait_for_exit(&mut child, EXIT_GRACE);
    let _ = join_within(pump, PIPE_GRACE);
    // Lines the pump queued while the loop ended or the child was waited for are still unread, and
    // an `error` line among them may name the only copy of the render.
    while let Ok(Incoming::Line(line)) = lines.try_recv() {
        take_line(&mut machine, done, &line);
    }
    let stderr_tail = join_within(tail, PIPE_GRACE).unwrap_or_default();
    let cancelled = cancel_sent.is_some() || cancel.load(Ordering::SeqCst);
    let (outcome, partials) = machine.finish(cancelled, code, &stderr_tail);
    // Removing a file that a child which could not be killed still holds open may fail, which
    // leaves the file behind.
    for partial in partials.iter().filter(|partial| is_own_partial(partial, outputs)) {
        let _ = std::fs::remove_file(partial);
    }
    outcome
}

/// The arguments of `gpufx-render` for this job.
fn render_arguments(project_file: &Path, job: &ExportJob, package_roots: &[PathBuf], headless: bool) -> Vec<std::ffi::OsString> {
    let mut arguments: Vec<std::ffi::OsString> = vec![project_file.into(), "--out".into(), job.out.clone().into(), "--encoder".into()];
    arguments.push(match &job.encoder {
        EncoderChoice::Auto => "auto".into(),
        EncoderChoice::Named(name) => name.into(),
    });
    if let Some(hashes) = &job.hashes {
        arguments.extend(["--hashes".into(), hashes.clone().into()]);
    }
    for root in package_roots {
        arguments.extend(["--packages".into(), root.clone().into()]);
    }
    arguments.extend(["--progress".into(), "json".into()]);
    if headless {
        arguments.push("--cancel-on-stdin-eof".into());
    }
    arguments
}

/// Exports `job` through the renderer at `tool`.
pub fn run_child(job: &ExportJob, package_roots: &[PathBuf], tool: &Path, cancel: &AtomicBool, done: &AtomicI64, grace: Duration) -> ExportOutcome {
    run_child_with(job, package_roots, tool, cancel, done, grace, PIPE_GRACE, false)
}

/// [`run_child`] for a server with no window. The child stops itself cleanly when stdin closes, which
/// is what happens if the server dies, so it is not put in a job that would kill it first and leave
/// its partial file behind.
pub fn run_child_headless(job: &ExportJob, package_roots: &[PathBuf], tool: &Path, cancel: &AtomicBool, done: &AtomicI64, grace: Duration) -> ExportOutcome {
    run_child_with(job, package_roots, tool, cancel, done, grace, PIPE_GRACE, true)
}

/// [`run_child`] with the time a gone child's stdout may stay open before it is no longer read.
fn run_child_with(job: &ExportJob, package_roots: &[PathBuf], tool: &Path, cancel: &AtomicBool, done: &AtomicI64, grace: Duration, pipe_grace: Duration, headless: bool) -> ExportOutcome {
    let failed = |message: String| ExportOutcome::Failed(Message::ExportFailed(message));
    // The project file the child gets is a temporary one, so this check against the real one is here.
    if let Err(message) = job.check() {
        return failed(message);
    }
    if cancel.load(Ordering::SeqCst) {
        return ExportOutcome::Cancelled;
    }
    if !tool.is_file() {
        return ExportOutcome::Failed(Message::ExportToolMissing(tool.display().to_string()));
    }
    let project = match TemporaryProject::write(&job.project, &job.paths) {
        Ok(project) => project,
        Err(message) => return failed(message),
    };
    let job_object = match (!headless).then(KillOnClose::new).transpose() {
        Ok(job_object) => job_object,
        Err(message) => return failed(message),
    };
    let spawned = Command::new(tool).args(render_arguments(&project.0, job, package_roots, headless)).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return failed(format!("starting {} failed: {error}", tool.display())),
    };
    if let Some(Err(message)) = job_object.as_ref().map(|job_object| job_object.assign(&child)) {
        let _ = child.kill();
        let _ = child.wait();
        return failed(message);
    }
    let mut outputs = vec![job.out.as_path()];
    outputs.extend(job.hashes.as_deref());
    supervise(child, cancel, done, grace, pipe_grace, &outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use d3d12_backend::ExportJob;
    use engine::{Clip, Effect, Output, Time, Track, TrackKind, SUPPORTED_VERSION};
    use serde_json::Map;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::time::Duration;

    const SHORT_GRACE: Duration = Duration::from_millis(300);

    fn fake_tool() -> PathBuf {
        // Only a `[[test]]` target with a hashed name: the newest one in the test executable's folder.
        let exe = std::env::current_exe().expect("the test executable");
        let folder = exe.parent().expect("target/<profile>/deps");
        let built = std::fs::read_dir(folder).expect("list deps").flatten().map(|entry| entry.path());
        let fakes = built.filter(|path| path.extension().is_some_and(|e| e == "exe") && path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("fake_render-")));
        fakes.max_by_key(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok()).expect("the fake_render test target is built by cargo test")
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-child-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the folder");
        dir
    }

    fn project(source: &str, package: &str) -> Project {
        let clip = Clip { source: source.into(), start: Time::ZERO, source_in: Time::ZERO, duration: Time::from_ticks(9000), effects: vec![Effect { package: package.into(), params: Map::new() }], gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None };
        Project { version: SUPPORTED_VERSION, output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: None }, tracks: vec![Track { kind: TrackKind::Video, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: true }] }
    }

    fn job(dir: &Path, mode: &str) -> ExportJob {
        // The track is hidden, so its missing source and package pass validation.
        ExportJob { project: project("clip.mp4", "fx"), project_file: Some(dir.join("project.json")), paths: Arc::new(ProjectPaths::new(&dir.join("project.json"), Vec::new())), out: dir.join(format!("{mode}.mp4")), encoder: EncoderChoice::Named("libx264".into()), hashes: None }
    }

    fn run(job: &ExportJob, cancel: bool, grace: Duration) -> ExportOutcome {
        run_child(job, &[PathBuf::from(r"D:\roots\one")], &fake_tool(), &AtomicBool::new(cancel), &AtomicI64::new(0), grace)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).expect("list").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_missing_tool_is_reported_by_name() {
        let dir = scratch("missing");
        let tool = dir.join("gpufx-render.exe");
        let outcome = run_child(&job(&dir, "ok"), &[], &tool, &AtomicBool::new(false), &AtomicI64::new(0), SHORT_GRACE);
        assert_eq!(outcome, ExportOutcome::Failed(Message::ExportToolMissing(tool.display().to_string())));
    }

    #[test]
    fn the_output_may_still_not_overwrite_the_real_project_file() {
        let dir = scratch("own-project");
        let mut own = job(&dir, "ok");
        own.out = dir.join("project.json");
        std::fs::write(&own.out, b"{}").expect("write");
        let outcome = run(&own, false, SHORT_GRACE);
        assert!(matches!(&outcome, ExportOutcome::Failed(Message::ExportFailed(text)) if text.contains("the project file")), "{outcome:?}");
        assert_eq!(std::fs::read(&own.out).expect("read"), b"{}");
    }

    #[test]
    fn the_child_gets_the_flags_and_an_absolute_copy_of_the_project_that_is_removed_after() {
        let dir = scratch("flags");
        let job = job(&dir, "ok");
        assert!(matches!(run(&job, false, SHORT_GRACE), ExportOutcome::Finished(_)));
        let arguments = std::fs::read_to_string(dir.join("ok.args.txt")).expect("the fake wrote its arguments");
        let lines: Vec<&str> = arguments.lines().collect();
        assert_eq!(lines[1..], ["--out", dir.join("ok.mp4").to_str().unwrap(), "--encoder", "libx264", "--packages", r"D:\roots\one", "--progress", "json"]);
        assert!(!Path::new(lines[0]).exists(), "the temporary project is gone: {}", lines[0]);
        assert!(lines[0].contains("gpufx-export") && lines[0].ends_with(".json"));
        let copied: Project = serde_json::from_str(&std::fs::read_to_string(dir.join("ok.project.json")).expect("the fake kept a copy")).expect("a project");
        let clip = &copied.tracks[0].clips[0];
        assert_eq!(clip.source, dir.join("clip.mp4").to_string_lossy(), "same meaning as beside the project file");
        assert!(copied.tracks[0].hidden, "hidden tracks stay hidden");
    }

    #[test]
    fn a_headless_child_is_told_to_stop_when_stdin_closes() {
        let dir = scratch("headless");
        let job = job(&dir, "ok");
        assert!(matches!(run_child_headless(&job, &[], &fake_tool(), &AtomicBool::new(false), &AtomicI64::new(0), SHORT_GRACE), ExportOutcome::Finished(_)));
        let arguments = std::fs::read_to_string(dir.join("ok.args.txt")).expect("the fake wrote its arguments");
        assert!(arguments.lines().any(|line| line == "--cancel-on-stdin-eof"), "{arguments}");
    }

    #[test]
    fn a_package_beside_the_project_keeps_resolving() {
        let dir = scratch("package");
        std::fs::create_dir_all(dir.join("fx")).expect("create");
        std::fs::write(dir.join("fx").join("manifest.json"), b"{}").expect("write");
        let paths = ProjectPaths::new(&dir.join("project.json"), Vec::new());
        let stored = absolute_project(&project("clip.mp4", "fx"), &paths);
        assert_eq!(stored.tracks[0].clips[0].effects[0].package, dir.join("fx").to_string_lossy());
        assert_eq!(paths.package_path(&stored.tracks[0].clips[0].effects[0].package), Some(dir.join("fx")));
        let unknown = absolute_project(&project("clip.mp4", "absent"), &paths);
        assert_eq!(unknown.tracks[0].clips[0].effects[0].package, "absent");
    }

    #[test]
    fn a_temporary_project_never_takes_an_existing_name() {
        let paths = ProjectPaths::new(Path::new("p.json"), Vec::new());
        let (first, second) = (TemporaryProject::write(&project("a", "b"), &paths).expect("write"), TemporaryProject::write(&project("a", "b"), &paths).expect("write"));
        assert_ne!(first.0, second.0);
        assert!(first.0.exists() && second.0.exists());
        // In a folder of its own: another test of this process may take a freed name at once.
        let own = scratch("temporary-project").join("project.json");
        std::fs::write(&own, b"{}").expect("write");
        drop(TemporaryProject(own.clone()));
        assert!(!own.exists());
    }

    #[test]
    fn malformed_output_does_not_stop_the_export() {
        let dir = scratch("malformed");
        assert!(matches!(run(&job(&dir, "malformed"), false, SHORT_GRACE), ExportOutcome::Finished(_)));
    }

    #[test]
    fn a_crash_reports_the_stderr_and_removes_the_partial_file() {
        let dir = scratch("crash");
        let outcome = run(&job(&dir, "crash"), false, SHORT_GRACE);
        match outcome {
            ExportOutcome::Failed(Message::ExportChildCrashed { code, stderr }) => {
                assert_eq!(code, "0xC0000005");
                assert!(stderr.contains("the device went away"), "{stderr}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(names(&dir), ["crash.args.txt", "crash.project.json"], "the partial file is gone");
    }

    #[test]
    fn an_exit_without_a_result_is_a_failure() {
        let dir = scratch("silent");
        assert!(matches!(run(&job(&dir, "silent"), false, SHORT_GRACE), ExportOutcome::Failed(Message::ExportChildCrashed { .. })));
    }

    #[test]
    fn the_childs_own_error_is_reported_and_its_file_kept() {
        let dir = scratch("error");
        let outcome = run(&job(&dir, "error"), false, SHORT_GRACE);
        assert_eq!(outcome, ExportOutcome::Failed(Message::ExportFailed("kept as error.1-0.tmp.mp4".into())));
        assert!(dir.join("error.1-0.tmp.mp4").is_file(), "that file may be the only copy");
    }

    #[test]
    fn an_error_still_queued_when_the_loop_ends_is_not_lost() {
        let dir = scratch("backlog");
        let job = job(&dir, "backlog");
        // No wait for the pipe, and a long run of lines ahead of the error: the loop ends with the
        // error still in the channel.
        let outcome = run_child_with(&job, &[], &fake_tool(), &AtomicBool::new(false), &AtomicI64::new(0), SHORT_GRACE, Duration::ZERO, false);
        assert_eq!(outcome, ExportOutcome::Failed(Message::ExportFailed("kept as backlog.1-0.tmp.mp4".into())));
        assert!(dir.join("backlog.1-0.tmp.mp4").is_file(), "that file may be the only copy");
    }

    #[test]
    fn a_crash_after_finished_still_reports_the_published_file() {
        let dir = scratch("endcrash");
        let job = job(&dir, "endcrash");
        assert!(matches!(run(&job, false, SHORT_GRACE), ExportOutcome::Finished(_)));
        assert!(job.out.is_file(), "the destination was replaced");
    }

    #[test]
    fn a_crash_after_an_error_keeps_the_only_copy_of_the_render() {
        let dir = scratch("errorcrash");
        let job = job(&dir, "errorcrash");
        assert_eq!(run(&job, false, SHORT_GRACE), ExportOutcome::Failed(Message::ExportFailed("kept as errorcrash.1-0.tmp.mp4".into())));
        assert!(dir.join("errorcrash.1-0.tmp.mp4").is_file());
    }

    #[test]
    fn a_huge_stderr_neither_blocks_the_child_nor_is_kept() {
        let dir = scratch("flood");
        assert!(matches!(run(&job(&dir, "flood"), false, SHORT_GRACE), ExportOutcome::Finished(_)), "the child wrote 32 MB to stderr");
        let dir = scratch("floodcrash");
        match run(&job(&dir, "floodcrash"), false, SHORT_GRACE) {
            ExportOutcome::Failed(Message::ExportChildCrashed { stderr, .. }) => {
                assert!(stderr.len() <= STDERR_TAIL, "{}", stderr.len());
                assert!(stderr.ends_with("last words"), "the tail is the end");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_huge_stdout_line_is_bounded_and_the_next_line_still_counts() {
        let mut input = vec![b'x'; MAX_LINE * 3];
        input.extend_from_slice(b"\nshort\nlast");
        let mut seen: Vec<usize> = Vec::new();
        pump_lines(&input[..], |line| seen.push(line.len()));
        assert_eq!(seen, [MAX_LINE, 5, 4]);
    }

    #[test]
    fn cancel_stops_the_child_cleanly() {
        let dir = scratch("cancel");
        let job = job(&dir, "cancel");
        let cancel = AtomicBool::new(false);
        let outcome = thread::scope(|scope| {
            let running = scope.spawn(|| run_child(&job, &[], &fake_tool(), &cancel, &AtomicI64::new(0), Duration::from_secs(30)));
            thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::SeqCst);
            running.join().expect("the run ended")
        });
        assert_eq!(outcome, ExportOutcome::Cancelled);
        assert_eq!(names(&dir), ["cancel.args.txt", "cancel.project.json", "cancel.stopped"], "the child removed its partial file and said so");
    }

    #[test]
    fn a_child_that_ignores_cancel_is_killed_and_its_partial_file_removed() {
        let dir = scratch("hang");
        let job = job(&dir, "hang");
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let outcome = thread::scope(|scope| {
            let running = scope.spawn(|| run_child(&job, &[], &fake_tool(), &cancel, &AtomicI64::new(0), SHORT_GRACE));
            thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::SeqCst);
            running.join().expect("the run ended")
        });
        assert_eq!(outcome, ExportOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(names(&dir), ["hang.args.txt", "hang.project.json"], "the partial file is gone");
    }

    #[test]
    fn closing_the_job_kills_the_child() {
        let dir = scratch("job");
        let job_object = KillOnClose::new().expect("create the job");
        let mut child = Command::new(fake_tool()).args(["project.json", "--out"]).arg(dir.join("hang.mp4")).args(["--progress", "json"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("start");
        job_object.assign(&child).expect("assign");
        assert!(child.try_wait().expect("poll").is_none(), "the child waits forever");
        drop(job_object);
        let started = Instant::now();
        while child.try_wait().expect("poll").is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "the child outlived its job");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_renderer_of_another_protocol_is_stopped_and_refused() {
        let dir = scratch("stale");
        let job = job(&dir, "stale");
        assert_eq!(within(Duration::from_secs(20), move || run(&job, false, SHORT_GRACE)), ExportOutcome::Failed(Message::ExportRendererMismatch));
    }

    /// `work` on a thread, failing the test rather than hanging it.
    fn within<T: Send + 'static>(limit: Duration, work: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = channel();
        thread::spawn(move || sender.send(work()));
        receiver.recv_timeout(limit).expect("the export ended in time")
    }

    #[test]
    fn a_child_that_hangs_after_its_end_is_killed_and_the_end_stands() {
        let dir = scratch("endhang");
        let job = job(&dir, "endhang");
        let out = job.out.clone();
        assert!(matches!(within(Duration::from_secs(20), move || run(&job, false, SHORT_GRACE)), ExportOutcome::Finished(_)));
        assert!(out.is_file());
    }

    #[test]
    fn a_pipe_held_open_by_another_process_does_not_stop_the_wait() {
        let dir = scratch("heldpipe");
        let job = job(&dir, "heldpipe");
        assert!(matches!(within(Duration::from_secs(20), move || run(&job, false, SHORT_GRACE)), ExportOutcome::Finished(_)));
    }

    #[test]
    fn dropping_the_worker_does_not_wait_for_a_stuck_child() {
        let dir = scratch("dropwait");
        let job = job(&dir, "hang");
        let (released, gate) = channel();
        released.send(()).expect("the receiver is alive");
        let worker = super::super::ExportWorker::start(job, Vec::new(), fake_tool(), gate, || {}).expect("start");
        thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        drop(worker);
        assert!(started.elapsed() < CANCEL_GRACE, "the UI thread waited {:?}", started.elapsed());
    }

    #[test]
    fn a_cancel_before_the_start_runs_nothing() {
        let dir = scratch("early");
        assert_eq!(run(&job(&dir, "ok"), true, SHORT_GRACE), ExportOutcome::Cancelled);
        assert!(names(&dir).is_empty());
    }
}
