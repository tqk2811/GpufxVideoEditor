//! Autosave and crash recovery. No window here.
//!
//! While a project has unsaved changes, a copy of it is written now and then to
//! `%LOCALAPPDATA%\gpufx-editor\recovery\<id>\` (`project.json` with absolute sources, and
//! `meta.json`), never over the user's own file. Each running editor keeps a session marker,
//! `recovery\session-<pid>.json`, holding its process id and creation time, and removes it on a
//! clean exit together with its entry. An entry whose owner is no longer running, or ran without
//! removing its marker, is orphaned: the editor that starts next offers to restore it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::editor::EditorState;
use crate::project_file::write_file_atomically;

/// How often a changed project is written, unless the user chose otherwise.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;
/// An orphaned entry older than this is deleted when the editor starts.
pub const KEEP_SECS: u64 = 14 * 24 * 60 * 60;
/// How soon an autosave that had to wait for a drag or a dialog is tried again.
const RETRY: Duration = Duration::from_secs(2);
const RECOVERY_FOLDER: &str = "recovery";
const PROJECT_FILE: &str = "project.json";
const META_FILE: &str = "meta.json";
const SESSION_PREFIX: &str = "session-";

/// A running process, told apart from a later one that reuses its id by its creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    /// The creation time the system reports, in its own units.
    pub started: u64,
}

/// What the system says about a process id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// It runs, created at this time.
    Running(u64),
    /// No process with this id runs.
    Gone,
    /// It may run, but the system will not say more (access denied): taken as alive.
    Unknown,
}

/// Asks the system about processes; tests give their own answers.
pub trait ProcessProbe {
    fn state(&self, pid: u32) -> ProcessState;
}

impl ProcessInfo {
    pub fn is_running(&self, probe: &dyn ProcessProbe) -> bool {
        match probe.state(self.pid) {
            ProcessState::Running(started) => started == self.started,
            ProcessState::Gone => false,
            ProcessState::Unknown => true,
        }
    }
}

/// The processes of this machine.
pub struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn state(&self, pid: u32) -> ProcessState {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        /// `STILL_ACTIVE`: the exit code of a process that has not ended.
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: the handle is checked, used only here and closed before returning; the out
        // parameters are valid places for the calls to write to.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // Only "no such process" (invalid parameter) means it is gone.
                return if GetLastError() == ERROR_ACCESS_DENIED { ProcessState::Unknown } else { ProcessState::Gone };
            }
            let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            let mut code = 0u32;
            let times = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
            let status = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            if times == 0 || status == 0 {
                return ProcessState::Unknown;
            }
            if code != STILL_ACTIVE {
                return ProcessState::Gone;
            }
            ProcessState::Running((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        }
    }
}

/// How many more times a claim retries a folder that is still there but cannot be renamed.
const CLAIM_RETRIES: u32 = 3;
const CLAIM_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Why an entry could not be claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// The entry is gone: another editor restored or discarded it.
    Taken(String),
    /// The entry is still there but could not be renamed; it stays offered.
    Locked(String),
    /// Autosave is off in this window.
    Off,
}

/// This process.
pub fn current_process() -> ProcessInfo {
    let pid = std::process::id();
    let started = match SystemProbe.state(pid) {
        ProcessState::Running(started) => started,
        _ => 0,
    };
    ProcessInfo { pid, started }
}

/// Seconds since 1970, the time kept in `meta.json`.
pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// What `meta.json` says about an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The project file the autosave belongs to; `None` for a project never saved.
    pub original: Option<PathBuf>,
    pub revision: u64,
    /// When it was written, in seconds since 1970.
    pub saved_at: u64,
    pub pid: u32,
    pub process_started: u64,
    pub app_version: String,
}

