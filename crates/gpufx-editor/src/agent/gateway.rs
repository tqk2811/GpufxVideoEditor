//! The local pipe through which `gpufx-mcp` reaches an open window. One client at a time, one JSON
//! line per request and per answer. The pipe only admits the current user and never leaves the
//! machine. Requests are handed to the UI thread, which alone touches the editor state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::protocol::{AgentError, AgentRequest, AgentResult};
use super::run::{prepare, Prepared, Step};

const POLL: Duration = Duration::from_millis(5);
const BUFFER: u32 = 64 * 1024;
/// Longer request lines end the connection: no request is anywhere near this.
const MAX_REQUEST: usize = 1024 * 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The SID of the user running this process, as text.
fn user_sid() -> Result<String, String> {
    let last_error = |what: &str| format!("{what} failed: {}", std::io::Error::last_os_error());
    // SAFETY: every pointer is valid for its call, and what the system allocates is freed here.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        let mut size = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let read = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), size, &mut size);
        CloseHandle(token);
        if read == 0 {
            return Err(last_error("GetTokenInformation"));
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut text: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(last_error("ConvertSidToStringSidW"));
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// The pipe of this user's editor.
pub fn pipe_name() -> Result<String, String> {
    Ok(format!(r"\\.\pipe\gpufx-editor-agent-{}", user_sid()?))
}

/// Creates the only instance of the pipe, usable by the current user alone.
fn create_pipe(name: &str) -> Result<HANDLE, String> {
    let descriptor_text = wide(&format!("D:P(A;;GA;;;{})", user_sid()?));
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the text is NUL-terminated and the out pointer is valid.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(descriptor_text.as_ptr() as PCWSTR, SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut()) } == 0 {
        return Err(format!("building the pipe security failed: {}", std::io::Error::last_os_error()));
    }
    let attributes = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
    let name = wide(name);
    // SAFETY: the name and the attributes outlive the call; the descriptor is freed after it.
    let pipe = unsafe {
        let pipe = CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS, 1, BUFFER, BUFFER, 0, &attributes);
        LocalFree(descriptor);
        pipe
    };
    if pipe == INVALID_HANDLE_VALUE {
        return Err(format!("creating the agent pipe failed (is another editor open?): {}", std::io::Error::last_os_error()));
    }
    Ok(pipe)
}

/// A request waiting for the UI thread.
pub struct Inbound {
    pub prepared: Prepared,
    reply: Sender<Step>,
}

impl Inbound {
    /// The request and where its answer goes.
    pub fn split(self) -> (Prepared, Replier) {
        (self.prepared, Replier(self.reply))
    }
}

/// Takes the answer of one request.
pub struct Replier(Sender<Step>);

impl Replier {
    pub fn respond(self, step: Step) {
        let _ = self.0.send(step);
    }
}

/// The running pipe. Dropping it closes the pipe.
pub struct AgentGateway {
    inbound: Receiver<Inbound>,
    stop: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

struct Pipe {
    handle: HANDLE,
    stop: Arc<AtomicBool>,
}

// A pipe handle may be used from any thread.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed once.
        unsafe { CloseHandle(self.handle) };
    }
}

impl Pipe {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Waits for a client; false when stopping.
    fn accept(&self) -> bool {
        while !self.stopping() {
            // SAFETY: the handle is alive; a non-blocking pipe needs no OVERLAPPED.
            let (connected, error) = unsafe { (ConnectNamedPipe(self.handle, std::ptr::null_mut()), GetLastError()) };
            if connected != 0 || error == ERROR_PIPE_CONNECTED {
                return true;
            }
            if error == ERROR_NO_DATA {
                // A client came and went.
                unsafe { DisconnectNamedPipe(self.handle) };
            }
            thread::sleep(POLL);
        }
        false
    }

