//! The MCP server of the editor: tools an agent calls to build a video, on a project file or in an
//! open editor window. See `docs/plans/spec-mcp-server-vi.md`.

#![cfg(windows)]

pub mod session;
pub mod tools;

pub use session::{EditorSession, FileSession, LiveSession};
