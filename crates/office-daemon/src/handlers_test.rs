//! Handler tests (BUILD_WAVES.md W6): `tool.call`/`panel.msg` route to the right
//! `Command` and ack without ever touching a host handle (the functions under test
//! take no `Koma`/`Host` argument at all — there is nothing to call), malformed
//! params degrade to an `{"error": ...}` output instead of panicking, and every
//! event name maps to the right `HostEvent`.

use super::*;
use serde_json::json;
use std::sync::mpsc;

fn channel() -> (Sender<Input>, std::sync::mpsc::Receiver<Input>) {
    mpsc::channel()
}

fn recv_command(rx: &std::sync::mpsc::Receiver<Input>) -> Command {
    match rx.try_recv().expect("expected a queued Input") {
        Input::Command(c) => c,
        Input::Event(e) => panic!("expected a Command, got Event {e:?}"),
    }
}

fn recv_event(rx: &std::sync::mpsc::Receiver<Input>) -> HostEvent {
    match rx.try_recv().expect("expected a queued Input") {
        Input::Event(e) => e,
        Input::Command(c) => panic!("expected an Event, got Command {c:?}"),
    }
}

// ---------------------------------------------------------------------------
// tool.call -> Command
// ---------------------------------------------------------------------------

#[test]
fn tool_call_brief_enqueues_command_and_acks_with_output() {
    let (tx, rx) = channel();
    let reply = on_invoke(
        "tool.call",
        json!({ "name": "workflow_brief", "args": { "message": "build a login page" } }),
        &tx,
    );

    assert_eq!(
        recv_command(&rx),
        Command::Brief {
            project: None,
            message: "build a login page".to_string(),
        }
    );
    assert_eq!(
        reply,
        json!({ "output": "office is thinking; answer will arrive via chat" })
    );
}

