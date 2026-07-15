//! koma->ext routing (BUILD_WAVES.md W6): `on_invoke`/`on_event` never touch a `Koma`
//! handle (the SDK's DEADLOCK RULE, sdk.rs:22-33) — they only parse the wire params
//! into a daemon-level [`Input`] and push it onto the `mpsc` channel the driver
//! thread (W7) owns, then reply immediately.
//!
//! [`Command`]/[`HostEvent`] here are daemon-level types, distinct from
//! `office_core::kernel::Command`/`HostEvent`: the kernel's `Command` only covers the
//! project-scoped control-loop intents wired in W4 (`Interrupt`/`Resume`/`Unpark`/
//! `AddComment`); PRD/breakdown/authorize intents land on it in W9. Every contributed
//! tool (manifest.json) and every panel op (ARCHITECTURE.md 10.2) needs a home NOW, so
//! this module's `Command` is the superset the driver will route piece by piece as
//! later waves wire the kernel, office persona, and store. Likewise `HostEvent` here
//! covers all four inbound event/notify names (`subagent.done`, `agent.turn_end`,
//! `session.foreground_change`, the private `agents.done`); only the last maps 1:1
//! onto `office_core::kernel::HostEvent::AgentsDone` today.

use serde_json::Value;
use std::sync::mpsc::Sender;

// ---------------------------------------------------------------------------
// Daemon-level protocol
// ---------------------------------------------------------------------------

/// Intents parsed from a contributed `tool.call` or a panel `panel.msg`. Carries
/// enough to route once the driver/kernel/office exist (W7-W9); this wave only
/// parses and enqueues.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    // ---- contributed tools (manifest.json / ARCHITECTURE.md 11) ----
    /// `workflow_brief`. `project: None` means "the single Drafting project".
    Brief { project: Option<String>, message: String },
    /// `workflow_status`. `project: None` means "all projects".
    Status { project: Option<String> },
    /// `workflow_authorize`.
    Authorize { project: String, delivery_path: String },
    /// `workflow_interrupt`. `hard` defaults `true` (mode absent/anything but "soft").
    Interrupt { project: String, hard: bool },
    /// `workflow_resume`.
    Resume { project: String },
    /// `workflow_comment`.
    Comment { task: String, text: String },
    /// `workflow_projects`.
    Projects,
    /// `{ op: "breakdown", project }` — ask the office to author + land the epic/story/
    /// task breakdown for the project's PRD (6.3.2). No contributed tool; panel-only.
    Breakdown { project: String },
    /// An off-loop `models.invoke` completed (W9): posted by the invoke worker pool onto
    /// the driver channel, NOT parsed from the wire. `req_id` matches the driver's pending
    /// job; `result` is the model output or the error string. The driver applies its one
    /// retry, then routes the outcome into the kernel as `kernel::Command::InvokeResult`.
    InvokeDone {
        req_id: u64,
        result: Result<String, String>,
    },

    // ---- panel ops (ARCHITECTURE.md 10.2) not already covered above ----
    /// `{ op: "hello", uiVersion }` — rehydrate.
    PanelHello { ui_version: Option<String> },
    /// `{ op: "state", project? }`.
    PanelState { project: Option<String> },
    /// `{ op: "office_chat", project, message }` — async, answer arrives via push.
    OfficeChat { project: String, message: String },
    /// `{ op: "card_move", task, to, killWorker? }`.
    CardMove {
        task: String,
        to: String,
        kill_worker: bool,
    },
    /// `{ op: "unpark", task }`.
    Unpark { task: String },
    /// `{ op: "edit_task", task, ... }` — the rest of the payload is an opaque patch;
    /// the shape is finalized when the panel wave that emits it lands.
    EditTask { task: String, patch: Value },
    /// `{ op: "edit_deps", task, ... }`.
    EditDeps { task: String, patch: Value },
    /// `{ op: "config_set", project, maxWorkers?, bounceBudget?, workerModel?,
    /// reviewerModel?, keepDesks?, crdPassGrade?, assumptionCheck? }`.
    ConfigSet {
        project: String,
        max_workers: Option<u32>,
        bounce_budget: Option<u32>,
        worker_model: Option<String>,
        reviewer_model: Option<String>,
        keep_desks: Option<bool>,
        crd_pass_grade: Option<u32>,
        assumption_check: Option<bool>,
    },
    /// `{ op: "project_create", name }`.
    ProjectCreate { name: String },
    /// `{ op: "project_archive", project }`.
    ProjectArchive { project: String },
    /// `{ op: "prd_get", project }`.
    PrdGet { project: String },
    /// `{ op: "task_detail", task }`.
    TaskDetail { task: String },
}

