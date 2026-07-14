//! Host tests (BUILD_WAVES.md W6): `FakeHost` scripted replies + call recording, and
//! the `is_grant_denied` / `is_timeout` error-classifier helpers.

use super::*;
use serde_json::json;

#[test]
fn fake_host_returns_scripted_reply_and_records_call() {
    let mut host = FakeHost::new();
    host.script("agents.spawn", json!({ "agentId": 7, "status": "spawned" }));

    let reply = host.call("agents.spawn", json!({ "task": "do it" }));

    assert_eq!(reply, json!({ "agentId": 7, "status": "spawned" }));
    assert_eq!(host.calls.len(), 1);
    assert_eq!(host.calls[0].0, "agents.spawn");
    assert_eq!(host.calls[0].1, json!({ "task": "do it" }));
}

#[test]
fn fake_host_replies_are_consumed_fifo_per_method() {
    let mut host = FakeHost::new();
    host.script("agents.status", json!({ "status": "running" }));
    host.script("agents.status", json!({ "status": "done" }));

    assert_eq!(
        host.call("agents.status", json!({})),
        json!({ "status": "running" })
    );
    assert_eq!(
        host.call("agents.status", json!({})),
        json!({ "status": "done" })
    );
}

#[test]
fn fake_host_unscripted_call_returns_loud_error_not_null() {
    let mut host = FakeHost::new();
    let reply = host.call("agents.list", json!({}));
    let err = reply.get("error").and_then(|e| e.as_str()).unwrap();
    assert!(err.contains("no script for 'agents.list'"));
}

#[test]
fn fake_host_records_panel_pushes_and_notifies() {
    let mut host = FakeHost::new();
    host.panel_push("board", json!({ "kind": "snapshot" }));
    host.notify("chat.prompt", json!({ "text": "hi" }));

    assert_eq!(host.panel_pushes, vec![("board".to_string(), json!({ "kind": "snapshot" }))]);
    assert_eq!(host.notifies, vec![("chat.prompt".to_string(), json!({ "text": "hi" }))]);
}

#[test]
fn is_grant_denied_detects_prefix_only() {
    assert!(is_grant_denied(
        &json!({ "error": "grant denied: agents.spawn requires agents:orchestrate" })
    ));
    assert!(!is_grant_denied(&json!({ "error": "unknown agentId: 3" })));
    assert!(!is_grant_denied(&json!({ "ok": true })));
}

#[test]
fn is_timeout_detects_exact_koma_call_timeout_string() {
    assert!(is_timeout(&json!({ "error": "koma call: timed out" })));
    assert!(!is_timeout(&json!({ "error": "session closed" })));
    assert!(!is_timeout(&json!({ "error": "grant denied: models.invoke requires models:invoke" })));
    assert!(!is_timeout(&json!({ "ok": true })));
}
