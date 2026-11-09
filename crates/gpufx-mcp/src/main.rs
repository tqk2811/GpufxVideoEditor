//! `gpufx-mcp`: the MCP server of the editor, over stdio.
//!
//! Without options it works on a project file with no window. Tools run one at a time on a thread
//! of their own, because a session may hold GPU objects that stay on the thread that made them.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::Arc;

use gpufx_editor::agent::{AgentError, AgentRequest, AgentResult};
use gpufx_mcp::session::{EditorSession, FileSession};
use gpufx_mcp::tools::{all_tools, ToolDef};
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::Value;
use tokio::sync::oneshot;

type Job = (AgentRequest, oneshot::Sender<AgentResult>);

struct Server {
    tools: Arc<Vec<ToolDef>>,
    jobs: mpsc::Sender<Job>,
}

fn run_session(mut session: Box<dyn EditorSession>, jobs: mpsc::Receiver<Job>) {
    for (request, reply) in jobs {
        let result = catch_unwind(AssertUnwindSafe(|| session.call(request))).unwrap_or_else(|_| Err(AgentError::new("internal", "the session hit an internal error")));
        let _ = reply.send(result);
    }
}

/// The answer of a tool: the JSON, with a captured image as an image block next to it.
fn tool_output(result: AgentResult) -> CallToolResult {
    match result {
        Ok(mut value) => {
            let png = value.as_object_mut().and_then(|object| object.remove("png"));
            let mut content = vec![ContentBlock::text(value.to_string())];
            if let Some(Value::String(data)) = png {
                content.push(ContentBlock::image(data, "image/png"));
            }
            CallToolResult::success(content)
        }
        Err(error) => CallToolResult::error(vec![ContentBlock::text(serde_json::to_string(&error).unwrap_or_else(|_| error.message.clone()))]),
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions("Build a video with the gpufx editor. Every edit takes expected_revision from the last result. Clips are named by {track, clip} indexes, which change after edits; times are ticks of 1/90000 s.")
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult { tools: self.tools.iter().map(ToolDef::tool).collect(), ..Default::default() })
    }

    async fn call_tool(&self, request: CallToolRequestParams, _context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let Some(tool) = self.tools.iter().find(|tool| tool.name == request.name) else {
            return Err(ErrorData::invalid_params(format!("no tool {}", request.name), None));
        };
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let result = match tool.request(arguments) {
            Ok(call) => {
                let (reply, answer) = oneshot::channel();
                if self.jobs.send((call, reply)).is_err() {
                    Err(AgentError::new("internal", "the session has stopped"))
                } else {
                    answer.await.unwrap_or_else(|_| Err(AgentError::new("internal", "the session has stopped")))
                }
            }
            Err(error) => Err(error),
        };
        Ok(tool_output(result).into())
    }
}

struct Options {
    project: Option<PathBuf>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options { project: None };
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_string_lossy().as_ref() {
            "--project" => options.project = Some(PathBuf::from(arguments.next().ok_or("--project needs a file")?)),
            other => return Err(format!("unknown argument {other}\nusage: gpufx-mcp [--project <file>]")),
        }
    }
    Ok(options)
}

#[tokio::main]
async fn run(options: Options) -> Result<(), String> {
    let (jobs, incoming) = mpsc::channel();
    let (ready_sender, ready) = oneshot::channel();
    std::thread::Builder::new().name("session".to_owned()).spawn(move || {
        // Made here: a session holds things that cannot move to another thread.
        let mut session = FileSession::new();
        let opened = options.project.map_or(Ok(()), |path| session.call(AgentRequest::ProjectOpen { path, discard_changes: true }).map(|_| ()).map_err(|error| error.message));
        let failed = opened.is_err();
        let _ = ready_sender.send(opened);
        if !failed {
            run_session(Box::new(session), incoming);
        }
    }).map_err(|e| e.to_string())?;
    ready.await.map_err(|_| "the session thread stopped".to_owned())??;
    let server = Server { tools: Arc::new(all_tools()), jobs };
    let service = server.serve(rmcp::transport::stdio()).await.map_err(|e| e.to_string())?;
    service.waiting().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn main() -> ExitCode {
    let result = parse_options().and_then(run);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gpufx-mcp: {message}");
            ExitCode::FAILURE
        }
    }
}