/// koma->ext events/notifies this daemon reacts to. `SubagentDone`/`AgentTurnEnd`/
/// `SessionForegroundChange` come from `contributes.events` (broadcast, best-effort,
/// no correlation to our own bindings by design — ARCHITECTURE.md 2.2). `AgentsDone`
/// comes from the private per-spawn `notify: true` callback and DOES carry our own
/// ext-facing `agentId`; it is the one variant with a direct `office_core::kernel::
/// HostEvent::AgentsDone` counterpart.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    SubagentDone {
        session: String,
        subagent_id: u64,
        agent: String,
        status: String,
    },
    AgentTurnEnd {
        session: String,
    },
    SessionForegroundChange {
        session: String,
    },
    AgentsDone {
        agent_id: u64,
        status: String,
    },
}

/// Everything routed onto the driver's `mpsc` channel.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Command(Command),
    Event(HostEvent),
}

// ---------------------------------------------------------------------------
// on_invoke / on_event
// ---------------------------------------------------------------------------

/// Handle a koma->ext `Invoke`. `tx` is the sender half of the driver's `mpsc`
/// channel — the ONLY thing this function touches besides `params`; it never holds
/// or calls a `Koma` handle (DEADLOCK RULE). Malformed params never panic: they
/// produce an `{"error": "..."}` output instead.
pub fn on_invoke(method: &str, params: Value, tx: &Sender<Input>) -> Value {
    match method {
        "tool.call" => handle_tool_call(params, tx),
        "panel.msg" => handle_panel_msg(params, tx),
        other => error(&format!("unknown method: {other}")),
    }
}

/// Handle a koma->ext `Event` (fire-and-forget, no reply). Unrecognized event names
/// are dropped silently (koma only ever delivers names this daemon subscribed to via
/// `contributes.events`, plus the private `agents.done` notify — anything else would
/// be a host bug, not something to panic over).
pub fn on_event(name: &str, params: Value, tx: &Sender<Input>) {
    let event = match name {
        "subagent.done" => {
            let session = str_field(&params, "session").unwrap_or_default();
            let subagent_id = params
                .get("subagentId")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let agent = str_field(&params, "agent").unwrap_or_default();
            let status = str_field(&params, "status").unwrap_or_default();
            HostEvent::SubagentDone {
                session,
                subagent_id,
                agent,
                status,
            }
        }
        "agent.turn_end" => HostEvent::AgentTurnEnd {
            session: str_field(&params, "session").unwrap_or_default(),
        },
        "session.foreground_change" => HostEvent::SessionForegroundChange {
            session: str_field(&params, "session").unwrap_or_default(),
        },
        "agents.done" => {
            let agent_id = params.get("agentId").and_then(Value::as_u64).unwrap_or_default();
            let status = str_field(&params, "status").unwrap_or_default();
            HostEvent::AgentsDone { agent_id, status }
        }
        _ => return,
    };
    let _ = tx.send(Input::Event(event));
}

// ---------------------------------------------------------------------------
// tool.call
// ---------------------------------------------------------------------------

