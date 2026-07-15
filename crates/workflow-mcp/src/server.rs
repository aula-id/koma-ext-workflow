//! The `workflow` MCP server: a hand-rolled [`ServerHandler`] exposing eight tools.
//!
//! Seven are COMMAND tools (brief/authorize/comment/interrupt/resume/breakdown/approve): each
//! builds the exact inbox JSON via `office_core::inboxmsg` and drops it into the resolved inbox
//! directory — the office picks it up and answers as a CHAT NOTICE, so the tool result only
//! confirms the drop. One is a READ tool (status): it reads the store directly and returns
//! the digest inline. Tools and JSON schemas are written out by hand (no `#[tool]` macros)
//! so the wire shape the koma MCP client advertises to the model is fully explicit.

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{json, Map, Value};

use office_core::inboxmsg;

use crate::status;
use crate::write::{inbox_dir_for, write_inbox_file};

/// Server-level instructions surfaced to the MCP client at initialize.
const INSTRUCTIONS: &str = "Workflow office control tools. The command tools \
(workflow_brief / workflow_authorize / workflow_comment / workflow_interrupt / \
workflow_resume / workflow_breakdown / workflow_approve) drop a request into the office inbox \
and return immediately; the office's acknowledgement and any reply arrive as CHAT NOTICES, not \
in the tool result. workflow_status is read-only and returns the board digest inline.";

/// The `workflow` MCP server. Stateless: every call resolves its inbox dir / reads the store
/// fresh, so a single instance is trivially `Send + Sync`.
#[derive(Clone, Default)]
pub struct WorkflowServer;

impl WorkflowServer {
    pub fn new() -> Self {
        Self
    }
}

