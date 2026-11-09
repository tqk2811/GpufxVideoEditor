//! Letting an agent drive the editor. A request runs against an [`crate::editor::EditorState`], the
//! same state and commands the window uses, so undo and the preview follow by themselves. Requests
//! come from the `gpufx-mcp` process, either directly (no window) or through the local pipe of an
//! open window. See `docs/plans/spec-mcp-server-vi.md`.

pub mod gateway;
pub mod image;
pub mod protocol;
pub mod run;

pub use protocol::{AgentEdit, AgentError, AgentRequest, AgentResult, CaptureTarget};
pub use run::{check_export, execute, prepare, AgentHost, PendingFrame, Prepared, Step};