#[test]
fn tool_call_brief_with_project_carries_project_through() {
    let (tx, rx) = channel();
    on_invoke(
        "tool.call",
        json!({ "name": "workflow_brief", "args": { "message": "hi", "project": "auth" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Brief {
            project: Some("auth".to_string()),
            message: "hi".to_string(),
        }
    );
}

#[test]
fn tool_call_brief_missing_message_is_error_output_not_panic() {
    let (tx, _rx) = channel();
    let reply = on_invoke("tool.call", json!({ "name": "workflow_brief", "args": {} }), &tx);
    assert!(reply.get("error").is_some());
    assert!(reply.get("output").is_none());
}

#[test]
fn tool_call_status_all_projects() {
    let (tx, rx) = channel();
    on_invoke("tool.call", json!({ "name": "workflow_status", "args": {} }), &tx);
    assert_eq!(recv_command(&rx), Command::Status { project: None });
}

#[test]
fn tool_call_authorize_requires_project_and_delivery_path() {
    let (tx, rx) = channel();
    let reply = on_invoke(
        "tool.call",
        json!({
            "name": "workflow_authorize",
            "args": { "project": "auth", "delivery_path": "/ws/deliver" }
        }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Authorize {
            project: "auth".to_string(),
            delivery_path: "/ws/deliver".to_string(),
        }
    );
    assert_eq!(reply, json!({ "output": "queued" }));

    let (tx2, _rx2) = channel();
    let missing_path = on_invoke(
        "tool.call",
        json!({ "name": "workflow_authorize", "args": { "project": "auth" } }),
        &tx2,
    );
    assert!(missing_path.get("error").is_some());
}

#[test]
fn tool_call_interrupt_defaults_hard_and_soft_mode_is_soft() {
    let (tx, rx) = channel();
    on_invoke(
        "tool.call",
        json!({ "name": "workflow_interrupt", "args": { "project": "auth" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Interrupt {
            project: "auth".to_string(),
            hard: true,
        }
    );

    let (tx2, rx2) = channel();
    on_invoke(
        "tool.call",
        json!({ "name": "workflow_interrupt", "args": { "project": "auth", "mode": "soft" } }),
        &tx2,
    );
    assert_eq!(
        recv_command(&rx2),
        Command::Interrupt {
            project: "auth".to_string(),
            hard: false,
        }
    );
}

#[test]
fn tool_call_resume_and_projects_and_comment() {
    let (tx, rx) = channel();
    on_invoke("tool.call", json!({ "name": "workflow_resume", "args": { "project": "auth" } }), &tx);
    assert_eq!(recv_command(&rx), Command::Resume { project: "auth".to_string() });

    let (tx, rx) = channel();
    on_invoke("tool.call", json!({ "name": "workflow_projects", "args": {} }), &tx);
    assert_eq!(recv_command(&rx), Command::Projects);

    let (tx, rx) = channel();
    let reply = on_invoke(
        "tool.call",
        json!({ "name": "workflow_comment", "args": { "task": "t1", "text": "please fix" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Comment {
            task: "t1".to_string(),
            text: "please fix".to_string(),
        }
    );
    assert_eq!(reply, json!({ "output": "queued" }));
}

#[test]
fn tool_call_unknown_tool_name_is_error_output() {
    let (tx, _rx) = channel();
    let reply = on_invoke("tool.call", json!({ "name": "not_a_tool", "args": {} }), &tx);
    assert_eq!(reply, json!({ "error": "unknown tool: not_a_tool" }));
}

#[test]
fn tool_call_missing_name_is_error_output_not_panic() {
    let (tx, _rx) = channel();
    let reply = on_invoke("tool.call", json!({ "args": {} }), &tx);
    assert!(reply.get("error").is_some());
}

#[test]
fn on_invoke_unknown_method_is_error_output_not_panic() {
    let (tx, _rx) = channel();
    let reply = on_invoke("bogus.method", json!({}), &tx);
    assert_eq!(reply, json!({ "error": "unknown method: bogus.method" }));
}

#[test]
fn on_invoke_malformed_json_shapes_never_panic() {
    let (tx, _rx) = channel();
    // Completely wrong top-level shapes for a tool.call invoke.
    let _ = on_invoke("tool.call", Value::Null, &tx);
    let _ = on_invoke("tool.call", json!([]), &tx);
    let _ = on_invoke("tool.call", json!("just a string"), &tx);
    let _ = on_invoke("tool.call", json!({ "name": 42 }), &tx);
}

// ---------------------------------------------------------------------------
// panel.msg -> Command
// ---------------------------------------------------------------------------

#[test]
fn panel_msg_hello_and_state() {
    let (tx, rx) = channel();
    let reply = on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "hello", "uiVersion": "1.0.0" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::PanelHello {
            ui_version: Some("1.0.0".to_string()),
        }
    );
    assert_eq!(reply, json!({ "ok": true, "accepted": true }));

    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "state", "project": "auth" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::PanelState {
            project: Some("auth".to_string()),
        }
    );
}

#[test]
fn panel_msg_authorize_and_interrupt_and_resume() {
    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({
            "panelId": "board",
            "payload": { "op": "authorize", "project": "auth", "deliveryPath": "/ws/deliver" }
        }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Authorize {
            project: "auth".to_string(),
            delivery_path: "/ws/deliver".to_string(),
        }
    );

    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "interrupt", "project": "auth", "mode": "soft" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Interrupt {
            project: "auth".to_string(),
            hard: false,
        }
    );

    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "resume", "project": "auth" } }),
        &tx,
    );
    assert_eq!(recv_command(&rx), Command::Resume { project: "auth".to_string() });
}

#[test]
fn panel_msg_card_move_comment_add_and_unpark() {
    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({
            "panelId": "board",
            "payload": { "op": "card_move", "task": "t1", "to": "todo", "killWorker": true }
        }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::CardMove {
            task: "t1".to_string(),
            to: "todo".to_string(),
            kill_worker: true,
        }
    );

    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "comment_add", "task": "t1", "text": "lgtm" } }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::Comment {
            task: "t1".to_string(),
            text: "lgtm".to_string(),
        }
    );

    let (tx, rx) = channel();
    on_invoke("panel.msg", json!({ "panelId": "board", "payload": { "op": "unpark", "task": "t1" } }), &tx);
    assert_eq!(recv_command(&rx), Command::Unpark { task: "t1".to_string() });
}

#[test]
fn panel_msg_config_set_project_create_archive_prd_get_task_detail() {
    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({
            "panelId": "board",
            "payload": {
                "op": "config_set",
                "project": "auth",
                "maxWorkers": 3,
                "bounceBudget": 2,
                "workerModel": "gpt-5",
                "reviewerModel": "opus"
            }
        }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::ConfigSet {
            project: "auth".to_string(),
            max_workers: Some(3),
            bounce_budget: Some(2),
            worker_model: Some("gpt-5".to_string()),
            reviewer_model: Some("opus".to_string()),
        }
    );

    let (tx, rx) = channel();
    on_invoke("panel.msg", json!({ "panelId": "board", "payload": { "op": "project_create", "name": "New" } }), &tx);
    assert_eq!(recv_command(&rx), Command::ProjectCreate { name: "New".to_string() });

    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "project_archive", "project": "auth" } }),
        &tx,
    );
    assert_eq!(recv_command(&rx), Command::ProjectArchive { project: "auth".to_string() });

    let (tx, rx) = channel();
    on_invoke("panel.msg", json!({ "panelId": "board", "payload": { "op": "prd_get", "project": "auth" } }), &tx);
    assert_eq!(recv_command(&rx), Command::PrdGet { project: "auth".to_string() });

    let (tx, rx) = channel();
    on_invoke("panel.msg", json!({ "panelId": "board", "payload": { "op": "task_detail", "task": "t1" } }), &tx);
    assert_eq!(recv_command(&rx), Command::TaskDetail { task: "t1".to_string() });
}