    /// The next line from the client, or `None` when it left or the gateway stops.
    fn read_line(&self, pending: &mut Vec<u8>) -> Option<Vec<u8>> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
                let mut line: Vec<u8> = pending.drain(..=end).collect();
                line.pop();
                return Some(line);
            }
            if self.stopping() || pending.len() > MAX_REQUEST {
                return None;
            }
            let mut read = 0u32;
            // SAFETY: the buffer and counter are valid for the call.
            let ok = unsafe { ReadFile(self.handle, chunk.as_mut_ptr(), chunk.len() as u32, &mut read, std::ptr::null_mut()) };
            if ok != 0 && read > 0 {
                pending.extend_from_slice(&chunk[..read as usize]);
            } else if ok != 0 || unsafe { GetLastError() } == ERROR_NO_DATA {
                thread::sleep(POLL);
            } else {
                return None;
            }
        }
    }

    /// False when the client left. The pipe does not block, so a full buffer takes several rounds.
    fn write_all(&self, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            if self.stopping() {
                return false;
            }
            let mut written = 0u32;
            // SAFETY: the slice and counter are valid for the call.
            if unsafe { WriteFile(self.handle, bytes.as_ptr(), bytes.len().min(BUFFER as usize) as u32, &mut written, std::ptr::null_mut()) } == 0 {
                return false;
            }
            bytes = &bytes[written as usize..];
            if written == 0 {
                thread::sleep(POLL);
            }
        }
        true
    }
}

/// Carries one request to the UI thread and back. A capture is finished here, off the UI thread.
fn answer(line: &[u8], roots: &[PathBuf], inbound: &Sender<Inbound>, wake: &(impl Fn() + Send), stop: &AtomicBool) -> AgentResult {
    let request: AgentRequest = serde_json::from_slice(line).map_err(|e| AgentError::new("invalid_request", e.to_string()))?;
    let prepared = prepare(request, roots);
    let (reply, step) = channel();
    let closed = || AgentError::new("no_app", "the editor is closing");
    inbound.send(Inbound { prepared, reply }).map_err(|_| closed())?;
    wake();
    loop {
        match step.recv_timeout(Duration::from_millis(100)) {
            Ok(Step::Done(result)) => return result,
            Ok(Step::Frame(frame)) => return frame.finish(),
            Err(RecvTimeoutError::Disconnected) => return Err(closed()),
            Err(RecvTimeoutError::Timeout) if stop.load(Ordering::SeqCst) => return Err(closed()),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn serve(pipe: Pipe, roots: Vec<PathBuf>, inbound: Sender<Inbound>, connected: Arc<AtomicBool>, wake: impl Fn() + Send) {
    while pipe.accept() {
        connected.store(true, Ordering::SeqCst);
        wake();
        let mut pending = Vec::new();
        while let Some(line) = pipe.read_line(&mut pending) {
            let result = answer(&line, &roots, &inbound, &wake, &pipe.stop);
            let mut text = serde_json::to_vec(&result).unwrap_or_else(|_| br#"{"Err":{"code":"internal","message":"the answer could not be written"}}"#.to_vec());
            text.push(b'\n');
            if !pipe.write_all(&text) {
                break;
            }
        }
        // SAFETY: the handle is alive.
        unsafe { DisconnectNamedPipe(pipe.handle) };
        connected.store(false, Ordering::SeqCst);
        wake();
    }
}

impl AgentGateway {
    /// Opens the pipe `name`. `wake` tells the UI thread to look at [`Self::next_request`].
    pub fn start(name: &str, package_roots: Vec<PathBuf>, wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let stop: Arc<AtomicBool> = Arc::default();
        let connected: Arc<AtomicBool> = Arc::default();
        let (sender, inbound) = channel();
        let pipe = Pipe { handle: create_pipe(name)?, stop: stop.clone() };
        let thread_connected = connected.clone();
        let handle = thread::Builder::new().name("agent-pipe".to_owned()).spawn(move || serve(pipe, package_roots, sender, thread_connected, wake)).map_err(|e| e.to_string())?;
        Ok(Self { inbound, stop, connected, handle: Some(handle) })
    }

    /// The next request for the UI thread, if one waits.
    pub fn next_request(&self) -> Option<Inbound> {
        self.inbound.try_recv().ok()
    }

    /// Whether an agent is connected now.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl Drop for AgentGateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            // Runs on the UI thread: a capture being finished may take a moment, so do not wait for it.
            let started = std::time::Instant::now();
            while !handle.is_finished() && started.elapsed() < Duration::from_secs(1) {
                thread::sleep(POLL);
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}