impl Meta {
    pub fn owner(&self) -> ProcessInfo {
        ProcessInfo { pid: self.pid, started: self.process_started }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `path` absolute, with backslashes and lower-cased, as Windows compares paths.
fn normalized(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// The entry of the project in `file`: the same for every spelling of the path.
pub fn id_for_path(file: &Path) -> String {
    format!("p-{}", hex(&Sha256::digest(normalized(file).as_bytes())[..8]))
}

/// The entry of a project never saved: distinct for each process, and within it for each `serial`.
pub fn untitled_id(me: ProcessInfo, nanos: u128, serial: u64) -> String {
    let seed = format!("{}:{}:{nanos}:{serial}", me.pid, me.started);
    format!("u-{}", hex(&Sha256::digest(seed.as_bytes())[..8]))
}

/// The folder name of entry `base` owned by `owner`: `<base>.<pid>-<started>` in hex, then
/// `-c<claim>` for an entry taken over from a dead editor. The owner is in the name so that a
/// rename hands an entry over at once, and no two editors ever write to the same folder.
pub fn owned_id(base: &str, owner: ProcessInfo, claim: Option<u64>) -> String {
    let base = base.split_once('.').map_or(base, |(base, _)| base);
    match claim {
        Some(claim) => format!("{base}.{:x}-{:x}-c{claim}", owner.pid, owner.started),
        None => format!("{base}.{:x}-{:x}", owner.pid, owner.started),
    }
}

/// The owner named by an entry folder of [`owned_id`] (or its `.tmp` twin).
pub fn owner_of(id: &str) -> Option<ProcessInfo> {
    let (_, rest) = id.split_once('.')?;
    let mut parts = rest.split(['-', '.']);
    let pid = u32::from_str_radix(parts.next()?, 16).ok()?;
    let started = u64::from_str_radix(parts.next()?, 16).ok()?;
    Some(ProcessInfo { pid, started })
}

const TMP_SUFFIX: &str = ".tmp";

/// An entry another editor left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub id: String,
    pub meta: Meta,
    /// Whether the original file was changed after the autosave was written.
    pub original_newer: bool,
}

/// The folder of the entries and session markers.
#[derive(Debug, Clone)]
pub struct RecoveryStore {
    root: PathBuf,
}

impl RecoveryStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `%LOCALAPPDATA%\gpufx-editor\recovery`, when the variable is set.
    pub fn default_location() -> Option<Self> {
        crate::project_file::user_package_root().map(|folder| Self::new(folder.join(RECOVERY_FOLDER)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn entry_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Where the autosaved project of entry `id` is.
    pub fn project_file(&self, id: &str) -> PathBuf {
        self.entry_dir(id).join(PROJECT_FILE)
    }

    fn session_file(&self, pid: u32) -> PathBuf {
        self.root.join(format!("{SESSION_PREFIX}{pid}.json"))
    }

    /// Says that `me` is running.
    pub fn write_session(&self, me: ProcessInfo) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| format!("creating {} failed: {e}", self.root.display()))?;
        let text = serde_json::to_string(&me).map_err(|e| e.to_string())?;
        write_file_atomically(&self.session_file(me.pid), text.as_bytes())
    }

    fn read_session(&self, pid: u32) -> Option<ProcessInfo> {
        let text = std::fs::read_to_string(self.session_file(pid)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Removes the marker of `me`, unless a later process with the same id wrote its own.
    pub fn remove_session(&self, me: ProcessInfo) {
        if self.read_session(me.pid).is_none_or(|stored| stored == me) {
            let _ = std::fs::remove_file(self.session_file(me.pid));
        }
    }

    /// Whether `owner` is still running and has not ended its session.
    fn session_alive(&self, owner: ProcessInfo, probe: &dyn ProcessProbe) -> bool {
        self.read_session(owner.pid) == Some(owner) && owner.is_running(probe)
    }

    /// Writes entry `id`. The first write fills `<id>.tmp` and renames it into place, so an
    /// entry never exists without its meta; later writes replace the project, then the meta.
    pub fn write_entry(&self, id: &str, project_text: &str, meta: &Meta) -> Result<(), String> {
        let dir = self.entry_dir(id);
        let meta_text = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
        if dir.join(META_FILE).is_file() {
            write_file_atomically(&dir.join(PROJECT_FILE), project_text.as_bytes())?;
            return write_file_atomically(&dir.join(META_FILE), meta_text.as_bytes());
        }
        let tmp = self.root.join(format!("{id}{TMP_SUFFIX}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("creating {} failed: {e}", tmp.display()))?;
        write_file_atomically(&tmp.join(PROJECT_FILE), project_text.as_bytes())?;
        write_file_atomically(&tmp.join(META_FILE), meta_text.as_bytes())?;
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::rename(&tmp, &dir).map_err(|e| format!("moving {} into place failed: {e}", tmp.display()))
    }

    /// Takes entry `from`, left by a dead editor, over as `to` by renaming its folder. The rename
    /// is atomic, so when two editors try, one fails: the entry is gone or already taken.
    ///
    /// A folder that is still there but cannot be renamed (a file in it held open by an antivirus,
    /// the search indexer or another editor reading it) is tried a few more times, then reported as
    /// [`ClaimError::Locked`], not as taken.
    pub fn claim(&self, from: &str, to: &str) -> Result<(), ClaimError> {
        let source = self.entry_dir(from);
        let mut attempt = 0;
        loop {
            match std::fs::rename(&source, self.entry_dir(to)) {
                Ok(()) => return Ok(()),
                Err(e) if !source.exists() => return Err(ClaimError::Taken(format!("{from}: {e}"))),
                Err(e) if attempt >= CLAIM_RETRIES => return Err(ClaimError::Locked(format!("{from}: {e}"))),
                Err(_) => {
                    attempt += 1;
                    std::thread::sleep(CLAIM_RETRY_DELAY);
                }
            }
        }
    }

    pub fn remove_entry(&self, id: &str) {
        let dir = self.entry_dir(id);
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                eprintln!("gpufx-editor: removing the autosave {} failed: {e}", dir.display());
            }
        }
    }

    pub fn read_meta(&self, id: &str) -> Option<Meta> {
        let text = std::fs::read_to_string(self.entry_dir(id).join(META_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn project_text(&self, id: &str) -> Result<String, String> {
        let file = self.project_file(id);
        std::fs::read_to_string(&file).map_err(|e| format!("reading {}: {e}", file.display()))
    }

    /// The ids of the entry folders.
    fn entry_ids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        entries.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir())).map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()
    }

    /// Removes markers of sessions that ended without saying so.
    fn prune_sessions(&self, probe: &dyn ProcessProbe) {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(pid) = name.strip_prefix(SESSION_PREFIX).and_then(|rest| rest.strip_suffix(".json")).and_then(|pid| pid.parse::<u32>().ok()) else { continue };
            if self.read_session(pid).is_none_or(|stored| !stored.is_running(probe)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// The entries no running editor owns, newest first. Orphans older than [`KEEP_SECS`] and
    /// broken entries as old are deleted, and so are the markers of dead sessions. `now` is in
    /// seconds since 1970.
    pub fn prune_and_list_orphans(&self, probe: &dyn ProcessProbe, now: u64) -> Vec<Orphan> {
        let mut orphans = Vec::new();
        for id in self.entry_ids() {
            // The folder name says who owns the entry; the meta may still name an earlier owner.
            let owner = owner_of(&id);
            if owner.is_some_and(|owner| self.session_alive(owner, probe)) {
                continue;
            }
            if id.ends_with(TMP_SUFFIX) {
                // A first write its owner did not finish.
                self.remove_entry(&id);
                continue;
            }
            match self.read_meta(&id) {
                Some(meta) if owner.is_none() && self.session_alive(meta.owner(), probe) => {}
                Some(meta) if now.saturating_sub(meta.saved_at) > KEEP_SECS => self.remove_entry(&id),
                Some(meta) => {
                    let original_newer = meta.original.as_deref().and_then(modified_secs).is_some_and(|modified| modified > meta.saved_at);
                    orphans.push(Orphan { id, meta, original_newer });
                }
                // A write that died before its meta: kept a while, in case its owner still runs.
                None => {
                    if modified_secs(&self.entry_dir(&id)).is_some_and(|modified| now.saturating_sub(modified) > KEEP_SECS) {
                        self.remove_entry(&id);
                    }
                }
            }
        }
        self.prune_sessions(probe);
        orphans.sort_by(|first, second| second.meta.saved_at.cmp(&first.meta.saved_at).then_with(|| first.id.cmp(&second.id)));
        orphans
    }
}

/// When `path` was last changed, in seconds since 1970.
fn modified_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified.duration_since(UNIX_EPOCH).ok().map(|elapsed| elapsed.as_secs())
}

/// When the next autosave is due.
#[derive(Debug, Clone)]
pub struct AutosaveTimer {
    /// Zero turns autosave off.
    interval: Duration,
    last: Instant,
    /// The revision last written, so an unchanged project is not written again.
    written_revision: Option<u64>,
}

impl AutosaveTimer {
    pub fn new(interval_secs: u64, now: Instant) -> Self {
        Self { interval: Duration::from_secs(interval_secs), last: now, written_revision: None }
    }

    pub fn set_interval(&mut self, interval_secs: u64) {
        self.interval = Duration::from_secs(interval_secs);
    }

    fn wanted(&self, revision: u64, dirty: bool) -> bool {
        !self.interval.is_zero() && dirty && self.written_revision != Some(revision)
    }

    /// Whether to write now. `blocked` is a drag, a live edit or a dialog in the way.
    pub fn due(&self, now: Instant, revision: u64, dirty: bool, blocked: bool) -> bool {
        self.wanted(revision, dirty) && !blocked && now >= self.last + self.interval
    }

    /// When to look again; `None` while nothing is to be written.
    pub fn next_check(&self, now: Instant, revision: u64, dirty: bool) -> Option<Instant> {
        self.wanted(revision, dirty).then(|| (self.last + self.interval).max(now + RETRY))
    }

    pub fn wrote(&mut self, now: Instant, revision: u64) {
        self.last = now;
        self.written_revision = Some(revision);
    }

    pub fn forget(&mut self) {
        self.written_revision = None;
    }
}

enum Job {
    Write { id: String, text: String, meta: Meta },
    Remove(String),
}

/// Writes and removes entries in order on a thread of its own.
struct Writer {
    jobs: Sender<Job>,
    failures: Receiver<String>,
    thread: JoinHandle<()>,
}

impl Writer {
    fn spawn(store: RecoveryStore) -> Option<Self> {
        let (jobs, job_receiver) = channel::<Job>();
        let (failure_sender, failures) = channel();
        let thread = std::thread::Builder::new()
            .name("autosave".to_owned())
            .spawn(move || {
                for job in job_receiver {
                    match job {
                        Job::Write { id, text, meta } => {
                            if let Err(message) = store.write_entry(&id, &text, &meta) {
                                let _ = failure_sender.send(message);
                            }
                        }
                        Job::Remove(id) => store.remove_entry(&id),
                    }
                }
            })
            .map_err(|e| eprintln!("gpufx-editor: the autosave thread could not start: {e}"))
            .ok()?;
        Some(Self { jobs, failures, thread })
    }
}

/// The autosave of one editor window: when to write, which entry is its own, and what to clean up.
pub struct Autosave {
    store: Option<RecoveryStore>,
    me: ProcessInfo,
    timer: AutosaveTimer,
    /// The entry of the project while it has no file.
    untitled: String,
    untitled_serial: u64,
    /// A restored entry of a project file, written to while that file is the project's:
    /// its normalised path and the entry.
    adopted: Option<(String, String)>,
    /// How many entries this process has claimed, for unique claimed names.
    claims: u64,
    /// The entry this window last wrote, which it removes once the project is clean.
    written: Option<String>,
    writer: Option<Writer>,
}

impl Autosave {
    /// Starts the session of `me` in `store`; `None` turns everything off.
    pub fn start(store: Option<RecoveryStore>, me: ProcessInfo, interval_secs: u64, now: Instant) -> Self {
        // Without its own start time another editor would take this live one for dead and offer its
        // entries as orphans, so autosave stays off.
        let store = store.filter(|_| me.started != 0 || {
            eprintln!("gpufx-editor: autosave is off: the start time of this process is unknown");
            false
        });
        let store = store.filter(|store| match store.write_session(me) {
            Ok(()) => true,
            Err(message) => {
                eprintln!("gpufx-editor: autosave is off: {message}");
                false
            }
        });
        let writer = store.clone().and_then(Writer::spawn);
        let mut autosave = Self { store, me, timer: AutosaveTimer::new(interval_secs, now), untitled: String::new(), untitled_serial: 0, adopted: None, claims: 0, written: None, writer };
        autosave.untitled = autosave.next_untitled();
        autosave
    }

    fn next_untitled(&mut self) -> String {
        self.untitled_serial += 1;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
        owned_id(&untitled_id(self.me, nanos, self.untitled_serial), self.me, None)
    }

    pub fn store(&self) -> Option<&RecoveryStore> {
        self.store.as_ref()
    }

    pub fn set_interval(&mut self, interval_secs: u64) {
        self.timer.set_interval(interval_secs);
    }

    /// The entry for the project in `file`, or for the unsaved one. Always named for this
    /// process, so it never meets an entry of another editor.
    pub fn id_for(&self, file: Option<&Path>) -> String {
        match file {
            None => self.untitled.clone(),
            Some(file) => {
                let key = normalized(file);
                match &self.adopted {
                    Some((path, id)) if *path == key => id.clone(),
                    _ => owned_id(&id_for_path(file), self.me, None),
                }
            }
        }
    }

    /// Takes the offered entry `id` over for this process; its new id, or why it could not be
    /// taken (another editor took or removed it first).
    pub fn claim(&mut self, id: &str) -> Result<String, ClaimError> {
        let Some(store) = &self.store else { return Err(ClaimError::Off) };
        self.claims += 1;
        let claimed = owned_id(id, self.me, Some(self.claims));
        store.claim(id, &claimed)?;
        Ok(claimed)
    }

    /// Hands a claimed entry back under its old id, after a restore that failed.
    pub fn unclaim(&self, claimed: &str, id: &str) {
        if let Some(store) = &self.store {
            let _ = store.claim(claimed, id);
        }
    }

    /// Claims the offered entry `id` and deletes it; an error when another editor took it first.
    pub fn discard(&mut self, id: &str) -> Result<(), ClaimError> {
        let claimed = self.claim(id)?;
        if let Some(store) = &self.store {
            store.remove_entry(&claimed);
        }
        Ok(())
    }

    /// Makes the claimed entry `claimed`, restored for `original` (`None`: never saved), the
    /// entry this window writes to and removes once clean, and writes it at once.
    pub fn adopt(&mut self, claimed: &str, original: Option<&Path>, state: &EditorState, now: Instant) {
        match original {
            None => self.untitled = claimed.to_owned(),
            Some(file) => self.adopted = Some((normalized(file), claimed.to_owned())),
        }
        self.timer.forget();
        if let Some(old) = self.written.replace(claimed.to_owned()).filter(|old| old != claimed) {
            self.send(Job::Remove(old));
        }
        self.write(state, now);
    }

    fn send(&self, job: Job) {
        if let Some(writer) = &self.writer {
            let _ = writer.jobs.send(job);
        }
    }

    /// Serialises the project here and leaves the writing to the thread.
    fn write(&mut self, state: &EditorState, now: Instant) {
        if self.writer.is_none() {
            return;
        }
        let id = self.id_for(state.project_file());
        if let Some(old) = self.written.take().filter(|old| *old != id) {
            self.send(Job::Remove(old));
        }
        let text = match serde_json::to_string_pretty(state.project()) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("gpufx-editor: serialising the autosave failed: {e}");
                return;
            }
        };
        let meta = Meta { original: state.project_file().map(Path::to_path_buf), revision: state.revision(), saved_at: unix_now(), pid: self.me.pid, process_started: self.me.started, app_version: env!("CARGO_PKG_VERSION").to_owned() };
        self.send(Job::Write { id: id.clone(), text, meta });
        self.written = Some(id);
        self.timer.wrote(now, state.revision());
    }

    /// Writes when due, removes this window's entry once the project is clean, and says when to
    /// look again. `blocked` is a drag, a live edit or a dialog in the way.
    pub fn tick(&mut self, state: &EditorState, blocked: bool, now: Instant) -> Option<Instant> {
        let dirty = state.is_dirty();
        if !dirty {
            // Saved, opened, replaced by a new project or discarded: nothing left to recover.
            if let Some(id) = self.written.take() {
                self.send(Job::Remove(id));
            }
            if state.project_file().is_none() && self.timer.written_revision.is_some() {
                // A new unsaved project gets an entry of its own.
                self.untitled = self.next_untitled();
            }
            self.timer.forget();
            return None;
        }
        if self.timer.due(now, state.revision(), dirty, blocked) {
            self.write(state, now);
        }
        self.timer.next_check(now, state.revision(), dirty)
    }

    /// A failure of the writer thread, to show once.
    pub fn take_failure(&self) -> Option<String> {
        self.writer.as_ref().and_then(|writer| writer.failures.try_recv().ok())
    }

    /// The app is closing on purpose: the entry of this window and its session marker go.
    pub fn finish_clean(&mut self) {
        if let Some(id) = self.written.take() {
            self.send(Job::Remove(id));
        }
        if let Some(writer) = self.writer.take() {
            drop(writer.jobs);
            let _ = writer.thread.join();
        }
        if let Some(store) = &self.store {
            store.remove_session(self.me);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeProbe(HashMap<u32, u64>);

    impl ProcessProbe for FakeProbe {
        fn state(&self, pid: u32) -> ProcessState {
            match self.0.get(&pid) {
                Some(&u64::MAX) => ProcessState::Unknown,
                Some(&started) => ProcessState::Running(started),
                None => ProcessState::Gone,
            }
        }
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-recovery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta(owner: ProcessInfo, saved_at: u64, original: Option<PathBuf>) -> Meta {
        Meta { original, revision: 7, saved_at, pid: owner.pid, process_started: owner.started, app_version: "0.1.0".to_owned() }
    }

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn a_path_id_is_stable_across_spellings_and_differs_between_files() {
        let first = id_for_path(Path::new(r"C:\Work\Project.json"));
        assert_eq!(first, id_for_path(Path::new("c:/work/project.json")));
        assert_eq!(first, id_for_path(Path::new(r"C:\Work\.\Project.json")));
        assert_ne!(first, id_for_path(Path::new(r"C:\Work\other.json")));
        assert!(first.starts_with("p-") && first.len() == 18, "{first}");
    }

    #[test]
    fn untitled_ids_differ_per_process_and_serial() {
        let me = ProcessInfo { pid: 10, started: 5 };
        let other = ProcessInfo { pid: 11, started: 5 };
        assert_ne!(untitled_id(me, 1, 1), untitled_id(other, 1, 1));
        assert_ne!(untitled_id(me, 1, 1), untitled_id(me, 1, 2));
        assert_eq!(untitled_id(me, 1, 1), untitled_id(me, 1, 1));
        assert!(untitled_id(me, 1, 1).starts_with("u-"));
    }

    #[test]
    fn meta_reads_back_as_written() {
        let store = RecoveryStore::new(folder("meta"));
        let written = meta(ProcessInfo { pid: 3, started: 99 }, NOW, Some(PathBuf::from(r"D:\a\b.json")));
        store.write_entry("p-1", "{}", &written).unwrap();
        assert_eq!(store.read_meta("p-1"), Some(written));
        assert_eq!(store.project_text("p-1").unwrap(), "{}");
        let untitled = meta(ProcessInfo { pid: 3, started: 99 }, NOW, None);
        store.write_entry("u-1", "{}", &untitled).unwrap();
        assert_eq!(store.read_meta("u-1"), Some(untitled));
    }

    #[test]
    fn only_entries_of_ended_sessions_are_orphans() {
        let store = RecoveryStore::new(folder("orphans"));
        let alive = ProcessInfo { pid: 1, started: 100 };
        let dead = ProcessInfo { pid: 2, started: 200 };
        let reused = ProcessInfo { pid: 3, started: 300 };
        let clean = ProcessInfo { pid: 4, started: 400 };
        for owner in [alive, dead, reused] {
            store.write_session(owner).unwrap();
        }
        store.write_entry("alive", "{}", &meta(alive, NOW - 10, None)).unwrap();
        store.write_entry("dead", "{}", &meta(dead, NOW - 20, None)).unwrap();
        store.write_entry("reused", "{}", &meta(reused, NOW - 30, None)).unwrap();
        store.write_entry("clean", "{}", &meta(clean, NOW - 40, None)).unwrap();
        // Pid 3 now belongs to another process; pid 4 runs but ended its session.
        let probe = FakeProbe(HashMap::from([(1, 100), (3, 999), (4, 400)]));
        let orphans: Vec<String> = store.prune_and_list_orphans(&probe, NOW).into_iter().map(|orphan| orphan.id).collect();
        assert_eq!(orphans, ["dead", "reused", "clean"], "newest first, the live one left out");
        assert!(store.session_file(1).exists(), "a live marker stays");
        assert!(!store.session_file(2).exists() && !store.session_file(3).exists(), "dead markers go");
        assert!(store.entry_dir("dead").exists(), "listing deletes nothing young");
    }

    #[test]
    fn old_orphans_are_pruned_but_old_entries_of_live_sessions_stay() {
        let store = RecoveryStore::new(folder("prune"));
        let alive = ProcessInfo { pid: 1, started: 100 };
        let dead = ProcessInfo { pid: 2, started: 200 };
        store.write_session(alive).unwrap();
        store.write_entry("old-dead", "{}", &meta(dead, NOW - KEEP_SECS - 1, None)).unwrap();
        store.write_entry("young-dead", "{}", &meta(dead, NOW - KEEP_SECS + 1, None)).unwrap();
        store.write_entry("old-alive", "{}", &meta(alive, NOW - KEEP_SECS - 1, None)).unwrap();
        let probe = FakeProbe(HashMap::from([(1, 100)]));
        let orphans: Vec<String> = store.prune_and_list_orphans(&probe, NOW).into_iter().map(|orphan| orphan.id).collect();
        assert_eq!(orphans, ["young-dead"]);
        assert!(!store.entry_dir("old-dead").exists());
        assert!(store.entry_dir("old-alive").exists());
    }

    #[test]
    fn an_original_changed_after_the_autosave_is_reported_newer() {
        let dir = folder("newer");
        let store = RecoveryStore::new(dir.join("recovery"));
        let original = dir.join("p.json");
        std::fs::write(&original, "{}").unwrap();
        let modified = modified_secs(&original).unwrap();
        let dead = ProcessInfo { pid: 2, started: 200 };
        store.write_entry("older", "{}", &meta(dead, modified - 5, Some(original.clone()))).unwrap();
        store.write_entry("newer", "{}", &meta(dead, modified + 5, Some(original.clone()))).unwrap();
        let orphans = store.prune_and_list_orphans(&FakeProbe(HashMap::new()), modified + 10);
        let newer_than = |id: &str| orphans.iter().find(|orphan| orphan.id == id).unwrap().original_newer;
        assert!(newer_than("older"));
        assert!(!newer_than("newer"));
    }

    #[test]
    fn a_session_marker_of_a_later_process_with_the_same_id_is_kept() {
        let store = RecoveryStore::new(folder("marker"));
        let earlier = ProcessInfo { pid: 5, started: 1 };
        let later = ProcessInfo { pid: 5, started: 2 };
        store.write_session(later).unwrap();
        store.remove_session(earlier);
        assert!(store.session_file(5).exists());
        store.remove_session(later);
        assert!(!store.session_file(5).exists());
    }

    #[test]
    fn the_timer_waits_for_the_interval_a_change_and_nothing_in_the_way() {
        let start = Instant::now();
        let mut timer = AutosaveTimer::new(60, start);
        let later = start + Duration::from_secs(61);
        assert!(!timer.due(start + Duration::from_secs(59), 1, true, false), "too soon");
        assert!(!timer.due(later, 1, false, false), "clean");
        assert!(!timer.due(later, 1, true, true), "blocked");
        assert!(timer.due(later, 1, true, false));
        assert!(timer.next_check(later, 1, true).unwrap() >= later + RETRY, "a blocked save is retried, not spun on");
        timer.wrote(later, 1);
        assert!(!timer.due(later + Duration::from_secs(120), 1, true, false), "unchanged since written");
        assert_eq!(timer.next_check(later, 1, true), None);
        assert!(timer.due(later + Duration::from_secs(60), 2, true, false));
        timer.set_interval(0);
        assert!(!timer.due(later + Duration::from_secs(600), 3, true, false), "off");
        assert_eq!(timer.next_check(later, 3, true), None);
    }

    fn dirty_state() -> EditorState {
        let mut state = EditorState::default();
        state.add_track(engine::TrackKind::Video);
        assert!(state.is_dirty());
        state
    }

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                found.extend(files_under(&entry.path()));
            } else {
                found.push(entry.path());
            }
        }
        found.sort();
        found
    }

    #[test]
    fn autosave_writes_only_into_the_store_and_cleans_up_on_save_and_exit() {
        let dir = folder("autosave");
        let original = dir.join("work").join("p.json");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        let mut state = dirty_state();
        crate::project_file::save_project_file(state.project(), &original).unwrap();
        state.mark_saved(original.clone());
        let saved_bytes = std::fs::read(&original).unwrap();
        state.add_track(engine::TrackKind::Audio);
        let store = RecoveryStore::new(dir.join("recovery"));
        let me = ProcessInfo { pid: 77, started: 7 };
        let start = Instant::now();
        let mut autosave = Autosave::start(Some(store.clone()), me, 60, start);
        assert_eq!(autosave.tick(&state, false, start), Some(start + Duration::from_secs(60)));
        autosave.tick(&state, false, start + Duration::from_secs(61));
        let id = autosave.id_for(Some(&original));
        // The writer runs on its thread: stop it so what it wrote can be checked.
        let writer = autosave.writer.take().unwrap();
        drop(writer.jobs);
        writer.thread.join().unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), saved_bytes, "the user's file is never written");
        assert_eq!(std::fs::read_dir(original.parent().unwrap()).unwrap().count(), 1, "nothing beside it either");
        let meta = store.read_meta(&id).expect("written");
        assert_eq!(meta.original.as_deref(), Some(original.as_path()));
        assert_eq!(meta.owner(), me);
        assert_eq!(meta.revision, state.revision());
        let project: engine::Project = serde_json::from_str(&store.project_text(&id).unwrap()).unwrap();
        assert_eq!(project.tracks.len(), 2);
        assert!(files_under(store.root()).iter().all(|file| file.starts_with(store.root())));

        // A save makes the project clean, which removes the entry.
        autosave.writer = Writer::spawn(store.clone());
        state.mark_saved(original.clone());
        assert_eq!(autosave.tick(&state, false, start + Duration::from_secs(62)), None);
        autosave.finish_clean();
        assert!(!store.entry_dir(&id).exists(), "saved");
        assert!(!store.session_file(77).exists(), "a clean exit ends the session");
    }

    #[test]
    fn a_clean_exit_removes_the_entry_of_a_discarded_project_and_a_crash_keeps_it() {
        let dir = folder("exit");
        let store = RecoveryStore::new(dir.clone());
        let me = ProcessInfo { pid: 78, started: 8 };
        let start = Instant::now();
        let state = dirty_state();
        let later = start + Duration::from_secs(61);

        let mut crashed = Autosave::start(Some(store.clone()), me, 60, start);
        crashed.tick(&state, false, later);
        let id = crashed.id_for(None);
        let writer = crashed.writer.take().unwrap();
        drop(writer.jobs);
        writer.thread.join().unwrap();
        // No finish_clean: the process died.
        let orphans = store.prune_and_list_orphans(&FakeProbe(HashMap::new()), unix_now());
        assert_eq!(orphans.iter().map(|orphan| orphan.id.as_str()).collect::<Vec<_>>(), [id.as_str()]);
        assert_eq!(orphans[0].meta.original, None);

        let next = ProcessInfo { pid: 79, started: 9 };
        let mut closing = Autosave::start(Some(store.clone()), next, 60, start);
        assert_ne!(closing.id_for(None), id);
        closing.tick(&state, false, later);
        let own = closing.id_for(None);
        closing.finish_clean();
        assert!(!store.entry_dir(&own).exists(), "discarded on close");
        assert!(store.entry_dir(&id).exists(), "the offered entry is left alone");
    }

    fn stop_writer(autosave: &mut Autosave) {
        let writer = autosave.writer.take().unwrap();
        drop(writer.jobs);
        writer.thread.join().unwrap();
    }

    /// An orphan of a dead editor for `file` (`None`: untitled), as the store lists it.
    fn orphan(store: &RecoveryStore, file: Option<&Path>) -> String {
        let dead = ProcessInfo { pid: 2, started: 200 };
        let base = file.map_or_else(|| untitled_id(dead, 1, 1), id_for_path);
        let id = owned_id(&base, dead, None);
        store.write_entry(&id, "{}", &meta(dead, NOW, file.map(Path::to_path_buf))).unwrap();
        id
    }

    #[test]
    fn a_restored_entry_is_written_over_and_removed_once_clean() {
        let dir = folder("adopt");
        let store = RecoveryStore::new(dir.clone());
        let file = dir.join("p.json");
        for original in [Some(file.as_path()), None] {
            let id = orphan(&store, original);
            let me = ProcessInfo { pid: 0x2a, started: 1 };
            let start = Instant::now();
            let mut autosave = Autosave::start(Some(store.clone()), me, 60, start);
            let claimed = autosave.claim(&id).unwrap();
            assert!(!store.entry_dir(&id).exists());
            let mut state = dirty_state();
            if let Some(file) = original {
                state.mark_saved(file.to_path_buf());
                state.add_track(engine::TrackKind::Audio);
            }
            autosave.adopt(&claimed, original, &state, start);
            assert_eq!(autosave.id_for(original), claimed);
            stop_writer(&mut autosave);
            assert_eq!(store.read_meta(&claimed).unwrap().owner(), me, "rewritten under this process");
            let listed = store.prune_and_list_orphans(&FakeProbe(HashMap::from([(0x2a, 1)])), NOW);
            assert!(listed.is_empty(), "owned by a live editor: {listed:?}");
            autosave.writer = Writer::spawn(store.clone());
            match original {
                Some(file) => state.mark_saved(file.to_path_buf()),
                None => state.new_project(),
            }
            autosave.tick(&state, false, start);
            autosave.finish_clean();
            assert!(!store.entry_dir(&claimed).exists(), "removed once clean");
            assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "nothing left behind");
        }
    }