fn handle_tool_call(params: Value, tx: &Sender<Input>) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("args").cloned().unwrap_or(Value::Null);

    match name {
        "workflow_brief" => {
            let message = match str_field(&args, "message") {
                Some(m) if !m.is_empty() => m,
                _ => return error("workflow_brief requires a non-empty 'message'"),
            };
            let project = opt_str_field(&args, "project");
            send(tx, Command::Brief { project, message });
            output("office is thinking; answer will arrive via chat")
        }
        "workflow_status" => {
            let project = opt_str_field(&args, "project");
            send(tx, Command::Status { project });
            output("queued")
        }
        "workflow_authorize" => {
            let project = match str_field(&args, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("workflow_authorize requires a non-empty 'project'"),
            };
            let delivery_path = match str_field(&args, "delivery_path") {
                Some(p) if !p.is_empty() => p,
                _ => return error("workflow_authorize requires a non-empty 'delivery_path'"),
            };
            send(
                tx,
                Command::Authorize {
                    project,
                    delivery_path,
                },
            );
            output("queued")
        }
        "workflow_interrupt" => {
            let project = match str_field(&args, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("workflow_interrupt requires a non-empty 'project'"),
            };
            let hard = opt_str_field(&args, "mode").as_deref() != Some("soft");
            send(tx, Command::Interrupt { project, hard });
            output("queued")
        }
        "workflow_resume" => {
            let project = match str_field(&args, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("workflow_resume requires a non-empty 'project'"),
            };
            send(tx, Command::Resume { project });
            output("queued")
        }
        "workflow_comment" => {
            let task = match str_field(&args, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("workflow_comment requires a non-empty 'task'"),
            };
            let text = match str_field(&args, "text") {
                Some(t) if !t.is_empty() => t,
                _ => return error("workflow_comment requires a non-empty 'text'"),
            };
            send(tx, Command::Comment { task, text });
            output("queued")
        }
        "workflow_projects" => {
            send(tx, Command::Projects);
            output("queued")
        }
        other => error(&format!("unknown tool: {other}")),
    }
}

// ---------------------------------------------------------------------------
// panel.msg
// ---------------------------------------------------------------------------

