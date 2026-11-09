//! Helpers shared by the tests of the tools.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use gpufx_editor::agent::AgentError;
use gpufx_mcp::session::EditorSession;
use gpufx_mcp::tools::all_tools;
use serde_json::Value;

#[path = "../../../gpufx-render/tests/common/mod.rs"]
mod render;

#[allow(unused_imports)]
pub use render::FRAME;

pub fn packages_root() -> PathBuf {
    std::fs::canonicalize(render::packages_root()).expect("the submodule")
}

/// Two H.264 clips with their frame number burnt in.
pub fn clip_paths() -> (String, String) {
    let (a, b) = render::test_clips();
    (a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned())
}

/// The frame number burnt into a clip's RGBA picture.
pub fn read_frame_number(rgba: &[u8], width: u32) -> i64 {
    render::read_frame_number(rgba, width)
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// One GPU test at a time in this process.
pub fn gpu_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn call(session: &mut dyn EditorSession, tool: &str, arguments: Value) -> Result<Value, AgentError> {
    let tools = all_tools();
    let definition = tools.iter().find(|definition| definition.name == tool).unwrap_or_else(|| panic!("no tool {tool}"));
    session.call(definition.request(arguments)?)
}

pub fn call_ok(session: &mut dyn EditorSession, tool: &str, arguments: Value) -> Value {
    call(session, tool, arguments.clone()).unwrap_or_else(|error| panic!("{tool} {arguments}: {error:?}"))
}

pub fn call_err(session: &mut dyn EditorSession, tool: &str, arguments: Value) -> AgentError {
    call(session, tool, arguments.clone()).expect_err(&format!("{tool} {arguments} must fail"))
}

pub fn revision(value: &Value) -> u64 {
    value["revision"].as_u64().expect("every result has a revision")
}