    #[test]
    fn autosave_is_off_without_the_process_start_time() {
        let store = RecoveryStore::new(folder("no-start-time"));
        let id = orphan(&store, None);
        let mut autosave = Autosave::start(Some(store.clone()), ProcessInfo { pid: 13, started: 0 }, 60, Instant::now());
        assert_eq!(autosave.claim(&id), Err(ClaimError::Off));
        assert!(store.entry_dir(&id).exists());
    }

    #[test]
    #[cfg(windows)]
    fn an_entry_held_open_is_locked_not_taken() {
        let store = RecoveryStore::new(folder("claim-locked"));
        let id = orphan(&store, None);
        let mut autosave = Autosave::start(Some(store.clone()), ProcessInfo { pid: 12, started: 1 }, 60, Instant::now());
        // Like an antivirus or the indexer reading it: Windows will not rename a folder with an open file.
        let held = std::fs::File::open(store.entry_dir(&id).join(META_FILE)).unwrap();
        assert!(matches!(autosave.claim(&id), Err(ClaimError::Locked(_))), "still there, so not taken");
        assert!(matches!(autosave.discard(&id), Err(ClaimError::Locked(_))));
        assert!(store.entry_dir(&id).exists(), "kept on offer");
        drop(held);
        autosave.discard(&id).unwrap();
        assert!(!store.entry_dir(&id).exists());
    }