fn handle_panel_msg(params: Value, tx: &Sender<Input>) -> Value {
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);
    let op = payload.get("op").and_then(Value::as_str).unwrap_or("");

    // Synchronous reads (PANEL_PROTOCOL.md 1.1): answered INLINE off the driver's
    // snapshot cache, never enqueued and never touching a Koma handle (deadlock rule).
    match op {
        "" => return error("panel.msg requires a payload 'op'"),
        "hello" | "state" => {
            return serde_json::json!({ "ok": true, "snapshot": crate::driver::cache_snapshot() });
        }
        "prd_get" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'prd_get' requires a non-empty 'project'"),
            };
            return serde_json::json!({ "ok": true, "prd": crate::driver::cache_prd(&project) });
        }
        _ => {}
    }

    // Owned copy: some arms below move `payload` (e.g. `edit_task`'s opaque patch), which
    // would otherwise end `op`'s borrow before the not-implemented check after the match.
    let op_owned = op.to_string();

    let command = match op {
        "office_chat" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'office_chat' requires a non-empty 'project'"),
            };
            let message = match str_field(&payload, "message") {
                Some(m) if !m.is_empty() => m,
                _ => return error("op 'office_chat' requires a non-empty 'message'"),
            };
            Command::OfficeChat { project, message }
        }
        "authorize" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'authorize' requires a non-empty 'project'"),
            };
            let delivery_path = match str_field(&payload, "deliveryPath") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'authorize' requires a non-empty 'deliveryPath'"),
            };
            Command::Authorize {
                project,
                delivery_path,
            }
        }
        "interrupt" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'interrupt' requires a non-empty 'project'"),
            };
            let hard = opt_str_field(&payload, "mode").as_deref() != Some("soft");
            Command::Interrupt { project, hard }
        }
        "resume" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'resume' requires a non-empty 'project'"),
            };
            Command::Resume { project }
        }
        "card_move" => {
            let task = match str_field(&payload, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'card_move' requires a non-empty 'task'"),
            };
            let to = match str_field(&payload, "to") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'card_move' requires a non-empty 'to'"),
            };
            let kill_worker = payload
                .get("killWorker")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Command::CardMove {
                task,
                to,
                kill_worker,
            }
        }
        "comment_add" => {
            let task = match str_field(&payload, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'comment_add' requires a non-empty 'task'"),
            };
            let text = match str_field(&payload, "text") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'comment_add' requires a non-empty 'text'"),
            };
            Command::Comment { task, text }
        }
        "unpark" => {
            let task = match str_field(&payload, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'unpark' requires a non-empty 'task'"),
            };
            Command::Unpark { task }
        }
        "edit_task" => {
            let task = match str_field(&payload, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'edit_task' requires a non-empty 'task'"),
            };
            Command::EditTask {
                task,
                patch: payload,
            }
        }
        "edit_deps" => {
            let task = match str_field(&payload, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'edit_deps' requires a non-empty 'task'"),
            };
            Command::EditDeps {
                task,
                patch: payload,
            }
        }
        "config_set" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'config_set' requires a non-empty 'project'"),
            };
            Command::ConfigSet {
                project,
                max_workers: payload.get("maxWorkers").and_then(Value::as_u64).map(|n| n as u32),
                bounce_budget: payload
                    .get("bounceBudget")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32),
                worker_model: opt_str_field(&payload, "workerModel"),
                reviewer_model: opt_str_field(&payload, "reviewerModel"),
                keep_desks: payload.get("keepDesks").and_then(Value::as_bool),
                crd_pass_grade: payload
                    .get("crdPassGrade")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32),
                assumption_check: payload.get("assumptionCheck").and_then(Value::as_bool),
            }
        }
        "project_create" => {
            let name = match str_field(&payload, "name") {
                Some(n) if !n.is_empty() => n,
                _ => return error("op 'project_create' requires a non-empty 'name'"),
            };
            Command::ProjectCreate { name }
        }
        "project_archive" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'project_archive' requires a non-empty 'project'"),
            };
            Command::ProjectArchive { project }
        }
        "task_detail" => {
            let task = match str_field(&payload, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return error("op 'task_detail' requires a non-empty 'task'"),
            };
            Command::TaskDetail { task }
        }
        "breakdown" => {
            let project = match str_field(&payload, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return error("op 'breakdown' requires a non-empty 'project'"),
            };
            Command::Breakdown { project }
        }
        other => return error(&format!("unknown panel op: {other}")),
    };

    // `driver::handle_command` currently no-ops these three ops (board edits are not wired
    // into the kernel yet; `config_set`/`project_archive` ARE wired, see
    // `driver::handle_command`'s `DCmd::ConfigSet`/`DCmd::ProjectArchive` arms). Telling the
    // panel `{ok:true}` for a write that silently vanishes is worse than an honest error:
    // the panel would show no toast and the very next snapshot push would revert the
    // optimistic UI change with no explanation. Surface a real error until a later wave
    // wires these through.
    let unimplemented = matches!(
        command,
        Command::CardMove { .. } | Command::EditTask { .. } | Command::EditDeps { .. }
    );
    send(tx, command);
    if unimplemented {
        return error(&format!("panel op '{op_owned}' is not implemented yet"));
    }
    serde_json::json!({ "ok": true, "accepted": true })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn send(tx: &Sender<Input>, c: Command) {
    // Best-effort: a send failure only happens once the driver thread (the
    // receiver) is gone, which only occurs during shutdown — nothing more to do
    // from a handler that must never block or panic.
    let _ = tx.send(Input::Command(c));
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn opt_str_field(v: &Value, key: &str) -> Option<String> {
    str_field(v, key)
}

fn output(text: &str) -> Value {
    serde_json::json!({ "output": text })
}

fn error(text: &str) -> Value {
    serde_json::json!({ "error": text })
}

#[cfg(test)]
#[path = "handlers_test.rs"]
mod handlers_test;
