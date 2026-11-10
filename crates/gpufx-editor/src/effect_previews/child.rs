//! The editor's side of `gpufx-render --effect-previews`: one helper process at a time, one job at a
//! time, and the rules for what a failure means. The lines are the types of
//! `d3d12_backend::{PreviewJob, PreviewEvent}`; the helper documents the protocol.
//!
//! [`PreviewSession::render`] blocks until the job is over, so a worker thread drives it. The
//! helper is behind the [`Helper`] and [`Launcher`] traits, so the rules need no process to test.
//!
//! What a failure means. The aim is that a healthy package is never marked failed for good because
//! the GPU was busy or something passed:
//! - An attempt record `.attempt` (`{count, adapter, version, at}`) counts as absent under another
//!   adapter or version, when it cannot be read, and after 7 days. Only a second occurrence for
//!   the same key writes `.failed`, and it counts only when the first was recorded before the
//!   current helper started: that is a retry on a new helper, not another editor failing at the
//!   same moment on the same store. After a package failure the helper is replaced on purpose so
//!   that the second one is never on the helper that saw the first.
//! - `blame: package` records an attempt. A timeout or a crash after the job was delivered does so
//!   only when a control job shows the machine was healthy right after it: a fresh helper renders
//!   a still of [`SessionConfig::control_package`] to a temporary file (never published) within
//!   [`SessionConfig::control_timeout`]. If the control fails, times out, has no package, or its
//!   helper cannot start, the failure is transient. A failed control counts against no key, and
//!   never leads to another control. A job that could not be sent blames nothing, and neither does
//!   a helper found dead before a job.
//! - An attempt that could not be written, or counted as no second occurrence, and a `.failed` that
//!   could not be written, are transient failures too, so they are bounded.
//! - `blame: device`, `blame: output` and the unproven timeouts and crashes never write
//!   `.failed`. A key that gets them [`MAX_TRANSIENT`] times is not tried again by this session
//!   (kept in memory only).
//! - [`MAX_DEATHS`] helpers in a row that die, or fail to start, or [`MAX_DEVICE_FAILURES`] jobs in
//!   a row that fail on the device, put the session in a cooldown
//!   ([`SessionConfig::cooldown`], doubled for each cooldown in a row, at most [`MAX_COOLDOWN`]):
//!   [`PreviewSession::render`] starts nothing and answers [`RenderOutcome::CoolingDown`] until it
//!   is over, and then the counts start again. A job that goes well resets the counts and the
//!   doubling. A helper of another protocol stops the session for good.
//! - A `.failed` record holds the adapter and the app version it was seen under and counts only
//!   under the same ones, so a different GPU or editor build tries again. The adapter identity has
//!   no driver version, so a new driver alone does not.
//!
//! Dropping a [`PreviewSession`] ends the helper and can block for several seconds, so it must not
//! happen on the UI thread.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use d3d12_backend::{PreviewBlame, PreviewEvent, PreviewJob, PreviewJobKind, MAX_EVENT_LINE, PREVIEW_PROTOCOL};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use super::{PreviewSpec, PreviewStore};
use crate::child_process::{join_within, pump_lines, tail_of, KillOnClose, POLL};
use crate::export::sibling_render_exe;

/// Helpers in a row that may die or fail to start before the session cools down.
pub const MAX_DEATHS: u32 = 5;
/// Jobs in a row, of any keys, that may fail with `blame: device` before the session cools down.
pub const MAX_DEVICE_FAILURES: u32 = 8;
/// Failures without a verdict on the package that one key may have before this session stops trying it.
pub const MAX_TRANSIENT: u32 = 3;
/// The longest a cooldown gets once it has doubled.
pub const MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// How many lines are read to find out whether an idle helper is gone.
const PROBE_LINES: u32 = 64;
/// How many lines are read after the deadline of a job, to find a `done` that is already there.
const DEADLINE_LINES: u32 = 64;
/// How long the wait for a line is cut into, so a stop request is seen soon.
const SLICE: Duration = Duration::from_millis(50);
/// How long a killed helper gets to be gone, and its reader threads to end.
const KILL_WAIT: Duration = Duration::from_secs(2);

/// What the session reads from the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Event(PreviewEvent),
    /// A line that is not an event.
    Malformed,
    /// Nothing came within the wait.
    Timeout,
    /// Stdout ended: the helper is gone.
    Closed,
}

/// A started helper.
pub trait Helper: Send {
    /// Writes one line (without its line end) to the helper. An error means it was not delivered.
    fn send(&mut self, line: &str) -> std::io::Result<()>;
    fn recv(&mut self, wait: Duration) -> Incoming;
    /// Ends the helper and everything it started.
    fn kill(&mut self);
    /// Closes its stdin, which ends a helper that is idle, and kills it if it is still there after `grace`.
    fn close_and_wait(&mut self, grace: Duration);
    /// What the helper wrote to stderr last, for a message about its death.
    fn diagnostics(&mut self) -> String {
        String::new()
    }
}

/// Starts helpers.
pub trait Launcher: Send {
    fn start(&mut self) -> Result<Box<dyn Helper>, String>;
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// How long a still may take before the helper is killed.
    pub still_timeout: Duration,
    /// How long a loop may take before the helper is killed.
    pub loop_timeout: Duration,
    /// How long a new helper has to say `ready`.
    pub ready_timeout: Duration,
    /// How long a helper that was told to stop has before it is killed.
    pub shutdown_grace: Duration,
    /// How long the first cooldown lasts, after too many deaths or device failures; each cooldown
    /// in a row doubles it, up to [`MAX_COOLDOWN`].
    pub cooldown: Duration,
    /// A package known to render, a still of which is the control job that tells a failure of the
    /// package from a failure of the machine. `None` makes every timeout and crash transient.
    /// The editor's wiring of the real [`ProcessLauncher`] path must give it a package that ships
    /// with the editor.
    pub control_package: Option<PathBuf>,
    /// How long the control job may take.
    pub control_timeout: Duration,
    /// Written into `.failed` and `.attempt` records.
    pub version: String,
}