#[test]
fn panel_msg_edit_task_and_edit_deps_carry_opaque_patch() {
    let (tx, rx) = channel();
    let payload = json!({ "op": "edit_task", "task": "t1", "priority": 5 });
    on_invoke("panel.msg", json!({ "panelId": "board", "payload": payload.clone() }), &tx);
    assert_eq!(
        recv_command(&rx),
        Command::EditTask {
            task: "t1".to_string(),
            patch: payload,
        }
    );
}

#[test]
fn panel_msg_missing_op_and_unknown_op_are_errors_not_panics() {
    let (tx, _rx) = channel();
    let reply = on_invoke("panel.msg", json!({ "panelId": "board", "payload": {} }), &tx);
    assert!(reply.get("error").is_some());

    let (tx, _rx) = channel();
    let reply = on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "not_a_real_op" } }),
        &tx,
    );
    assert_eq!(reply, json!({ "error": "unknown panel op: not_a_real_op" }));
}

#[test]
fn panel_msg_office_chat_requires_project_and_message() {
    let (tx, rx) = channel();
    on_invoke(
        "panel.msg",
        json!({
            "panelId": "board",
            "payload": { "op": "office_chat", "project": "auth", "message": "status?" }
        }),
        &tx,
    );
    assert_eq!(
        recv_command(&rx),
        Command::OfficeChat {
            project: "auth".to_string(),
            message: "status?".to_string(),
        }
    );

    let (tx, _rx) = channel();
    let reply = on_invoke(
        "panel.msg",
        json!({ "panelId": "board", "payload": { "op": "office_chat", "project": "auth" } }),
        &tx,
    );
    assert!(reply.get("error").is_some());
}

// ---------------------------------------------------------------------------
// on_event -> HostEvent
// ---------------------------------------------------------------------------

#[test]
fn event_subagent_done_maps_fields() {
    let (tx, rx) = channel();
    on_event(
        "subagent.done",
        json!({ "session": "sess-1", "subagentId": 3, "agent": "general", "status": "done" }),
        &tx,
    );
    assert_eq!(
        recv_event(&rx),
        HostEvent::SubagentDone {
            session: "sess-1".to_string(),
            subagent_id: 3,
            agent: "general".to_string(),
            status: "done".to_string(),
        }
    );
}

#[test]
fn event_agent_turn_end_and_foreground_change() {
    let (tx, rx) = channel();
    on_event("agent.turn_end", json!({ "session": "sess-1" }), &tx);
    assert_eq!(recv_event(&rx), HostEvent::AgentTurnEnd { session: "sess-1".to_string() });

    let (tx, rx) = channel();
    on_event("session.foreground_change", json!({ "session": "sess-2" }), &tx);
    assert_eq!(
        recv_event(&rx),
        HostEvent::SessionForegroundChange {
            session: "sess-2".to_string(),
        }
    );
}

#[test]
fn event_agents_done_private_notify_maps_to_agents_done() {
    let (tx, rx) = channel();
    on_event("agents.done", json!({ "agentId": 42, "status": "done" }), &tx);
    assert_eq!(recv_event(&rx), HostEvent::AgentsDone { agent_id: 42, status: "done".to_string() });
}

#[test]
fn event_unknown_name_is_dropped_silently_not_panic() {
    let (tx, rx) = channel();
    on_event("some.unrelated.event", json!({ "whatever": true }), &tx);
    assert!(rx.try_recv().is_err(), "unknown event names must not be queued");
}

#[test]
fn event_malformed_params_never_panic() {
    let (tx, _rx) = channel();
    on_event("subagent.done", Value::Null, &tx);
    on_event("agents.done", json!("not an object"), &tx);
    on_event("agent.turn_end", json!([1, 2, 3]), &tx);
}