    #[test]
    fn only_one_of_two_editors_claims_an_orphan() {
        let store = RecoveryStore::new(folder("claim"));
        let id = orphan(&store, None);
        let mut first = Autosave::start(Some(store.clone()), ProcessInfo { pid: 10, started: 1 }, 60, Instant::now());
        let mut second = Autosave::start(Some(store.clone()), ProcessInfo { pid: 11, started: 1 }, 60, Instant::now());
        let claimed = first.claim(&id).unwrap();
        assert!(matches!(second.claim(&id), Err(ClaimError::Taken(_))), "taken already");
        assert!(second.discard(&id).is_err(), "a discard cannot delete what another editor restored");
        assert!(store.entry_dir(&claimed).exists());
        // A failed restore hands it back.
        first.unclaim(&claimed, &id);
        assert!(store.entry_dir(&id).exists());
        second.discard(&id).unwrap();
        assert!(!store.entry_dir(&id).exists());
    }

    #[test]
    fn an_unfinished_first_write_never_leaves_an_entry_without_meta() {
        let store = RecoveryStore::new(folder("first-write"));
        let dead = ProcessInfo { pid: 2, started: 200 };
        let id = owned_id("u-1", dead, None);
        // Died after the project and before the meta of the first write.
        let tmp = store.root().join(format!("{id}{TMP_SUFFIX}"));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(PROJECT_FILE), "{}").unwrap();
        assert!(!store.entry_dir(&id).exists(), "nothing half-written under the entry's name");
        assert!(store.prune_and_list_orphans(&FakeProbe(HashMap::new()), NOW).is_empty());
        assert!(!tmp.exists(), "the leftover of a dead editor goes");
        // Every completed write is offered, and a later write that died midway still has its meta.
        store.write_entry(&id, "{}", &meta(dead, NOW - 5, None)).unwrap();
        std::fs::write(store.project_file(&id), "{\"newer\":1}").unwrap();
        let orphans = store.prune_and_list_orphans(&FakeProbe(HashMap::new()), NOW);
        assert_eq!(orphans.iter().map(|orphan| orphan.id.as_str()).collect::<Vec<_>>(), [id.as_str()]);
        assert!(!store.root().join(format!("{id}{TMP_SUFFIX}")).exists());
    }

    #[test]
    fn a_process_the_system_will_not_open_counts_as_alive() {
        let store = RecoveryStore::new(folder("denied"));
        let owner = ProcessInfo { pid: 5, started: 50 };
        store.write_session(owner).unwrap();
        let id = owned_id("u-1", owner, None);
        store.write_entry(&id, "{}", &meta(owner, NOW, None)).unwrap();
        let denied = FakeProbe(HashMap::from([(5, u64::MAX)]));
        assert!(owner.is_running(&denied));
        assert!(store.prune_and_list_orphans(&denied, NOW).is_empty());
        assert!(store.session_file(5).exists());
        assert_eq!(store.prune_and_list_orphans(&FakeProbe(HashMap::new()), NOW).len(), 1, "gone is dead");
    }

    #[test]
    fn owned_ids_name_their_owner() {
        let owner = ProcessInfo { pid: 0x2a, started: 0x1234 };
        assert_eq!(owned_id("p-ab", owner, None), "p-ab.2a-1234");
        assert_eq!(owned_id("p-ab.3-4", owner, Some(2)), "p-ab.2a-1234-c2");
        assert_eq!(owner_of("p-ab.2a-1234-c2"), Some(owner));
        assert_eq!(owner_of("p-ab.2a-1234.tmp"), Some(owner));
        assert_eq!(owner_of("p-ab"), None);
    }
}
