//! What the editor needs to run `gpufx-render` as a child, for the export and for the effect
//! preview helper alike: a job object that takes the child down with the editor, bounded reading
//! of its pipes, and waiting for a thread of those readers.

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use d3d12_backend::MAX_EVENT_LINE;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE};

/// How often a waiting loop looks again.
pub(crate) const POLL: Duration = Duration::from_millis(20);
/// Longer lines of stdout are not events. Their bytes are dropped, not kept.
pub(crate) const MAX_LINE: usize = MAX_EVENT_LINE;
/// How much of stderr is kept for the error message.
pub(crate) const STDERR_TAIL: usize = 1024;

/// A job object that kills what is in it when its last handle closes, which the system also does
/// for a process that crashes.
pub(crate) struct KillOnClose(HANDLE);

// A job handle may be used and closed from any thread.
unsafe impl Send for KillOnClose {}

impl KillOnClose {
    pub(crate) fn new() -> Result<Self, String> {
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

    /// Puts `child` in the job. `what` names it in the error, like "the export process".
    pub(crate) fn assign(&self, child: &Child, what: &str) -> Result<(), String> {
        // SAFETY: both handles are alive for the call.
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) } == 0 {
            return Err(format!("adding {what} to its job failed: {}", std::io::Error::last_os_error()));
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
pub(crate) fn pump_lines(reader: impl Read, mut on_line: impl FnMut(&[u8])) {
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
pub(crate) fn tail_of(mut reader: impl Read) -> String {
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

/// The thread's result if it ends within `limit`; otherwise it is left running on its own.
pub(crate) fn join_within<T>(handle: thread::JoinHandle<T>, limit: Duration) -> Option<T> {
    let started = Instant::now();
    while !handle.is_finished() {
        if started.elapsed() >= limit {
            return None;
        }
        thread::sleep(POLL);
    }
    handle.join().ok()
}