impl SessionConfig {
    pub fn new() -> Self {
        Self {
            still_timeout: Duration::from_secs(20),
            loop_timeout: Duration::from_secs(60),
            ready_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(2),
            cooldown: Duration::from_secs(60),
            control_package: None,
            control_timeout: Duration::from_secs(5),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn timeout_for(&self, kind: PreviewJobKind) -> Duration {
        match kind {
            PreviewJobKind::Still => self.still_timeout,
            PreviewJobKind::Loop => self.loop_timeout,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// One preview to render.
#[derive(Debug, Clone)]
pub struct PreviewRequest {
    /// The package folder.
    pub package_dir: PathBuf,
    /// The store's name for the package, from [`package_prefix`](super::package_prefix).
    pub package_prefix: String,
    pub key: String,
    pub kind: PreviewJobKind,
    pub spec: PreviewSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderOutcome {
    /// The preview is in the store.
    Published { is_static: bool },
    /// A path of the job is not valid UTF-8, so it cannot be sent without naming another file.
    NotPreviewable(String),
    /// A record of a failure under this adapter and version stands; nothing was sent.
    AlreadyFailed(String),
    /// This time failed. `recorded` is true when it wrote `.failed`.
    Failed { reason: String, recorded: bool },
    /// This session stopped trying the key after failures that are not its fault, or that may pass.
    GivenUp(String),
    /// A stop was asked for.
    Cancelled,
    /// The session ended: shut down, or the helper is not this editor's.
    Stopped(String),
    /// Too many helpers died, or jobs failed on the device; nothing was started and the session
    /// tries again once the cooldown is over.
    CoolingDown(String),
}

struct Running {
    helper: Box<dyn Helper>,
    device: String,
    /// The wall clock time just before the helper was started.
    started: SystemTime,
}

/// How one job ended, before the rules decide what it means.
enum JobEnd {
    Done { is_static: bool },
    Failed { blame: PreviewBlame, message: String },
    /// The helper ended after it had the job.
    Died(String),
    /// The job did not get to the helper.
    NotDelivered(String),
    TimedOut,
    Cancelled,
}

/// What the control job showed.
enum Control {
    /// The machine rendered the control package in time.
    Passed,
    /// It did not, or there was no control: the failure may be the machine's.
    Inconclusive,
    /// The session ended in the meantime.
    Over(RenderOutcome),
}

type KeyId = (String, String);

/// Dropping it ends the helper and can block for several seconds, so do it on a worker thread, not
/// on the UI thread.
pub struct PreviewSession {
    launcher: Box<dyn Launcher>,
    store: PreviewStore,
    config: SessionConfig,
    running: Option<Running>,
    /// The adapter of the last helper that said `ready`.
    last_device: Option<String>,
    /// The next job starts a new helper.
    replace_helper: bool,
    deaths: u32,
    /// Jobs in a row, across keys, that failed with `blame: device`.
    device_failures: u32,
    /// When the cooldown ends, and why it started.
    cooldown: Option<(Instant, String)>,
    /// Cooldowns in a row, since a job last went well.
    cooldown_streak: u32,
    stopped: Option<String>,
    transient: HashMap<KeyId, u32>,
    given_up: HashSet<KeyId>,
    next_id: u64,
    stop: Arc<AtomicBool>,
}

impl PreviewSession {
    pub fn new(launcher: Box<dyn Launcher>, store: PreviewStore, config: SessionConfig) -> Self {
        Self {
            launcher,
            store,
            config,
            running: None,
            last_device: None,
            replace_helper: false,
            deaths: 0,
            device_failures: 0,
            cooldown: None,
            cooldown_streak: 0,
            stopped: None,
            transient: HashMap::new(),
            given_up: HashSet::new(),
            next_id: 0,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A flag another thread sets to end a [`render`](Self::render) that is waiting: the helper is
    /// killed and the job is [`Cancelled`](RenderOutcome::Cancelled). The session then stays stopped.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// The adapter the helper reported, once one was started.
    pub fn device(&self) -> Option<&str> {
        self.last_device.as_deref()
    }

    pub fn store(&self) -> &PreviewStore {
        &self.store
    }

    pub fn render(&mut self, request: &PreviewRequest) -> RenderOutcome {
        if let Some(reason) = &self.stopped {
            return RenderOutcome::Stopped(reason.clone());
        }
        if self.stop.load(Ordering::SeqCst) {
            return RenderOutcome::Cancelled;
        }
        if let Some(outcome) = self.cooling_down() {
            return outcome;
        }
        let key_id: KeyId = (request.package_prefix.clone(), request.key.clone());
        if self.given_up.contains(&key_id) {
            return RenderOutcome::GivenUp("this session stopped trying this preview".into());
        }
        let Some(package) = request.package_dir.to_str() else {
            return RenderOutcome::NotPreviewable(format!("the package path is not valid UTF-8: {}", request.package_dir.display()));
        };
        self.next_id += 1;
        let id = self.next_id;
        let temporary = self.store.temporary_path();
        let Some(out) = temporary.to_str() else {
            return RenderOutcome::NotPreviewable(format!("the preview folder is not valid UTF-8: {}", self.store.root.display()));
        };
        let job = PreviewJob { id, kind: request.kind, package: package.to_string(), out: out.to_string() };
        let line = job.to_line();
        if line.len() > MAX_EVENT_LINE {
            return RenderOutcome::NotPreviewable(format!("the job line is {} bytes, more than the {MAX_EVENT_LINE} the helper reads", line.len()));
        }
        if let Err(e) = std::fs::create_dir_all(&self.store.root) {
            return RenderOutcome::Failed { reason: format!("creating {} failed: {e}", self.store.root.display()), recorded: false };
        }
        // A key that failed on this adapter starts or replaces no helper.
        if let Some(reason) = self.known_failure(request) {
            return RenderOutcome::AlreadyFailed(reason);
        }
        if let Err(outcome) = self.ensure_running() {
            return outcome;
        }
        if self.helper_is_gone() {
            // Nothing was sent, so the key is not to blame.
            self.note_death("the preview helper was gone before the job was sent");
            if let Some(outcome) = self.cooling_down() {
                return outcome;
            }
            if let Err(outcome) = self.ensure_running() {
                return outcome;
            }
        }
        if let Some(reason) = self.known_failure(request) {
            return RenderOutcome::AlreadyFailed(reason);
        }
        let started = self.running.as_ref().map_or_else(SystemTime::now, |running| running.started);
        let end = self.run_job(&job, &line, self.config.timeout_for(request.kind));
        let outcome = self.settle(request, &key_id, &temporary, started, end);
        let _ = std::fs::remove_file(&temporary);
        outcome
    }

    /// The reason of a `.failed` record of this key under the adapter last seen and this version.
    fn known_failure(&self, request: &PreviewRequest) -> Option<String> {
        let device = self.last_device.as_deref()?;
        self.store.failure_on(&request.package_prefix, &request.key, device, &self.config.version)
    }

    /// `CoolingDown` while the cooldown lasts. When it is over the counts start again.
    fn cooling_down(&mut self) -> Option<RenderOutcome> {
        let (until, reason) = self.cooldown.as_ref()?;
        if Instant::now() < *until {
            return Some(RenderOutcome::CoolingDown(reason.clone()));
        }
        self.cooldown = None;
        self.deaths = 0;
        self.device_failures = 0;
        None
    }

    /// How long the cooldown lasts when `streak` cooldowns came before it in a row.
    fn cooldown_for(&self, streak: u32) -> Duration {
        self.config.cooldown.saturating_mul(1u32 << streak.min(16)).min(MAX_COOLDOWN)
    }

    fn start_cooldown(&mut self, reason: String) {
        self.kill_running();
        let length = self.cooldown_for(self.cooldown_streak);
        self.cooldown_streak = self.cooldown_streak.saturating_add(1);
        self.cooldown = Some((Instant::now() + length, reason));
    }

    /// Whether the helper that is idle has ended since its last line. Lines it wrote in between are read and dropped.
    fn helper_is_gone(&mut self) -> bool {
        let Some(running) = self.running.as_mut() else { return false };
        for _ in 0..PROBE_LINES {
            match running.helper.recv(Duration::ZERO) {
                Incoming::Closed => return true,
                Incoming::Timeout => return false,
                Incoming::Event(_) | Incoming::Malformed => {}
            }
        }
        false
    }

    /// Ends the helper: its stdin is closed, and it is killed if it is still there after the grace.
    /// The session is stopped afterwards.
    pub fn shutdown(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.helper.close_and_wait(self.config.shutdown_grace);
        }
        self.stopped.get_or_insert_with(|| "the preview session was shut down".to_string());
    }

    fn kill_running(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.helper.kill();
        }
    }

    /// A helper died, or could not be started. Too many in a row start a cooldown.
    fn note_death(&mut self, reason: &str) {
        self.kill_running();
        self.deaths += 1;
        if self.deaths >= MAX_DEATHS {
            self.start_cooldown(format!("the preview helper died {} times in a row; the last time: {reason}", self.deaths));
        }
    }

    fn ensure_running(&mut self) -> Result<(), RenderOutcome> {
        if self.replace_helper {
            self.kill_running();
            self.replace_helper = false;
        }
        if self.running.is_some() {
            return Ok(());
        }
        let started = SystemTime::now();
        let mut helper = match self.launcher.start() {
            Ok(helper) => helper,
            Err(reason) => {
                self.note_death(&reason);
                return Err(RenderOutcome::Failed { reason, recorded: false });
            }
        };
        let deadline = Instant::now() + self.config.ready_timeout;
        let stop = self.stop.clone();
        loop {
            if stop.load(Ordering::SeqCst) {
                helper.kill();
                return Err(RenderOutcome::Cancelled);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                helper.kill();
                let reason = format!("the preview helper did not say it was ready within {:?}", self.config.ready_timeout);
                self.note_death(&reason);
                return Err(RenderOutcome::Failed { reason, recorded: false });
            }
            match helper.recv(left.min(SLICE)) {
                Incoming::Event(PreviewEvent::Ready { protocol, device }) => {
                    if protocol != PREVIEW_PROTOCOL {
                        helper.kill();
                        let reason = format!("the preview helper speaks protocol {protocol}, this editor speaks {PREVIEW_PROTOCOL}: gpufx-render.exe is not the one built with it");
                        self.stopped = Some(reason.clone());
                        return Err(RenderOutcome::Stopped(reason));
                    }
                    self.last_device = Some(device.clone());
                    self.running = Some(Running { helper, device, started });
                    return Ok(());
                }
                Incoming::Event(PreviewEvent::Fatal { message }) => {
                    let diagnostics = helper.diagnostics();
                    helper.kill();
                    let reason = with_diagnostics(format!("the preview helper has no renderer: {message}"), &diagnostics);
                    self.note_death(&reason);
                    return Err(RenderOutcome::Failed { reason, recorded: false });
                }
                Incoming::Closed => {
                    let diagnostics = helper.diagnostics();
                    helper.kill();
                    let reason = with_diagnostics("the preview helper exited before it was ready".into(), &diagnostics);
                    self.note_death(&reason);
                    return Err(RenderOutcome::Failed { reason, recorded: false });
                }
                Incoming::Event(_) | Incoming::Malformed | Incoming::Timeout => {}
            }
        }
    }

    fn run_job(&mut self, job: &PreviewJob, line: &str, timeout: Duration) -> JobEnd {
        let stop = self.stop.clone();
        let running = self.running.as_mut().expect("a helper is running");
        if let Err(e) = running.helper.send(line) {
            return JobEnd::NotDelivered(format!("writing to the preview helper failed: {e}"));
        }
        let deadline = Instant::now() + timeout;
        // Lines read after the deadline, which looks once at what is already there.
        let mut late = 0;
        loop {
            if stop.load(Ordering::SeqCst) {
                return JobEnd::Cancelled;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let expired = left.is_zero();
            match running.helper.recv(if expired { Duration::ZERO } else { left.min(SLICE) }) {
                Incoming::Event(PreviewEvent::Done { id, is_static }) if id == job.id => return JobEnd::Done { is_static },
                Incoming::Event(PreviewEvent::Failed { id, blame, message }) if id == job.id => return JobEnd::Failed { blame, message },
                // The job line was not accepted as it was written; that is neither the package's nor the device's doing.
                Incoming::Event(PreviewEvent::Invalid { id: Some(id), message }) if id == job.id => return JobEnd::Failed { blame: PreviewBlame::Output, message },
                // The helper cannot tell which line it refused, and this is the only job it has.
                Incoming::Event(PreviewEvent::Invalid { id: None, message }) => return JobEnd::Failed { blame: PreviewBlame::Output, message },
                Incoming::Event(PreviewEvent::Fatal { message }) => return JobEnd::Died(format!("the preview helper reported: {message}")),
                Incoming::Closed => {
                    let diagnostics = running.helper.diagnostics();
                    return JobEnd::Died(with_diagnostics("the preview helper exited during a job".into(), &diagnostics));
                }
                Incoming::Timeout if expired => return JobEnd::TimedOut,
                // Lines of another job, a second `ready`, noise.
                Incoming::Event(_) | Incoming::Malformed | Incoming::Timeout => {}
            }
            if expired {
                late += 1;
                if late >= DEADLINE_LINES {
                    return JobEnd::TimedOut;
                }
            }
        }
    }

    fn settle(&mut self, request: &PreviewRequest, key_id: &KeyId, temporary: &Path, helper_started: SystemTime, end: JobEnd) -> RenderOutcome {
        match end {
            JobEnd::Done { is_static } => {
                let (prefix, key) = (&request.package_prefix, &request.key);
                let published = match request.kind {
                    PreviewJobKind::Still => self.store.publish_still(prefix, key, request.spec, temporary),
                    PreviewJobKind::Loop => self.store.publish_loop(prefix, key, request.spec, temporary),
                };
                match published {
                    Ok(()) => {
                        self.store.clear_attempt(prefix, key);
                        self.deaths = 0;
                        self.device_failures = 0;
                        self.cooldown_streak = 0;
                        self.transient.remove(key_id);
                        RenderOutcome::Published { is_static }
                    }
                    Err(e) => self.transient_failure(key_id, format!("publishing the preview failed: {e}"), false),
                }
            }
            JobEnd::Failed { blame: PreviewBlame::Package, message } => self.package_failure(request, key_id, message, true, helper_started),
            JobEnd::Failed { blame: PreviewBlame::Device, message } => {
                self.device_failures += 1;
                if self.device_failures >= MAX_DEVICE_FAILURES {
                    self.start_cooldown(format!("{} jobs in a row failed on the device; the last time: {message}", self.device_failures));
                }
                self.transient_failure(key_id, message, true)
            }
            JobEnd::Failed { blame: PreviewBlame::Output, message } => self.transient_failure(key_id, message, false),
            JobEnd::Died(reason) => {
                self.note_death(&reason);
                self.unproven_failure(request, key_id, reason, helper_started)
            }
            JobEnd::TimedOut => {
                let reason = format!("rendering took longer than {:?}", self.config.timeout_for(request.kind));
                self.note_death(&reason);
                self.unproven_failure(request, key_id, reason, helper_started)
            }
            JobEnd::NotDelivered(reason) => {
                // Nothing ran, so this says nothing about the package, and it is no finished job.
                self.note_death(&reason);
                RenderOutcome::Failed { reason, recorded: false }
            }
            JobEnd::Cancelled => {
                self.kill_running();
                RenderOutcome::Cancelled
            }
        }
    }

    /// A timeout or a crash after delivery. It is held against the package only when a control job
    /// shows that the machine can still render.
    fn unproven_failure(&mut self, request: &PreviewRequest, key_id: &KeyId, reason: String, helper_started: SystemTime) -> RenderOutcome {
        match self.run_control() {
            // The control helper is not the one that saw the failure; the retry must not run on it either.
            Control::Passed => self.package_failure(request, key_id, reason, true, helper_started),
            Control::Inconclusive => self.transient_failure(key_id, reason, false),
            Control::Over(outcome) => outcome,
        }
    }

    /// Renders a still of the control package on a fresh helper, to a temporary file that is
    /// removed and never published. A helper that dies here counts as a death, but there is no
    /// control for it.
    fn run_control(&mut self) -> Control {
        let Some(package) = self.config.control_package.as_deref().and_then(Path::to_str).map(str::to_string) else {
            return Control::Inconclusive;
        };
        if self.cooldown.is_some() {
            return Control::Inconclusive;
        }
        let temporary = self.store.temporary_path();
        let Some(out) = temporary.to_str().map(str::to_string) else {
            return Control::Inconclusive;
        };
        match self.ensure_running() {
            Ok(()) => {}
            Err(outcome @ (RenderOutcome::Cancelled | RenderOutcome::Stopped(_))) => return Control::Over(outcome),
            Err(_) => return Control::Inconclusive,
        }
        self.next_id += 1;
        let job = PreviewJob { id: self.next_id, kind: PreviewJobKind::Still, package, out };
        let line = job.to_line();
        if line.len() > MAX_EVENT_LINE {
            return Control::Inconclusive;
        }
        let end = self.run_job(&job, &line, self.config.control_timeout);
        let _ = std::fs::remove_file(&temporary);
        match end {
            JobEnd::Done { .. } => Control::Passed,
            JobEnd::Cancelled => {
                self.kill_running();
                Control::Over(RenderOutcome::Cancelled)
            }
            JobEnd::Died(reason) | JobEnd::NotDelivered(reason) => {
                self.note_death(&reason);
                Control::Inconclusive
            }
            JobEnd::TimedOut => {
                self.note_death("the control job timed out");
                Control::Inconclusive
            }
            JobEnd::Failed { .. } => Control::Inconclusive,
        }
    }

    /// Records an attempt; the second one for this key writes `.failed`. `replace` asks for a new
    /// helper before the next job, since the helper that saw a failure must not see the retry.
    /// `helper_started` is when the helper that ran the job was started. When no attempt was
    /// written, or `.failed` could not be, the failure is transient, so that it is bounded.
    fn package_failure(&mut self, request: &PreviewRequest, key_id: &KeyId, reason: String, replace: bool, helper_started: SystemTime) -> RenderOutcome {
        let adapter = self.last_device.clone().unwrap_or_default();
        match self.store.mark_attempt(&request.package_prefix, &request.key, &adapter, &self.config.version, helper_started) {
            Ok(Some(before)) if before >= 1 => {
                if self.store.mark_failed_on(&request.package_prefix, &request.key, &adapter, &self.config.version, &reason).is_ok() {
                    return RenderOutcome::Failed { reason, recorded: true };
                }
                self.transient_failure(key_id, reason, replace)
            }
            Ok(Some(_)) => {
                self.replace_helper |= replace;
                RenderOutcome::Failed { reason, recorded: false }
            }
            Ok(None) | Err(_) => self.transient_failure(key_id, reason, replace),
        }
    }

    /// A failure that is not recorded on disk. The key is given up for the session after [`MAX_TRANSIENT`].
    fn transient_failure(&mut self, key_id: &KeyId, reason: String, replace: bool) -> RenderOutcome {
        self.replace_helper |= replace;
        let count = self.transient.entry(key_id.clone()).or_insert(0);
        *count += 1;
        if *count >= MAX_TRANSIENT {
            self.given_up.insert(key_id.clone());
            return RenderOutcome::GivenUp(format!("failed {MAX_TRANSIENT} times without a verdict on the package; the last time: {reason}"));
        }
        RenderOutcome::Failed { reason, recorded: false }
    }
}

impl Drop for PreviewSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn with_diagnostics(reason: String, diagnostics: &str) -> String {
    let diagnostics = diagnostics.trim();
    if diagnostics.is_empty() {
        reason
    } else {
        format!("{reason}; stderr: {diagnostics}")
    }
}

/// Starts the real `gpufx-render.exe`.
pub struct ProcessLauncher {
    exe: PathBuf,
}

impl ProcessLauncher {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }

    /// The renderer beside the running editor.
    pub fn sibling() -> Self {
        Self::new(sibling_render_exe())
    }
}

impl Launcher for ProcessLauncher {
    fn start(&mut self) -> Result<Box<dyn Helper>, String> {
        if !self.exe.is_file() {
            return Err(format!("the preview helper {} does not exist", self.exe.display()));
        }
        let job = KillOnClose::new()?;
        let mut child = Command::new(&self.exe).arg("--effect-previews").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW).spawn().map_err(|e| format!("starting {} failed: {e}", self.exe.display()))?;
        if let Err(message) = job.assign(&child, "the preview helper") {
            let _ = child.kill();
            let _ = child.wait();
            return Err(message);
        }
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        // Both pipes are read on threads of their own, so a full pipe never blocks the helper.
        let tail = thread::spawn(move || tail_of(stderr));
        let (sender, events) = channel();
        let pump = thread::spawn(move || {
            pump_lines(stdout, |line| {
                let _ = sender.send(Some(line.to_vec()));
            });
            let _ = sender.send(None);
        });
        Ok(Box::new(ProcessHelper { child, stdin, events, pump: Some(pump), tail: Some(tail), stderr: String::new(), _job: job }))
    }
}

struct ProcessHelper {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Option<Vec<u8>>>,
    pump: Option<JoinHandle<()>>,
    tail: Option<JoinHandle<String>>,
    stderr: String,
    /// Closing it kills the helper, so it goes last.
    _job: KillOnClose,
}

impl ProcessHelper {
    fn wait_exit(&mut self, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if started.elapsed() < limit => thread::sleep(POLL),
                _ => return false,
            }
        }
    }

    /// Waits a little for the reader threads, which end once the pipes close. A pipe that a stray
    /// process holds open leaves its thread behind.
    fn finish_threads(&mut self) {
        if let Some(pump) = self.pump.take() {
            let _ = join_within(pump, KILL_WAIT);
        }
        if let Some(tail) = self.tail.take() {
            if let Some(text) = join_within(tail, KILL_WAIT) {
                self.stderr = text;
            }
        }
    }
}

impl Helper for ProcessHelper {
    fn send(&mut self, line: &str) -> std::io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin is closed"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    fn recv(&mut self, wait: Duration) -> Incoming {
        match self.events.recv_timeout(wait) {
            Ok(Some(line)) => std::str::from_utf8(&line).ok().and_then(PreviewEvent::parse).map_or(Incoming::Malformed, Incoming::Event),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => Incoming::Closed,
            Err(RecvTimeoutError::Timeout) => Incoming::Timeout,
        }
    }

    fn kill(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        self.wait_exit(KILL_WAIT);
        self.finish_threads();
    }

    fn close_and_wait(&mut self, grace: Duration) {
        self.stdin = None;
        if !self.wait_exit(grace) {
            let _ = self.child.kill();
            self.wait_exit(KILL_WAIT);
        }
        self.finish_threads();
    }

    fn diagnostics(&mut self) -> String {
        self.finish_threads();
        self.stderr.clone()
    }
}

impl Drop for ProcessHelper {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_previews::disk::unix_nanos;
    use crate::effect_previews::{package_prefix, PreviewStore};
    use d3d12_backend::PREVIEW_SIZE;
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::sync::atomic::AtomicU32;
    use std::sync::Mutex;

    const KEY: &str = "0123456789abcdef";
    const OTHER_KEY: &str = "fedcba9876543210";
    const VERSION: &str = "9.9.9";
    /// The package of the control job.
    const CONTROL_PACKAGE: &str = r"C:\packages\ffmpeg\control";

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Behavior {
        Done,
        Fail(PreviewBlame),
        /// Never answers.
        Hang,
        /// Stdout closes.
        Crash,
        Fatal,
        /// The line cannot be written.
        SendFails,
        /// Answers `done` just before the deadline, and not before it is looked for.
        LateDone,
        /// Says the line it read was invalid, without an id.
        InvalidNoId,
    }

    struct Plan {
        /// What the helper says first, instead of `ready`.
        first: Option<PreviewEvent>,
        protocol: u32,
        device: &'static str,
        behaviors: VecDeque<Behavior>,
        /// Stdout closes right after `ready`.
        dead_after_ready: bool,
    }

    fn plan(behaviors: &[Behavior]) -> Plan {
        Plan { first: None, protocol: PREVIEW_PROTOCOL, device: "GPU A", behaviors: behaviors.iter().copied().collect(), dead_after_ready: false }
    }

    #[derive(Default)]
    struct Log {
        starts: u32,
        sent: Vec<PreviewJob>,
        killed: u32,
        closed: u32,
    }

    struct FakeHelper {
        queue: VecDeque<Incoming>,
        /// An event that is delivered once its time has come.
        pending: Option<(Instant, Incoming)>,
        behaviors: VecDeque<Behavior>,
        hung: bool,
        gone: bool,
        log: Arc<Mutex<Log>>,
    }

    fn png_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, PREVIEW_SIZE, PREVIEW_SIZE);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![200u8; (PREVIEW_SIZE * PREVIEW_SIZE * 4) as usize]).unwrap();
        writer.finish().unwrap();
        bytes
    }

    impl Helper for FakeHelper {
        fn send(&mut self, line: &str) -> std::io::Result<()> {
            let job = PreviewJob::parse(line).expect("the session sends a valid job");
            let behavior = self.behaviors.pop_front().unwrap_or(Behavior::Done);
            if behavior == Behavior::SendFails {
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the pipe is closed"));
            }
            self.log.lock().unwrap().sent.push(job.clone());
            match behavior {
                Behavior::Done => {
                    std::fs::write(&job.out, png_bytes()).unwrap();
                    self.queue.push_back(Incoming::Event(PreviewEvent::Done { id: job.id, is_static: false }));
                }
                Behavior::Fail(blame) => self.queue.push_back(Incoming::Event(PreviewEvent::Failed { id: job.id, blame, message: format!("{blame:?} went wrong") })),
                Behavior::Hang => self.hung = true,
                Behavior::Crash => self.gone = true,
                Behavior::Fatal => self.queue.push_back(Incoming::Event(PreviewEvent::Fatal { message: "no renderer".into() })),
                Behavior::SendFails => unreachable!("returned above"),
                Behavior::LateDone => {
                    std::fs::write(&job.out, png_bytes()).unwrap();
                    self.hung = true;
                    self.pending = Some((Instant::now() + Duration::from_millis(100), Incoming::Event(PreviewEvent::Done { id: job.id, is_static: false })));
                }
                Behavior::InvalidNoId => self.queue.push_back(Incoming::Event(PreviewEvent::Invalid { id: None, message: "the line is bad".into() })),
            }
            Ok(())
        }

        fn recv(&mut self, wait: Duration) -> Incoming {
            if let Some(next) = self.queue.pop_front() {
                return next;
            }
            if self.pending.as_ref().is_some_and(|(at, _)| Instant::now() >= *at) {
                return self.pending.take().expect("checked").1;
            }
            if self.gone {
                return Incoming::Closed;
            }
            thread::sleep(if self.hung { wait } else { Duration::ZERO });
            Incoming::Timeout
        }

        fn kill(&mut self) {
            self.log.lock().unwrap().killed += 1;
        }

        fn close_and_wait(&mut self, _grace: Duration) {
            self.log.lock().unwrap().closed += 1;
        }
    }

    struct FakeLauncher {
        plans: VecDeque<Plan>,
        log: Arc<Mutex<Log>>,
    }

    impl Launcher for FakeLauncher {
        fn start(&mut self) -> Result<Box<dyn Helper>, String> {
            self.log.lock().unwrap().starts += 1;
            let plan = self.plans.pop_front().ok_or_else(|| "the helper cannot start".to_string())?;
            let ready = plan.first.unwrap_or(PreviewEvent::Ready { protocol: plan.protocol, device: plan.device.to_string() });
            Ok(Box::new(FakeHelper { queue: VecDeque::from([Incoming::Event(ready)]), pending: None, behaviors: plan.behaviors, hung: false, gone: plan.dead_after_ready, log: self.log.clone() }))
        }
    }

    fn scratch_root(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("gpufx-previews-child-{}-{name}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("previews")
    }

    fn config() -> SessionConfig {
        SessionConfig {
            still_timeout: Duration::from_millis(100),
            loop_timeout: Duration::from_millis(100),
            ready_timeout: Duration::from_millis(100),
            shutdown_grace: Duration::from_millis(10),
            cooldown: Duration::from_millis(500),
            control_package: None,
            control_timeout: Duration::from_millis(100),
            version: VERSION.into(),
        }
    }

    /// Longer than the cooldown of [`config`].
    const COOLDOWN_PASSED: Duration = Duration::from_millis(600);

    fn start_session(name: &str, plans: Vec<Plan>) -> (PreviewSession, Arc<Mutex<Log>>, PreviewStore) {
        let root = scratch_root(name);
        let log = Arc::new(Mutex::new(Log::default()));
        let launcher = FakeLauncher { plans: plans.into(), log: log.clone() };
        (PreviewSession::new(Box::new(launcher), PreviewStore::new(root.clone()), config()), log, PreviewStore::new(root))
    }

    fn request(key: &str) -> PreviewRequest {
        PreviewRequest { package_dir: PathBuf::from(r"C:\packages\ffmpeg\invert"), package_prefix: package_prefix("ffmpeg/invert"), key: key.into(), kind: PreviewJobKind::Still, spec: PreviewSpec::STANDARD }
    }

    fn with_control(session: &mut PreviewSession) {
        session.config.control_package = Some(PathBuf::from(CONTROL_PACKAGE));
    }

    /// Another session on the store of an earlier one, as after the editor was restarted.
    fn new_session(store: &PreviewStore, plans: Vec<Plan>) -> (PreviewSession, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let launcher = FakeLauncher { plans: plans.into(), log: log.clone() };
        (PreviewSession::new(Box::new(launcher), PreviewStore::new(store.root.clone()), config()), log)
    }

    fn count_files(store: &PreviewStore, extension: &str) -> usize {
        std::fs::read_dir(&store.root).map_or(0, |entries| entries.flatten().filter(|entry| entry.path().extension().is_some_and(|found| found == extension)).count())
    }

    fn failed(outcome: &RenderOutcome) -> bool {
        matches!(outcome, RenderOutcome::Failed { recorded: false, .. })
    }

    fn has_failed_record(store: &PreviewStore, key: &str) -> bool {
        store.failure(&request(key).package_prefix, key).is_some()
    }

    fn attempt_file(store: &PreviewStore, key: &str) -> PathBuf {
        store.root.join(format!("{}-{key}.attempt", request(key).package_prefix))
    }

    fn read_attempt(store: &PreviewStore, key: &str) -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(attempt_file(store, key)).ok()?).ok()
    }

    fn write_attempt(store: &PreviewStore, key: &str, adapter: &str, at: SystemTime) {
        std::fs::create_dir_all(&store.root).unwrap();
        let record = serde_json::json!({ "count": 1, "adapter": adapter, "version": VERSION, "at": unix_nanos(at) });
        std::fs::write(attempt_file(store, key), record.to_string()).unwrap();
    }

    fn key(n: u32) -> String {
        format!("{n:016x}")
    }

    #[test]
    fn a_job_that_goes_well_is_published_and_leaves_no_temporary_file() {
        let (mut session, log, store) = start_session("done", vec![plan(&[Behavior::Done])]);
        let request = request(KEY);
        assert_eq!(session.render(&request), RenderOutcome::Published { is_static: false });
        assert!(store.read_still(&request.package_prefix, KEY, PreviewSpec::STANDARD).unwrap().is_some());
        let sent = log.lock().unwrap().sent.clone();
        assert_eq!(sent.len(), 1);
        let out = Path::new(&sent[0].out);
        assert_eq!(out.parent().unwrap(), store.root);
        assert_eq!(out.extension().unwrap(), "tmp");
        assert!(!out.exists());
        assert_eq!(sent[0].package, r"C:\packages\ffmpeg\invert");
        assert_eq!(session.device(), Some("GPU A"));
    }

    #[test]
    fn a_path_that_is_not_utf8_is_not_previewable_and_nothing_is_sent() {
        let (mut session, log, _store) = start_session("lossy", vec![plan(&[])]);
        let mut request = request(KEY);
        request.package_dir = PathBuf::from(OsString::from_wide(&[0x61, 0xD800, 0x62]));
        assert!(matches!(session.render(&request), RenderOutcome::NotPreviewable(_)));
        let log = log.lock().unwrap();
        assert!(log.sent.is_empty());
        assert_eq!(log.starts, 0);
    }

    #[test]
    fn a_job_line_longer_than_the_helper_reads_is_not_previewable_and_starts_nothing() {
        let (mut session, log, _store) = start_session("long-line", vec![plan(&[])]);
        let mut request = request(KEY);
        request.package_dir = PathBuf::from(format!(r"C:\{}", "a".repeat(MAX_EVENT_LINE)));
        assert!(matches!(session.render(&request), RenderOutcome::NotPreviewable(_)));
        let log = log.lock().unwrap();
        assert!(log.sent.is_empty());
        assert_eq!(log.starts, 0);
    }

    #[test]
    fn a_helper_of_another_protocol_stops_the_session_with_a_reason() {
        let (mut session, log, _store) = start_session("protocol", vec![Plan { protocol: PREVIEW_PROTOCOL + 1, ..plan(&[]) }, plan(&[])]);
        let RenderOutcome::Stopped(reason) = session.render(&request(KEY)) else { panic!("not stopped") };
        assert!(reason.contains("protocol"), "{reason}");
        assert_eq!(log.lock().unwrap().killed, 1);
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::Stopped(_)));
        assert_eq!(log.lock().unwrap().starts, 1, "no second try");
    }

    #[test]
    fn a_package_failure_is_recorded_once_and_the_second_one_on_a_new_helper_writes_failed() {
        let (mut session, log, store) = start_session("package", vec![plan(&[Behavior::Fail(PreviewBlame::Package)]), plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        assert!(!has_failed_record(&store, KEY), "one failure never writes .failed");
        let attempt = read_attempt(&store, KEY).expect("the attempt was recorded");
        assert_eq!((attempt["count"].as_u64(), attempt["adapter"].as_str(), attempt["version"].as_str()), (Some(1), Some("GPU A"), Some(VERSION)));
        let second = session.render(&request);
        assert!(matches!(second, RenderOutcome::Failed { recorded: true, .. }), "{second:?}");
        assert_eq!(log.lock().unwrap().starts, 2, "the retry ran on a helper started after the first failure");
        assert_eq!(store.failure_on(&request.package_prefix, KEY, "GPU A", VERSION).as_deref(), Some("Package went wrong"));
        assert_eq!(store.failure_on(&request.package_prefix, KEY, "GPU B", VERSION), None);
        assert_eq!(store.failure_on(&request.package_prefix, KEY, "GPU A", "9.9.10"), None);
        assert!(!attempt_file(&store, KEY).exists(), "writing .failed removes the attempt");
    }

    #[test]
    fn a_failed_record_of_an_old_version_followed_by_one_failure_does_not_write_failed() {
        let (mut session, _log, store) = start_session("old-failed", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        let request = request(KEY);
        store.mark_failed_on(&request.package_prefix, KEY, "GPU A", "0.0.1", "old").unwrap();
        let outcome = session.render(&request);
        assert!(failed(&outcome), "{outcome:?}");
        assert_eq!(store.failure_on(&request.package_prefix, KEY, "GPU A", VERSION), None);
        assert!(attempt_file(&store, KEY).exists());
    }

    #[test]
    fn a_stored_attempt_of_another_adapter_counts_as_zero() {
        let (mut session, _log, store) = start_session("attempt-adapter", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        write_attempt(&store, KEY, "GPU B", SystemTime::now() - Duration::from_secs(60));
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(!has_failed_record(&store, KEY));
        assert_eq!(read_attempt(&store, KEY).unwrap()["adapter"], "GPU A", "the attempt starts again under this adapter");
    }

    #[test]
    fn a_stored_attempt_older_than_seven_days_counts_as_zero() {
        let (mut session, _log, store) = start_session("attempt-age", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        write_attempt(&store, KEY, "GPU A", SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60));
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn a_stored_attempt_in_the_old_format_counts_as_zero() {
        let (mut session, _log, store) = start_session("attempt-old-format", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        std::fs::create_dir_all(&store.root).unwrap();
        std::fs::write(attempt_file(&store, KEY), "1").unwrap();
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn an_attempt_recorded_after_the_helper_started_is_not_a_second_occurrence() {
        let (mut session, _log, store) = start_session("attempt-other-editor", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        // Another editor failed on the same store a moment ago, after this helper started.
        let at = SystemTime::now() + Duration::from_secs(60);
        write_attempt(&store, KEY, "GPU A", at);
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(!has_failed_record(&store, KEY));
        assert_eq!(read_attempt(&store, KEY).unwrap()["at"], unix_nanos(at), "the record is left as it was");
    }

    #[test]
    fn a_timeout_kills_the_helper_and_with_no_control_package_writes_nothing_on_disk() {
        let (mut session, log, store) = start_session("timeout-unproven", vec![plan(&[Behavior::Hang]), plan(&[Behavior::Done])]);
        assert!(failed(&session.render(&request(KEY))));
        assert_eq!(log.lock().unwrap().killed, 1);
        assert_eq!(log.lock().unwrap().starts, 1, "no control job without a package");
        assert_eq!(session.deaths, 1);
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn timeouts_without_a_passing_control_give_the_key_up_for_the_session_only() {
        let (mut session, _log, store) = start_session("timeout-given-up", vec![plan(&[Behavior::Hang]), plan(&[Behavior::Hang]), plan(&[Behavior::Hang])]);
        assert!(failed(&session.render(&request(KEY))));
        assert!(failed(&session.render(&request(KEY))));
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::GivenUp(_)));
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn a_timeout_with_a_passing_control_records_an_attempt_and_the_first_job_of_the_next_session_writes_failed() {
        let (mut session, log, store) = start_session("timeout-proven", vec![plan(&[Behavior::Hang]), plan(&[Behavior::Done])]);
        with_control(&mut session);
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        {
            let log = log.lock().unwrap();
            assert_eq!(log.starts, 2, "the control ran on a fresh helper");
            assert_eq!(log.sent.len(), 2);
            assert_eq!(log.sent[1].package, CONTROL_PACKAGE);
            assert_eq!(log.killed, 1);
        }
        assert_eq!(read_attempt(&store, KEY).expect("an attempt")["count"], 1);
        assert!(!has_failed_record(&store, KEY));
        assert_eq!(count_files(&store, "png"), 0, "the control is never published");
        assert_eq!(count_files(&store, "tmp"), 0, "the control leaves no temporary file");

        thread::sleep(Duration::from_millis(20));
        let (mut next, next_log) = new_session(&store, vec![plan(&[Behavior::Hang]), plan(&[Behavior::Done])]);
        with_control(&mut next);
        let second = next.render(&request);
        assert!(matches!(second, RenderOutcome::Failed { recorded: true, .. }), "{second:?}");
        assert_eq!(next_log.lock().unwrap().starts, 2);
        assert!(has_failed_record(&store, KEY));
        assert!(!attempt_file(&store, KEY).exists());
    }

    #[test]
    fn a_loop_has_its_own_timeout() {
        let (mut session, _log, _store) = start_session("loop-timeout", vec![plan(&[Behavior::Hang])]);
        session.config.loop_timeout = Duration::from_millis(400);
        let mut request = request(KEY);
        request.kind = PreviewJobKind::Loop;
        let started = Instant::now();
        let RenderOutcome::Failed { reason, .. } = session.render(&request) else { panic!("did not fail") };
        assert!(started.elapsed() >= Duration::from_millis(350), "{:?}", started.elapsed());
        assert!(reason.contains("400ms"), "{reason}");
    }

    #[test]
    fn a_done_that_is_queued_at_the_deadline_is_accepted() {
        let (mut session, log, _store) = start_session("late-done", vec![plan(&[Behavior::LateDone])]);
        assert_eq!(session.render(&request(KEY)), RenderOutcome::Published { is_static: false });
        assert_eq!(log.lock().unwrap().killed, 0);
    }

    #[test]
    fn a_crash_with_a_passing_control_counts_and_a_good_job_after_it_clears_the_attempt() {
        let (mut session, log, store) = start_session("crash", vec![plan(&[Behavior::Crash]), plan(&[Behavior::Done]), plan(&[Behavior::Done])]);
        with_control(&mut session);
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        assert!(attempt_file(&store, KEY).exists());
        assert_eq!(session.render(&request), RenderOutcome::Published { is_static: false });
        assert_eq!(log.lock().unwrap().starts, 3, "the retry did not run on the control helper");
        assert!(!has_failed_record(&store, KEY));
        assert!(!attempt_file(&store, KEY).exists(), "success cleared the attempt");
    }

    #[test]
    fn a_crash_with_no_control_package_writes_nothing_on_disk() {
        let (mut session, log, store) = start_session("crash-unproven", vec![plan(&[Behavior::Crash]), plan(&[Behavior::Done])]);
        assert!(failed(&session.render(&request(KEY))));
        assert_eq!(session.deaths, 1);
        assert_eq!(log.lock().unwrap().starts, 1);
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn a_crash_as_the_first_job_of_each_of_two_sessions_with_a_passing_control_writes_failed_on_the_second() {
        let (mut session, _log, store) = start_session("crash-twice", vec![plan(&[Behavior::Crash]), plan(&[Behavior::Done])]);
        with_control(&mut session);
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        assert!(!has_failed_record(&store, KEY));
        thread::sleep(Duration::from_millis(20));
        let (mut next, _next_log) = new_session(&store, vec![plan(&[Behavior::Crash]), plan(&[Behavior::Done])]);
        with_control(&mut next);
        assert!(matches!(next.render(&request), RenderOutcome::Failed { recorded: true, .. }));
        assert!(has_failed_record(&store, KEY));
    }

    #[test]
    fn two_crashes_in_one_session_with_a_passing_control_write_failed_on_a_helper_started_after_the_first() {
        let plans = vec![plan(&[Behavior::Crash]), plan(&[Behavior::Done]), plan(&[Behavior::Crash]), plan(&[Behavior::Done])];
        let (mut session, log, store) = start_session("crash-same-session", plans);
        with_control(&mut session);
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        assert!(session.replace_helper, "the control helper does not see the retry");
        let second = session.render(&request);
        assert!(matches!(second, RenderOutcome::Failed { recorded: true, .. }), "{second:?}");
        assert_eq!(log.lock().unwrap().starts, 4);
        assert!(has_failed_record(&store, KEY));
    }

    #[test]
    fn a_control_that_fails_writes_nothing_counts_against_no_key_and_is_not_followed_by_a_control() {
        for control in [Behavior::Crash, Behavior::Hang, Behavior::SendFails, Behavior::Fail(PreviewBlame::Package), Behavior::Fail(PreviewBlame::Device), Behavior::Fatal] {
            let (mut session, log, store) = start_session("control-fails", vec![plan(&[Behavior::Crash]), plan(&[control]), plan(&[Behavior::Done])]);
            with_control(&mut session);
            let outcome = session.render(&request(KEY));
            assert!(failed(&outcome), "{control:?}: {outcome:?}");
            assert_eq!(log.lock().unwrap().starts, 2, "{control:?}: no third helper, so no control for the control");
            assert!(!attempt_file(&store, KEY).exists(), "{control:?}");
            assert!(!has_failed_record(&store, KEY), "{control:?}");
            assert_eq!(count_files(&store, "png"), 0, "{control:?}");
            assert_eq!(session.transient.len(), 1, "{control:?}: only the key that failed first is counted");
            assert_eq!(session.transient.get(&(request(KEY).package_prefix, KEY.to_string())), Some(&1), "{control:?}");
        }
    }

    #[test]
    fn a_control_helper_that_dies_counts_as_a_death_and_one_that_cannot_start_does_too() {
        let (mut session, _log, _store) = start_session("control-death", vec![plan(&[Behavior::Crash]), plan(&[Behavior::Crash])]);
        with_control(&mut session);
        assert!(failed(&session.render(&request(KEY))));
        assert_eq!(session.deaths, 2);

        let (mut session, log, _store) = start_session("control-no-start", vec![plan(&[Behavior::Crash])]);
        with_control(&mut session);
        assert!(failed(&session.render(&request(KEY))));
        assert_eq!(log.lock().unwrap().starts, 2);
        assert_eq!(session.deaths, 2);
    }

    #[test]
    fn a_control_that_times_out_is_transient() {
        let (mut session, log, store) = start_session("control-timeout", vec![plan(&[Behavior::Crash]), plan(&[Behavior::Hang])]);
        with_control(&mut session);
        session.config.control_timeout = Duration::from_millis(300);
        let started = Instant::now();
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(started.elapsed() >= Duration::from_millis(250), "{:?}", started.elapsed());
        assert_eq!(log.lock().unwrap().killed, 2, "the control helper was killed");
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn a_stop_request_during_the_control_ends_the_render_as_cancelled_with_no_attempt() {
        let (mut session, log, store) = start_session("control-stop", vec![plan(&[Behavior::Crash]), plan(&[Behavior::Hang])]);
        with_control(&mut session);
        session.config.control_timeout = Duration::from_secs(30);
        let flag = session.stop_flag();
        let started = Instant::now();
        let outcome = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                flag.store(true, Ordering::SeqCst);
            });
            session.render(&request(KEY))
        });
        assert_eq!(outcome, RenderOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(log.lock().unwrap().killed, 2);
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
        assert_eq!(count_files(&store, "tmp"), 0);
    }

    #[test]
    fn an_attempt_that_cannot_be_written_is_a_transient_failure_so_the_key_is_given_up() {
        let plans = (0..3).map(|_| plan(&[Behavior::Fail(PreviewBlame::Package)])).collect();
        let (mut session, _log, store) = start_session("attempt-io-error", plans);
        // A folder where the record belongs makes writing it fail.
        std::fs::create_dir_all(attempt_file(&store, KEY)).unwrap();
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        assert!(failed(&session.render(&request)));
        assert!(matches!(session.render(&request), RenderOutcome::GivenUp(_)));
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn a_failed_record_that_cannot_be_written_is_a_transient_failure() {
        let (mut session, _log, store) = start_session("failed-io-error", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        write_attempt(&store, KEY, "GPU A", SystemTime::now() - Duration::from_secs(60));
        let failed_path = store.root.join(format!("{}-{KEY}.failed", request(KEY).package_prefix));
        std::fs::create_dir_all(&failed_path).unwrap();
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert_eq!(session.transient.get(&(request(KEY).package_prefix, KEY.to_string())), Some(&1));
    }

    #[test]
    fn an_attempt_dated_far_in_the_future_counts_as_absent_when_a_package_fails() {
        let (mut session, _log, store) = start_session("attempt-future", vec![plan(&[Behavior::Fail(PreviewBlame::Package)])]);
        write_attempt(&store, KEY, "GPU A", SystemTime::now() + Duration::from_secs(3 * 24 * 60 * 60));
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(!has_failed_record(&store, KEY));
        let at = read_attempt(&store, KEY).unwrap()["at"].as_u64().unwrap();
        assert!(at < unix_nanos(SystemTime::now() + Duration::from_secs(60)), "the record was written again with the time now");
    }

    #[test]
    fn the_cooldown_doubles_for_each_one_in_a_row_up_to_fifteen_minutes() {
        let (mut session, _log, _store) = start_session("cooldown-grows", vec![]);
        session.config.cooldown = Duration::from_secs(60);
        let lengths: Vec<u64> = (0..7).map(|streak| session.cooldown_for(streak).as_secs()).collect();
        assert_eq!(lengths, [60, 120, 240, 480, 900, 900, 900]);
        assert_eq!(session.cooldown_for(u32::MAX), MAX_COOLDOWN);

        session.start_cooldown("first".into());
        let left = session.cooldown.as_ref().unwrap().0.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_secs(55) && left <= Duration::from_secs(60), "{left:?}");
        session.start_cooldown("second".into());
        let left = session.cooldown.as_ref().unwrap().0.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_secs(115) && left <= Duration::from_secs(120), "{left:?}");
        assert_eq!(session.cooldown_streak, 2);
    }

    #[test]
    fn a_published_job_resets_the_cooldown_streak() {
        let (mut session, _log, _store) = start_session("cooldown-reset", vec![plan(&[Behavior::Done])]);
        session.cooldown_streak = 3;
        assert_eq!(session.render(&request(KEY)), RenderOutcome::Published { is_static: false });
        assert_eq!(session.cooldown_streak, 0);
        assert_eq!(session.cooldown_for(session.cooldown_streak), session.config.cooldown);
    }

    #[test]
    fn a_failed_write_to_the_helper_blames_nobody_and_the_next_job_gets_a_new_helper() {
        let (mut session, log, store) = start_session("send-fails", vec![plan(&[Behavior::SendFails]), plan(&[Behavior::Done])]);
        let request = request(KEY);
        assert!(failed(&session.render(&request)));
        assert_eq!(log.lock().unwrap().killed, 1);
        assert_eq!(session.deaths, 1);
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
        assert_eq!(session.render(&request), RenderOutcome::Published { is_static: false });
        assert_eq!(log.lock().unwrap().starts, 2);
    }

    #[test]
    fn a_helper_found_dead_before_a_job_is_replaced_and_the_key_is_not_blamed() {
        let (mut session, log, store) = start_session("dead-before", vec![Plan { dead_after_ready: true, ..plan(&[]) }, plan(&[Behavior::Done])]);
        let request = request(KEY);
        assert_eq!(session.render(&request), RenderOutcome::Published { is_static: false });
        let log = log.lock().unwrap();
        assert_eq!(log.starts, 2);
        assert_eq!(log.killed, 1);
        assert_eq!(log.sent.len(), 1, "the dead helper was sent nothing");
        assert!(!attempt_file(&store, KEY).exists());
        assert!(!has_failed_record(&store, KEY));
    }

    #[test]
    fn an_invalid_line_without_an_id_ends_the_job_at_once() {
        let (mut session, log, store) = start_session("invalid-no-id", vec![plan(&[Behavior::InvalidNoId])]);
        session.config.still_timeout = Duration::from_secs(30);
        let started = Instant::now();
        let outcome = session.render(&request(KEY));
        assert!(failed(&outcome), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert_eq!(log.lock().unwrap().killed, 0, "the helper is not blamed");
        assert!(!attempt_file(&store, KEY).exists());
    }

    #[test]
    fn a_failure_under_another_adapter_or_version_does_not_count_so_the_key_is_retried() {
        let (mut session, log, store) = start_session("other-adapter", vec![plan(&[Behavior::Done])]);
        let request = request(KEY);
        store.mark_failed_on(&request.package_prefix, KEY, "GPU B", VERSION, "old").unwrap();
        assert_eq!(session.render(&request), RenderOutcome::Published { is_static: false });
        assert_eq!(log.lock().unwrap().sent.len(), 1);

        let (mut session, log, store) = start_session("same-adapter", vec![plan(&[Behavior::Done])]);
        store.mark_failed_on(&request.package_prefix, KEY, "GPU A", VERSION, "it broke").unwrap();
        assert_eq!(session.render(&request), RenderOutcome::AlreadyFailed("it broke".into()));
        assert!(log.lock().unwrap().sent.is_empty());

        let (mut session, _log, store) = start_session("other-version", vec![plan(&[Behavior::Done])]);
        store.mark_failed_on(&request.package_prefix, KEY, "GPU A", "0.0.1", "old").unwrap();
        assert_eq!(session.render(&request), RenderOutcome::Published { is_static: false });
    }

    #[test]
    fn a_failed_key_starts_no_helper_once_the_device_is_known() {
        let (mut session, log, store) = start_session("failed-no-start", vec![plan(&[Behavior::Fail(PreviewBlame::Package)]), plan(&[])]);
        assert!(failed(&session.render(&request(OTHER_KEY))));
        assert!(session.replace_helper, "the next job would replace the helper");
        assert_eq!(session.device(), Some("GPU A"));
        store.mark_failed_on(&request(KEY).package_prefix, KEY, "GPU A", VERSION, "it broke").unwrap();
        assert_eq!(session.render(&request(KEY)), RenderOutcome::AlreadyFailed("it broke".into()));
        let log = log.lock().unwrap();
        assert_eq!(log.starts, 1);
        assert_eq!(log.killed, 0);
    }

    #[test]
    fn device_and_output_failures_never_write_failed_and_a_key_is_given_up_after_three() {
        for blame in [PreviewBlame::Device, PreviewBlame::Output] {
            let plans = (0..3).map(|_| plan(&[Behavior::Fail(blame), Behavior::Fail(blame), Behavior::Fail(blame)])).collect();
            let (mut session, log, store) = start_session(&format!("transient-{blame:?}"), plans);
            let request = request(KEY);
            assert!(failed(&session.render(&request)));
            assert!(failed(&session.render(&request)));
            assert!(matches!(session.render(&request), RenderOutcome::GivenUp(_)));
            let sent = log.lock().unwrap().sent.len();
            assert_eq!(sent, 3);
            assert!(matches!(session.render(&request), RenderOutcome::GivenUp(_)));
            assert_eq!(log.lock().unwrap().sent.len(), sent, "nothing more is sent for the key");
            assert!(!has_failed_record(&store, KEY), "{blame:?}");
            assert!(!attempt_file(&store, KEY).exists(), "{blame:?} records no attempt");
        }
    }

    #[test]
    fn a_given_up_key_does_not_stop_the_others() {
        let plans = (0..3).map(|_| plan(&[Behavior::Fail(PreviewBlame::Device)])).chain([plan(&[Behavior::Done])]).collect();
        let (mut session, _log, _store) = start_session("given-up-others", plans);
        for _ in 0..3 {
            session.render(&request(KEY));
        }
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::GivenUp(_)));
        assert_eq!(session.render(&request(OTHER_KEY)), RenderOutcome::Published { is_static: false });
    }

    #[test]
    fn five_helpers_dying_in_a_row_start_a_cooldown_that_ends_and_a_good_job_resets_the_count() {
        let crashing = |count: usize| (0..count).map(|_| plan(&[Behavior::Crash])).collect::<Vec<_>>();

        let mut plans = crashing(5);
        plans.push(plan(&[Behavior::Done]));
        let (mut session, log, _store) = start_session("deaths", plans);
        for n in 0..5 {
            assert!(failed(&session.render(&request(&key(n)))), "{n}");
        }
        let RenderOutcome::CoolingDown(reason) = session.render(&request(&key(5))) else { panic!("the session went on") };
        assert!(reason.contains('5'), "{reason}");
        assert_eq!(log.lock().unwrap().starts, 5, "nothing is started while cooling down");
        assert!(session.stopped.is_none(), "a cooldown is not a stop");
        thread::sleep(COOLDOWN_PASSED);
        assert_eq!(session.render(&request(&key(5))), RenderOutcome::Published { is_static: false });
        assert_eq!(session.deaths, 0);

        let mut plans = crashing(4);
        plans.push(plan(&[Behavior::Done, Behavior::Crash]));
        plans.extend(crashing(3));
        let (mut session, _log, _store) = start_session("deaths-reset", plans);
        for n in 0..4 {
            assert!(failed(&session.render(&request(&key(n)))));
        }
        assert_eq!(session.render(&request(&key(10))), RenderOutcome::Published { is_static: false });
        for n in 20..24 {
            assert!(failed(&session.render(&request(&key(n)))), "{n}");
        }
        assert!(session.cooldown.is_none());
    }

    #[test]
    fn eight_device_failures_in_a_row_across_keys_start_a_cooldown_that_ends() {
        let mut plans: Vec<Plan> = (0..8).map(|_| plan(&[Behavior::Fail(PreviewBlame::Device)])).collect();
        plans.push(plan(&[Behavior::Done]));
        let (mut session, log, store) = start_session("device-cooldown", plans);
        for n in 0..8 {
            assert!(failed(&session.render(&request(&key(n)))), "{n}");
        }
        let RenderOutcome::CoolingDown(reason) = session.render(&request(&key(8))) else { panic!("the session went on") };
        assert!(reason.contains('8'), "{reason}");
        assert_eq!(log.lock().unwrap().starts, 8, "nothing is started while cooling down");
        assert!(!has_failed_record(&store, &key(0)));
        thread::sleep(COOLDOWN_PASSED);
        assert_eq!(session.render(&request(&key(8))), RenderOutcome::Published { is_static: false });
        assert_eq!(log.lock().unwrap().starts, 9);
        assert_eq!(session.device_failures, 0);
    }

    #[test]
    fn a_published_job_resets_the_device_failure_count() {
        let mut plans: Vec<Plan> = (0..7).map(|_| plan(&[Behavior::Fail(PreviewBlame::Device)])).collect();
        plans.push(plan(&[Behavior::Done, Behavior::Fail(PreviewBlame::Device)]));
        plans.extend((0..6).map(|_| plan(&[Behavior::Fail(PreviewBlame::Device)])));
        let (mut session, _log, _store) = start_session("device-reset", plans);
        for n in 0..7 {
            assert!(failed(&session.render(&request(&key(n)))));
        }
        assert_eq!(session.render(&request(&key(50))), RenderOutcome::Published { is_static: false });
        for n in 100..107 {
            assert!(failed(&session.render(&request(&key(n)))), "{n}");
        }
        assert!(session.cooldown.is_none());
    }

    #[test]
    fn a_helper_that_cannot_start_counts_as_a_death() {
        let (mut session, log, _store) = start_session("no-start", vec![]);
        for _ in 0..5 {
            assert!(failed(&session.render(&request(KEY))));
        }
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::CoolingDown(_)));
        assert_eq!(log.lock().unwrap().starts, 5);
    }

    #[test]
    fn a_fatal_event_while_starting_is_a_death_with_its_message() {
        let fatal = Plan { first: Some(PreviewEvent::Fatal { message: "no adapter".into() }), ..plan(&[]) };
        let (mut session, _log, _store) = start_session("fatal", vec![fatal]);
        let RenderOutcome::Failed { reason, recorded } = session.render(&request(KEY)) else { panic!("did not fail") };
        assert!(!recorded);
        assert!(reason.contains("no adapter"), "{reason}");
        assert_eq!(session.deaths, 1);
    }

    #[test]
    fn a_stop_request_ends_a_waiting_job_without_blaming_the_package() {
        let (mut session, log, store) = start_session("stop", vec![plan(&[Behavior::Hang])]);
        session.config.still_timeout = Duration::from_secs(30);
        let flag = session.stop_flag();
        let started = Instant::now();
        let outcome = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                flag.store(true, Ordering::SeqCst);
            });
            session.render(&request(KEY))
        });
        assert_eq!(outcome, RenderOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(log.lock().unwrap().killed, 1);
        assert!(!attempt_file(&store, KEY).exists());
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::Cancelled));
    }

    #[test]
    fn shutdown_closes_the_helper_and_later_jobs_are_refused() {
        let (mut session, log, _store) = start_session("shutdown", vec![plan(&[Behavior::Done])]);
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::Published { .. }));
        session.shutdown();
        assert_eq!(log.lock().unwrap().closed, 1);
        assert_eq!(log.lock().unwrap().killed, 0);
        assert!(matches!(session.render(&request(KEY)), RenderOutcome::Stopped(_)));
    }
}
