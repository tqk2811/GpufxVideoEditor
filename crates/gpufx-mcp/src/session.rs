//! Where the tools act: a project held in this process, or (see [`LiveSession`]) an open window.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use d3d12_backend::{D3d12Compositor, EncoderChoice, ExportJob, Gpu, ProjectPaths};
use engine::render::{Compositor, FrameRenderer, RenderError};
use engine::{frame_count, Project};
use gpufx_editor::agent::gateway::pipe_name;
use gpufx_editor::agent::{execute, prepare, AgentError, AgentHost, AgentRequest, AgentResult, Step};
use gpufx_editor::editor::{EditorState, Status};
use gpufx_editor::export::{sibling_render_exe, ExportWorker};
use gpufx_editor::project_file::package_roots;

/// What every tool calls. The tools do not know which session they are on.
pub trait EditorSession {
    fn call(&mut self, request: AgentRequest) -> AgentResult;
}

/// A project held in this process, with no window. Runs the requests on an [`EditorState`] of its
/// own, so a project built here equals one built in the window.
pub struct FileSession {
    host: FileHost,
    roots: Vec<PathBuf>,
}

struct FileHost {
    state: EditorState,
    roots: Vec<PathBuf>,
    /// Opened by the first tool that needs it.
    gpu: Option<Arc<Gpu>>,
    rig: Option<CaptureRig>,
    export: Option<ExportWorker>,
}

/// What captures reuse while the project file and output size stay the same.
struct CaptureRig {
    file: Option<PathBuf>,
    size: (u32, u32),
    compositor: D3d12Compositor,
    renderer: FrameRenderer<D3d12Compositor>,
}

fn gpu_error(message: impl Into<String>) -> AgentError {
    AgentError::new("gpu_unavailable", message)
}

impl FileHost {
    /// Follows the running export, with the same effect on the state as in the window.
    fn poll_export(&mut self) {
        let Some(export) = &mut self.export else { return };
        self.state.set_export_progress(export.progress());
        let Some(outcome) = export.poll() else { return };
        self.export = None;
        self.state.finish_agent_export(&outcome);
        self.state.end_export(Status::Idle);
    }

    fn gpu(&mut self) -> Result<Arc<Gpu>, AgentError> {
        if self.gpu.is_none() {
            self.gpu = Some(Arc::new(Gpu::create().map_err(gpu_error)?));
        }
        let gpu = self.gpu.clone().expect("just opened");
        match gpu.device_lost() {
            Some(message) => Err(gpu_error(format!("the GPU device was lost, restart the session: {message}"))),
            None => Ok(gpu),
        }
    }

    fn render(&mut self, project: &Project, frame: i64) -> Result<Vec<u8>, AgentError> {
        let gpu = self.gpu()?;
        let file = self.state.project_file().map(PathBuf::from);
        let size = (project.output.width, project.output.height);
        if !self.rig.as_ref().is_some_and(|rig| rig.file == file && rig.size == size) {
            self.rig = None;
            let anchor = file.clone().unwrap_or_else(|| PathBuf::from("unsaved.json"));
            let paths = Arc::new(ProjectPaths::roots_only(&anchor, self.roots.clone()));
            let compositor = D3d12Compositor::new(gpu.clone(), paths, size.0, size.1).map_err(gpu_error)?;
            self.rig = Some(CaptureRig { file, size, compositor, renderer: FrameRenderer::default() });
        }
        let rig = self.rig.as_mut().expect("just built");
        let mut rgba = Vec::new();
        let rendered = rig.renderer.render_frame(project, &mut rig.compositor, frame, |compositor, image| -> Result<(), RenderError> { compositor.read(image, &mut rgba) });
        // A failed frame must not be blamed on the next one.
        gpu.take_error();
        rendered.map_err(|error| AgentError::new("capture_failed", error.0))?;
        Ok(rgba)
    }
}

impl AgentHost for FileHost {
    fn mode(&self) -> &'static str {
        "file"
    }

    fn state(&mut self) -> &mut EditorState {
        &mut self.state
    }

    fn capture(&mut self, project: Arc<Project>, frame: i64) -> Receiver<Result<Vec<u8>, AgentError>> {
        let (sender, receiver) = channel();
        let _ = sender.send(self.render(&project, frame));
        receiver
    }

    fn start_export(&mut self, out: PathBuf, encoder: EncoderChoice) -> Result<(), AgentError> {
        let file = self.state.project_file().map(PathBuf::from);
        let anchor = file.clone().unwrap_or_else(|| PathBuf::from("unsaved.json"));
        let paths = Arc::new(ProjectPaths::roots_only(&anchor, self.roots.clone()));
        let project = self.state.project().clone();
        let total = frame_count(&project);
        let job = ExportJob { project, project_file: file, paths, out, encoder, hashes: None };
        job.check().map_err(|message| AgentError::new("invalid_output", message))?;
        // The child needs the GPU memory and decoders the captures hold.
        self.rig = None;
        let (released, gate) = channel();
        let _ = released.send(());
        let worker = ExportWorker::start_headless(job, self.roots.clone(), sibling_render_exe(), gate, || {}).map_err(|message| AgentError::new("export_failed", message))?;
        self.state.begin_export(total);
        self.export = Some(worker);
        Ok(())
    }

    fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
        }
    }
}

impl FileSession {
    pub fn new() -> Self {
        Self::with_roots(package_roots())
    }

    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { host: FileHost { state: EditorState::default(), roots: roots.clone(), gpu: None, rig: None, export: None }, roots }
    }
}

impl Default for FileSession {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorSession for FileSession {
    fn call(&mut self, request: AgentRequest) -> AgentResult {
        self.host.poll_export();
        match execute(&mut self.host, prepare(request, &self.roots)) {
            Step::Done(result) => result,
            Step::Frame(frame) => frame.finish(),
        }
    }
}

/// An open editor window, reached through its local pipe. Connects when a tool needs it and again
/// after the window was closed, so a missing window is an error of the tool and not of the server.
pub struct LiveSession {
    name: String,
    connection: Option<BufReader<File>>,
}

fn no_app(detail: impl std::fmt::Display) -> AgentError {
    AgentError::new("no_app", format!("no editor window is reachable: turn on agent access in its File menu ({detail})"))
}

impl LiveSession {
    /// The pipe of this user's editor.
    pub fn new() -> Result<Self, String> {
        Ok(Self::on_pipe(pipe_name()?))
    }

    pub fn on_pipe(name: String) -> Self {
        Self { name, connection: None }
    }

    fn exchange(&mut self, line: &[u8]) -> std::io::Result<AgentResult> {
        if self.connection.is_none() {
            let file = std::fs::OpenOptions::new().read(true).write(true).open(&self.name)?;
            self.connection = Some(BufReader::new(file));
        }
        let connection = self.connection.as_mut().expect("just connected");
        connection.get_mut().write_all(line)?;
        let mut answer = String::new();
        if connection.read_line(&mut answer)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(serde_json::from_str(&answer).unwrap_or_else(|e| Err(AgentError::new("internal", format!("an unreadable answer from the editor: {e}")))))
    }
}

impl EditorSession for LiveSession {
    fn call(&mut self, request: AgentRequest) -> AgentResult {
        let mut line = serde_json::to_vec(&request).map_err(|e| AgentError::new("internal", e.to_string()))?;
        line.push(b'\n');
        match self.exchange(&line) {
            Ok(result) => result,
            Err(error) => {
                self.connection = None;
                Err(no_app(error))
            }
        }
    }
}