impl ServerHandler for WorkflowServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("workflow", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tool_defs()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = args_value(&request);
        let workspace = nonempty(&args, "workspace");
        let workspace = workspace.as_deref();

        let result = match request.name.as_ref() {
            "workflow_brief" => {
                let Some(message) = nonempty(&args, "message") else {
                    return Ok(error_result("workflow_brief requires a non-empty 'message'"));
                };
                let project = nonempty(&args, "project");
                write_command(inboxmsg::brief(project.as_deref(), &message), workspace)
            }
            "workflow_status" => {
                let project = nonempty(&args, "project");
                text_result(status::status_digest(project.as_deref()))
            }
            "workflow_authorize" => {
                let Some(project) = nonempty(&args, "project") else {
                    return Ok(error_result("workflow_authorize requires a non-empty 'project'"));
                };
                let Some(delivery_path) = nonempty(&args, "delivery_path") else {
                    return Ok(error_result(
                        "workflow_authorize requires a non-empty 'delivery_path'",
                    ));
                };
                write_command(inboxmsg::authorize(&project, &delivery_path), workspace)
            }
            "workflow_comment" => {
                let Some(task) = nonempty(&args, "task") else {
                    return Ok(error_result("workflow_comment requires a non-empty 'task'"));
                };
                let Some(text) = nonempty(&args, "text") else {
                    return Ok(error_result("workflow_comment requires a non-empty 'text'"));
                };
                write_command(inboxmsg::comment(&task, &text), workspace)
            }
            "workflow_interrupt" => {
                let Some(project) = nonempty(&args, "project") else {
                    return Ok(error_result("workflow_interrupt requires a non-empty 'project'"));
                };
                // `hard` defaults true (a bare interrupt is a hard stop), matching the
                // daemon's `mode`-absent default.
                let hard = bool_arg(&args, "hard").unwrap_or(true);
                write_command(inboxmsg::interrupt(&project, hard), workspace)
            }
            "workflow_resume" => {
                let Some(project) = nonempty(&args, "project") else {
                    return Ok(error_result("workflow_resume requires a non-empty 'project'"));
                };
                write_command(inboxmsg::resume(&project), workspace)
            }
            "workflow_breakdown" => {
                let Some(project) = nonempty(&args, "project") else {
                    return Ok(error_result("workflow_breakdown requires a non-empty 'project'"));
                };
                write_command(inboxmsg::breakdown(&project), workspace)
            }
            "workflow_approve" => {
                let Some(project) = nonempty(&args, "project") else {
                    return Ok(error_result("workflow_approve requires a non-empty 'project'"));
                };
                write_command(inboxmsg::approve(&project), workspace)
            }
            other => {
                // An unknown tool name is unroutable -> a JSON-RPC protocol error.
                return Err(McpError::invalid_params(format!("unknown tool '{other}'"), None));
            }
        };
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Command write path
// ---------------------------------------------------------------------------

/// Resolve the inbox dir, write the builder `body`, and report the path used. The office's
/// reply comes back in chat, so the tool result only confirms the drop.
fn write_command(body: Value, workspace_arg: Option<&str>) -> CallToolResult {
    let dir = inbox_dir_for(workspace_arg);
    match write_inbox_file(&dir, &body) {
        Ok(path) => text_result(format!(
            "workflow: request written to {}. The office will process it and reply as a chat \
             notice (not in this tool result).",
            path.display()
        )),
        Err(e) => error_result(&format!(
            "workflow: could not write to the inbox at {}: {e}",
            dir.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool definitions (name + description + JSON schema)
// ---------------------------------------------------------------------------

fn tool_defs() -> Vec<Tool> {
    vec![
        Tool::new(
            "workflow_brief",
            "Start or continue the PRD conversation with the Workflow office. Put your \
             natural-language brief or reply in `message`. Pass `project` to continue an \
             existing project by id; a new or unknown id mints a fresh project. This drops a \
             request into the office inbox and returns immediately - the office's reply \
             arrives as a chat notice, not in this tool result.",
            object_schema(
                json!({
                    "message": { "type": "string", "description": "Natural-language brief or reply to the office." },
                    "project": { "type": "string", "description": "Existing project id to continue; a new/unknown id mints a fresh project." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["message"],
            ),
        ),
        Tool::new(
            "workflow_status",
            "Read a plain-text status digest of Workflow projects directly from the store \
             (read-only; writes nothing). With no `project`, lists every project with its \
             phase, task counts by column, parked tasks and reasons, total bounces, and \
             pending office notices. Pass `project` for a single-project detail view with a \
             task listing. The result is returned inline.",
            object_schema(
                json!({
                    "project": { "type": "string", "description": "Optional project id for a single-project detail view; omit for all projects." }
                }),
                &[],
            ),
        ),
        Tool::new(
            "workflow_authorize",
            "Approve a project's PRD and start the production line. `project` is the project \
             id; `delivery_path` is where finished work is delivered. Drops an authorize \
             request into the office inbox; the office confirms via a chat notice, not in \
             this tool result.",
            object_schema(
                json!({
                    "project": { "type": "string", "description": "The project id to authorize." },
                    "delivery_path": { "type": "string", "description": "Absolute path where finished work is delivered." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["project", "delivery_path"],
            ),
        ),
        Tool::new(
            "workflow_comment",
            "Post a comment on a task card; the task's agent consumes it. `task` is the full \
             task id, `text` is the comment body. Dropped into the office inbox; the office \
             acknowledges in chat, not in this tool result.",
            object_schema(
                json!({
                    "task": { "type": "string", "description": "The full task id to comment on." },
                    "text": { "type": "string", "description": "The comment body." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["task", "text"],
            ),
        ),
        Tool::new(
            "workflow_interrupt",
            "Interrupt a running project. `hard` (default true) stops immediately; \
             `hard: false` requests a soft drain that lets in-flight work finish. Dropped \
             into the office inbox; the office acknowledges in chat, not in this tool result.",
            object_schema(
                json!({
                    "project": { "type": "string", "description": "The project id to interrupt." },
                    "hard": { "type": "boolean", "description": "true (default) = hard stop; false = soft drain." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["project"],
            ),
        ),
        Tool::new(
            "workflow_resume",
            "Resume a previously interrupted project. Dropped into the office inbox; the \
             office acknowledges in chat, not in this tool result.",
            object_schema(
                json!({
                    "project": { "type": "string", "description": "The project id to resume." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["project"],
            ),
        ),
        Tool::new(
            "workflow_breakdown",
            "Re-run the office breakdown for a drafted PRD (e.g. after a model timeout); \
             result arrives as chat notices. Dropped into the office inbox; the office \
             acknowledges in chat, not in this tool result.",
            object_schema(
                json!({
                    "project": { "type": "string", "description": "The project id to re-run the breakdown for." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["project"],
            ),
        ),
        Tool::new(
            "workflow_approve",
            "Approve the safeguard's pending assumptions for a drafting project so the office \
             stops waiting and resumes drafting. Use this when the office flagged unapproved \
             assumptions and you are fine with its proposed choices (equivalent to saying 'you \
             decide, proceed'). Dropped into the office inbox; the office acknowledges in chat, \
             not in this tool result.",
            object_schema(
                json!({
                    "project": { "type": "string", "description": "The project id whose pending assumptions to approve." },
                    "workspace": { "type": "string", "description": "Optional workspace dir override for where the request file is written." }
                }),
                &["project"],
            ),
        ),
    ]
}

/// Build a JSON-Schema `object` with the given `properties` and `required` keys.
fn object_schema(properties: Value, required: &[&str]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".to_string(), json!("object"));
    m.insert("properties".to_string(), properties);
    if !required.is_empty() {
        m.insert("required".to_string(), json!(required));
    }
    m
}

// ---------------------------------------------------------------------------
// Small arg + result helpers
// ---------------------------------------------------------------------------

fn args_value(request: &CallToolRequestParams) -> Value {
    match &request.arguments {
        Some(map) => Value::Object(map.clone()),
        None => Value::Object(Map::new()),
    }
}

/// A required/optional string arg, present and non-empty.
fn nonempty(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn text_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

/// A tool-level error result (caller-visible content), for user-fixable validation failures
/// like a missing required argument (per rmcp's `CallToolResult::error` guidance).
fn error_result(text: &str) -> CallToolResult {
    CallToolResult::error(vec![Content::text(text.to_string())])
}
