//! Where the tools act: a project held in this process, or (see [`LiveSession`]) an open window.

use std::path::PathBuf;

use gpufx_editor::agent::{execute, prepare, AgentHost, AgentRequest, AgentResult, Step};
use gpufx_editor::editor::EditorState;
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
}

impl AgentHost for FileHost {
    fn mode(&self) -> &'static str {
        "file"
    }

    fn state(&mut self) -> &mut EditorState {
        &mut self.state
    }
}

impl FileSession {
    pub fn new() -> Self {
        Self::with_roots(package_roots())
    }

    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { host: FileHost { state: EditorState::default() }, roots }
    }
}

impl Default for FileSession {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorSession for FileSession {
    fn call(&mut self, request: AgentRequest) -> AgentResult {
        match execute(&mut self.host, prepare(request, &self.roots)) {
            Step::Done(result) => result,
        }
    }
}
