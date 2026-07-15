//! Pure JSON builders for the daemon file-inbox commands (ARCHITECTURE.md 6.4).
//!
//! The daemon consumes `<workspace>/koma-workflow/inbox/*.json` (and, since the MCP front
//! door, the global `~/.koma-workflow/inbox/`) files whose bodies are `{ "op": ... }`
//! objects parsed by `office-daemon`'s `inbox::parse`. Every producer of those files — the
//! documented hand-written drop, and now the `workflow-mcp` server — should emit the exact
//! same shape. These builders are that single shape: one function per op, each returning a
//! `serde_json::Value` whose field names BYTE-MATCH `office-daemon/src/inbox.rs`.
//!
//! The pinning is enforced from the consumer side: `office-daemon/src/inbox_test.rs` runs
//! every builder here through the real parser and asserts the resulting `Command`. If a
//! field name drifts on either side, that roundtrip test fails — the two crates stay wired
//! together forever.
//!
//! Pure and IO-free: these only build a `Value`. Where the file is written, and how it is
//! named, is the caller's concern (`workflow-mcp`'s `write` module).

use serde_json::{json, Value};

/// `{ "op": "brief", "message": <message> [, "project": <project>] }`.
///
/// Start or continue the office PRD conversation. `project` targets an existing project id;
/// omit it (`None`) to let the office mint a fresh project from the message. The `project`
/// key is emitted only when `Some`, matching how the parser reads it (`opt_str_field`).
pub fn brief(project: Option<&str>, message: &str) -> Value {
    let mut v = json!({ "op": "brief", "message": message });
    if let Some(p) = project {
        v["project"] = json!(p);
    }
    v
}

/// `{ "op": "status" [, "project": <project>] }`.
///
/// Request a status readout. `project` scopes to one project; `None` means "all". The
/// parser accepts this op (`Command::Status`), so it is included for completeness and
/// roundtrip pinning even though the MCP `workflow_status` tool reads the store directly
/// instead of writing this file.
pub fn status(project: Option<&str>) -> Value {
    let mut v = json!({ "op": "status" });
    if let Some(p) = project {
        v["project"] = json!(p);
    }
    v
}

/// `{ "op": "authorize", "project": <project>, "delivery_path": <delivery_path> }`.
///
/// Approve a project's PRD and start the production line, delivering finished work to
/// `delivery_path`. Both fields are required by the parser.
pub fn authorize(project: &str, delivery_path: &str) -> Value {
    json!({ "op": "authorize", "project": project, "delivery_path": delivery_path })
}

/// `{ "op": "comment", "task": <task>, "text": <text> }`.
///
/// Post a comment on a task card. Both fields are required by the parser.
pub fn comment(task: &str, text: &str) -> Value {
    json!({ "op": "comment", "task": task, "text": text })
}

/// `{ "op": "interrupt", "project": <project>, "mode": "hard"|"soft" }`.
///
/// Interrupt a running project. The parser derives `hard` from the `mode` field
/// (`hard == (mode != "soft")`), so `hard: true` emits `"mode": "hard"` and `hard: false`
/// emits `"mode": "soft"` — an explicit, unambiguous roundtrip either way.
pub fn interrupt(project: &str, hard: bool) -> Value {
    let mode = if hard { "hard" } else { "soft" };
    json!({ "op": "interrupt", "project": project, "mode": mode })
}

/// `{ "op": "resume", "project": <project> }`.
///
/// Resume a previously interrupted project. `project` is required by the parser.
pub fn resume(project: &str) -> Value {
    json!({ "op": "resume", "project": project })
}

/// `{ "op": "breakdown", "project": <project> }`.
///
/// Re-run the office breakdown for a drafted PRD (e.g. after a model timeout). `project`
/// is required by the parser; the result lands as chat notices, same as every other
/// command tool.
pub fn breakdown(project: &str) -> Value {
    json!({ "op": "breakdown", "project": project })
}

/// `{ "op": "archive_project", "project": <project> }`.
///
/// Permanently delete (archive) a project and stop its agents. `project` is required by the
/// parser. Like every other command tool the office acknowledges in chat, not in the tool
/// result. The two-step confirm (the caller must echo the slug) lives in the `workflow-mcp`
/// tool, not here — this builder only emits the envelope once the tool has decided to fire.
pub fn archive_project(project: &str) -> Value {
    json!({ "op": "archive_project", "project": project })
}
